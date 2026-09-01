// SPDX-License-Identifier: AGPL-3.0-or-later

//! Consonance whole-VM backend for the Nova campaign adapter.
//!
//! Dissonance retains portable, game-owned input prefixes. Each evaluator
//! thread lazily owns one Consonance VM session and maps those prefixes to real
//! whole-VM snapshots. A mutation therefore restores a Consonance snapshot,
//! supplies one opaque controller chord through the guest SDK, and decodes only
//! the guest-published progress markers. The generic search coordinator never
//! learns Nova rules or memory addresses.

use std::{cell::RefCell, collections::BTreeMap, error::Error, path::Path, sync::Arc};

use control_proto::{
    Moment, Reply, Reproducer, Request, SnapId, StopConditions, StopMask, StopReason,
};
use environment::{EnvSpec, FaultPolicy};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use vmm_backend::{Backend, X86};
use vmm_core::{
    control::{ControlServer, VmmFactory, server_caps},
    vendor::x86::bringup::boot_linux_stock_virtual_time,
};

use crate::{
    nova::target::{
        ButtonChord, MAX_HOLD_FRAMES, NovaMechanicalState, NovaObservations, WRAM_SIZE,
        decode_consonance_state,
    },
    target::ExitKind,
};

type Server = ControlServer<Box<dyn Backend<A = X86>>>;

const RAM: usize = 512 * 1024 * 1024;
const DEADLINE: u64 = 2_000_000_000;
const SEED: u64 = 0x4e4f_5641_5f53_4541;
const CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t tsc=reliable \
    no_timer_check lpj=4000000 random.trust_cpu=off nokaslr nosmp maxcpus=1 \
    nox2apic hpet=disable harmony_pvclock rdinit=/init";

const SDK_NS_SHIFT: u32 = 24;
const SDK_NS_STATE: u8 = 2;
const SDK_STATE_SET: u8 = 0;
const SDK_STATE_MAX: u8 = 1;
const REG_STARTED_LEVEL: u32 = 1;
const REG_LEVEL: u32 = 2;
const REG_HEALTH: u32 = 5;
const REG_ABILITY: u32 = 6;
const REG_CLEARED: u32 = 7;
const REG_AVAILABLE: u32 = 8;
const REG_COLLECTIBLES: u32 = 9;
const REG_FRAME: u32 = 10;
const REG_BILLBOARD_GPA: u32 = 11;
const REG_BILLBOARD_LEN: u32 = 12;

const BILLBOARD_HEADER_LEN: usize = 32;
const BILLBOARD_MAGIC: &[u8; 4] = b"HBBD";
const BILLBOARD_VERSION: u16 = 1;

/// Portable campaign snapshot for a Consonance-backed Nova endpoint.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConsonanceNovaSnapshot {
    actions: Vec<ButtonChord>,
    observation: NovaObservations,
    work_ram: Vec<u8>,
    failed: bool,
}

#[derive(Debug)]
struct Config {
    key: [u8; 32],
    kernel: Vec<u8>,
    initramfs: Vec<u8>,
}

struct Session {
    key: [u8; 32],
    server: Server,
    setup: SnapId,
    snapshots: BTreeMap<Vec<ButtonChord>, SnapId>,
    billboard_gpa: u64,
    billboard_len: u32,
}

thread_local! {
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
}

/// One game-aware target handle; the live KVM session stays thread-local.
#[derive(Debug)]
pub struct ConsonanceNovaTarget {
    config: Arc<Config>,
    actions: Vec<ButtonChord>,
    observation: NovaObservations,
    action_observations: Vec<NovaObservations>,
    work_ram: Vec<u8>,
    failed: bool,
    frames_clocked: u64,
    genesis_cleared: u8,
}

impl ConsonanceNovaTarget {
    /// Boot or join the current evaluator thread's Consonance Nova session.
    pub fn new(kernel: &[u8], initramfs: &[u8]) -> Result<Self, String> {
        let mut digest = Sha256::new();
        digest.update(kernel);
        digest.update(initramfs);
        let key: [u8; 32] = digest.finalize().into();
        let config = Arc::new(Config {
            key,
            kernel: kernel.to_vec(),
            initramfs: initramfs.to_vec(),
        });
        let (frame, state, work_ram) = with_session(&config, |session| session.observe())?;
        let observation = make_observation(frame, state, &work_ram, &[]);
        Ok(Self {
            config,
            actions: Vec::new(),
            action_observations: vec![observation.clone()],
            observation,
            work_ram,
            failed: false,
            frames_clocked: 0,
            genesis_cleared: state.cleared_count(),
        })
    }

    /// Current decoded, source-derived game state.
    #[must_use]
    pub fn mechanical_state(&self) -> NovaMechanicalState {
        self.observation.decoded
    }

    /// Whether health reached zero.
    #[must_use]
    pub fn is_dead(&self) -> bool {
        self.observation.decoded.health == 0
    }

    /// Whether this branch durably cleared a level beyond genesis.
    #[must_use]
    pub fn cleared_a_level(&self) -> bool {
        self.observation.decoded.cleared_count() > self.genesis_cleared
    }

    /// Total emulated frames evaluated by this target handle.
    #[must_use]
    pub fn frames_clocked(&self) -> u64 {
        self.frames_clocked
    }

    /// Endpoint observations emitted by the most recent opaque chord.
    #[must_use]
    pub fn last_action_observations(&self) -> &[NovaObservations] {
        &self.action_observations
    }

    /// Restore one portable prefix through this thread's Consonance snapshot cache.
    pub fn restore(&mut self, snapshot: &ConsonanceNovaSnapshot) -> Result<(), Box<dyn Error>> {
        with_session(&self.config, |session| {
            let snap = session.ensure_prefix(&snapshot.actions)?;
            expect_unit(session.drive(&Request::Replay(snap))?, "replay")
        })?;
        self.actions = snapshot.actions.clone();
        self.observation = snapshot.observation.clone();
        self.action_observations = vec![self.observation.clone()];
        self.work_ram = snapshot.work_ram.clone();
        self.failed = snapshot.failed;
        Ok(())
    }

    /// Capture a portable reference to the real whole-VM endpoint snapshot.
    #[must_use]
    pub fn snapshot(&self) -> Option<ConsonanceNovaSnapshot> {
        (!self.failed).then(|| ConsonanceNovaSnapshot {
            actions: self.actions.clone(),
            observation: self.observation.clone(),
            work_ram: self.work_ram.clone(),
            failed: false,
        })
    }

    /// Test a fixed neutral/button continuation, then return to the caller's prefix.
    pub fn survives_probe(&mut self, buttons: u8, frames: u16) -> bool {
        if self.failed || self.is_dead() || self.cleared_a_level() {
            return false;
        }
        let saved = self.snapshot();
        let Some(saved) = saved else {
            return false;
        };
        let mut remaining = frames;
        while remaining > 0 && !self.is_dead() && !self.failed {
            let hold = remaining.min(u16::from(MAX_HOLD_FRAMES));
            let Ok(hold) = u8::try_from(hold) else {
                self.failed = true;
                break;
            };
            self.apply(ButtonChord::new(buttons, hold));
            remaining -= u16::from(hold);
        }
        let survived = !self.failed && !self.is_dead();
        if self.restore(&saved).is_err() {
            self.failed = true;
            false
        } else {
            survived
        }
    }

    /// Restore the sealed gameplay genesis.
    pub fn reset(&mut self) {
        let result = with_session(&self.config, |session| {
            expect_unit(
                session.drive(&Request::Replay(session.setup))?,
                "genesis replay",
            )?;
            session.observe()
        });
        match result {
            Ok((frame, state, work_ram)) => {
                self.actions.clear();
                self.observation = make_observation(frame, state, &work_ram, &[]);
                self.action_observations = vec![self.observation.clone()];
                self.work_ram = work_ram;
                self.failed = false;
            }
            Err(_) => self.failed = true,
        }
    }

    /// Apply one opaque controller chord through the SDK payload service.
    pub fn apply(&mut self, action: ButtonChord) {
        self.action_observations.clear();
        if self.failed || self.is_dead() || self.cleared_a_level() {
            return;
        }
        let prior = self.work_ram.clone();
        let result = with_session(&self.config, |session| {
            session.advance(&self.actions, action)?;
            session.observe()
        });
        match result {
            Ok((frame, state, work_ram)) => {
                self.actions.push(action);
                let observation = make_observation(frame, state, &work_ram, &prior);
                self.frames_clocked = self
                    .frames_clocked
                    .saturating_add(u64::from(action.bounded_hold_frames()));
                self.work_ram = work_ram;
                self.observation = observation.clone();
                self.action_observations.push(observation);
            }
            Err(_) => self.failed = true,
        }
    }

    /// Game-neutral target exit classification.
    #[must_use]
    pub fn exit_kind(&self) -> ExitKind {
        if self.failed {
            ExitKind::Crash
        } else {
            ExitKind::Ok
        }
    }
}

fn with_session<T>(
    config: &Arc<Config>,
    operation: impl FnOnce(&mut Session) -> Result<T, String>,
) -> Result<T, String> {
    SESSION.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot
            .as_ref()
            .is_none_or(|session| session.key != config.key)
        {
            *slot = Some(Session::boot(config)?);
        }
        operation(
            slot.as_mut()
                .ok_or("Consonance session was not initialized")?,
        )
    })
}

impl Session {
    fn boot(config: &Config) -> Result<Self, String> {
        let boot = |kernel: &[u8], initramfs: &[u8]| {
            let mut vmm = boot_linux_stock_virtual_time(kernel, initramfs, RAM, CMDLINE, SEED)?;
            vmm.wire_snapshot_hashing();
            Ok(vmm)
        };
        let live = boot(&config.kernel, &config.initramfs)
            .map_err(|error| format!("Consonance Nova boot compose failed: {error:?}"))?;
        let factory_kernel = config.kernel.clone();
        let factory_initramfs = config.initramfs.clone();
        let factory: VmmFactory<Box<dyn Backend<A = X86>>> =
            Box::new(move || boot(&factory_kernel, &factory_initramfs));
        let mut server = ControlServer::new(live, factory);
        match drive(&mut server, &Request::Hello(server_caps()))? {
            Reply::Hello(caps) if caps == server_caps() => {}
            other => return Err(format!("Consonance hello returned {other:?}")),
        }
        let genesis = snapshot(&mut server)?;
        expect_unit(
            drive(
                &mut server,
                &Request::Branch {
                    snap: genesis,
                    env: payload_env(vec![vec![0, 1]; 16]),
                },
            )?,
            "bootstrap branch",
        )?;
        run_to_snapshot(&mut server)?;
        let setup = snapshot(&mut server)?;
        let registers = state_registers(&mut server)?;
        let billboard_gpa = register(&registers, REG_BILLBOARD_GPA)?;
        let billboard_len = u32::try_from(register(&registers, REG_BILLBOARD_LEN)?)
            .map_err(|_| "Nova billboard length does not fit u32".to_owned())?;
        let mut snapshots = BTreeMap::new();
        snapshots.insert(Vec::new(), setup);
        Ok(Self {
            key: config.key,
            server,
            setup,
            snapshots,
            billboard_gpa,
            billboard_len,
        })
    }

    fn drive(&mut self, request: &Request) -> Result<Reply, String> {
        drive(&mut self.server, request)
    }

    fn ensure_prefix(&mut self, actions: &[ButtonChord]) -> Result<SnapId, String> {
        if let Some(snap) = self.snapshots.get(actions).copied() {
            return Ok(snap);
        }
        let start_len = (0..actions.len())
            .rev()
            .find(|length| self.snapshots.contains_key(&actions[..*length]))
            .unwrap_or(0);
        let parent = self
            .snapshots
            .get(&actions[..start_len])
            .copied()
            .ok_or("Consonance setup snapshot is missing")?;
        let mut payloads = actions[start_len..]
            .iter()
            .map(|action| vec![action.buttons, action.bounded_hold_frames()])
            .collect::<Vec<_>>();
        payloads.push(vec![0, 1]);
        expect_unit(
            self.drive(&Request::Branch {
                snap: parent,
                env: payload_env(payloads),
            })?,
            "prefix branch",
        )?;
        let mut last = parent;
        for length in start_len + 1..=actions.len() {
            run_to_snapshot(&mut self.server)?;
            last = snapshot(&mut self.server)?;
            self.snapshots.insert(actions[..length].to_vec(), last);
        }
        Ok(last)
    }

    fn advance(&mut self, prefix: &[ButtonChord], action: ButtonChord) -> Result<SnapId, String> {
        let mut next = prefix.to_vec();
        next.push(action);
        if let Some(snap) = self.snapshots.get(&next).copied() {
            expect_unit(self.drive(&Request::Replay(snap))?, "cached replay")?;
            return Ok(snap);
        }
        let parent = self.ensure_prefix(prefix)?;
        expect_unit(
            self.drive(&Request::Branch {
                snap: parent,
                env: payload_env(vec![
                    vec![action.buttons, action.bounded_hold_frames()],
                    vec![0, 1],
                ]),
            })?,
            "action branch",
        )?;
        run_to_snapshot(&mut self.server)?;
        let snap = snapshot(&mut self.server)?;
        self.snapshots.insert(next, snap);
        Ok(snap)
    }

    fn observe(&mut self) -> Result<(u64, NovaMechanicalState, Vec<u8>), String> {
        let registers = state_registers(&mut self.server)?;
        let header = read_exact(&mut self.server, self.billboard_gpa, BILLBOARD_HEADER_LEN)?;
        if header.get(0..4) != Some(BILLBOARD_MAGIC.as_slice()) {
            return Err("Nova billboard magic is absent".to_owned());
        }
        let version = read_u16(&header, 4)?;
        if version != BILLBOARD_VERSION {
            return Err(format!("unsupported Nova billboard version {version}"));
        }
        let work_offset = u64::from(read_u32(&header, 24)?);
        let work_len = read_u32(&header, 28)?;
        if work_len != u32::try_from(WRAM_SIZE).unwrap_or(u32::MAX)
            || work_offset.saturating_add(u64::from(work_len)) > u64::from(self.billboard_len)
        {
            return Err("Nova billboard work-RAM region is malformed".to_owned());
        }
        let work_ram = read_exact(
            &mut self.server,
            self.billboard_gpa.saturating_add(work_offset),
            WRAM_SIZE,
        )?;
        let state = decode_consonance_state(
            &work_ram,
            u8_register(&registers, REG_ABILITY)?,
            u8_register(&registers, REG_CLEARED)?,
            u8_register(&registers, REG_AVAILABLE)?,
            u8_register(&registers, REG_COLLECTIBLES)?,
        )
        .map_err(|error| error.to_string())?;
        if state.started_level != u8_register(&registers, REG_STARTED_LEVEL)?
            || state.level != u8_register(&registers, REG_LEVEL)?
            || state.health != u8_register(&registers, REG_HEALTH)?
        {
            return Err("Nova SDK registers disagree with the billboard".to_owned());
        }
        Ok((register(&registers, REG_FRAME)?, state, work_ram))
    }
}

fn make_observation(
    frame_count: u64,
    state: NovaMechanicalState,
    work_ram: &[u8],
    prior: &[u8],
) -> NovaObservations {
    let changed_indices = work_ram
        .iter()
        .zip(prior)
        .enumerate()
        .filter_map(|(index, (current, previous))| {
            (current != previous)
                .then(|| u16::try_from(index).ok())
                .flatten()
        })
        .collect::<Vec<_>>();
    NovaObservations {
        frame_count,
        decoded: state,
        changed_indices: changed_indices.clone(),
        dead: state.health == 0,
        log_line: format!("frame={frame_count} changed={changed_indices:?}"),
    }
}

fn payload_env(payloads: Vec<Vec<u8>>) -> Reproducer {
    let mut spec = EnvSpec::Seeded {
        seed: SEED,
        policy: FaultPolicy::none(),
    };
    spec.set_payloads(Some(payloads));
    Reproducer {
        blob_version: EnvSpec::BLOB_VERSION,
        bytes: spec.encode(),
    }
}

fn drive(server: &mut Server, request: &Request) -> Result<Reply, String> {
    match server.handle(request) {
        Ok(Ok(reply)) => Ok(reply),
        Ok(Err(error)) => Err(format!("{request:?} returned {error:?}")),
        Err(error) => Err(format!("{request:?} ended the session: {error:?}")),
    }
}

fn expect_unit(reply: Reply, operation: &str) -> Result<(), String> {
    match reply {
        Reply::Unit => Ok(()),
        other => Err(format!("{operation} returned {other:?}")),
    }
}

fn run_to_snapshot(server: &mut Server) -> Result<Moment, String> {
    match drive(
        server,
        &Request::Run {
            until: StopConditions {
                deadline: Some(Moment(DEADLINE)),
                on: StopMask::NONE.arm(control_proto::class_bit::SNAPSHOT_POINT),
            },
            resolve: None,
        },
    )? {
        Reply::Stop(StopReason::SnapshotPoint { vtime }) => Ok(vtime),
        other => Err(format!("expected Nova snapshot point, received {other:?}")),
    }
}

fn snapshot(server: &mut Server) -> Result<SnapId, String> {
    match drive(server, &Request::Snapshot)? {
        Reply::Snapshot {
            id, tainted: false, ..
        } => Ok(id),
        Reply::Snapshot { tainted: true, .. } => Err("Nova timeline is tainted".to_owned()),
        other => Err(format!("snapshot returned {other:?}")),
    }
}

fn state_registers(server: &mut Server) -> Result<BTreeMap<u32, u64>, String> {
    let mut registers = BTreeMap::<u32, u64>::new();
    let mut offset = 0_u32;
    loop {
        let events = match drive(server, &Request::SdkEvents { offset })? {
            Reply::SdkEvents(events) => events,
            other => return Err(format!("SDK event fetch returned {other:?}")),
        };
        if events.is_empty() {
            break;
        }
        offset = offset
            .checked_add(u32::try_from(events.len()).map_err(|_| "SDK event page is too large")?)
            .ok_or("SDK event offset overflow")?;
        for (_, event_id, bytes) in events {
            let namespace = (event_id >> SDK_NS_SHIFT) as u8;
            let register_id = event_id & ((1 << SDK_NS_SHIFT) - 1);
            if namespace != SDK_NS_STATE || bytes.len() != 9 {
                continue;
            }
            let value = u64::from_le_bytes(
                bytes[1..9]
                    .try_into()
                    .map_err(|_| "SDK state payload is truncated")?,
            );
            match bytes[0] {
                SDK_STATE_SET => {
                    registers.insert(register_id, value);
                }
                SDK_STATE_MAX => {
                    registers
                        .entry(register_id)
                        .and_modify(|current| *current = (*current).max(value))
                        .or_insert(value);
                }
                _ => return Err("SDK state event has an unknown operation".to_owned()),
            }
        }
    }
    Ok(registers)
}

fn register(registers: &BTreeMap<u32, u64>, id: u32) -> Result<u64, String> {
    registers
        .get(&id)
        .copied()
        .ok_or_else(|| format!("Nova SDK register {id} is absent"))
}

fn u8_register(registers: &BTreeMap<u32, u64>, id: u32) -> Result<u8, String> {
    u8::try_from(register(registers, id)?)
        .map_err(|_| format!("Nova SDK register {id} does not fit u8"))
}

fn read_exact(server: &mut Server, gpa: u64, len: usize) -> Result<Vec<u8>, String> {
    let len = u32::try_from(len).map_err(|_| "Nova observation read is too large")?;
    match drive(server, &Request::Read { gpa, len })? {
        Reply::Bytes(bytes) if bytes.len() == len as usize => Ok(bytes),
        Reply::Bytes(_) => Err("Nova observation read was truncated".to_owned()),
        other => Err(format!("Nova observation read returned {other:?}")),
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, String> {
    let end = offset.saturating_add(2);
    let field = bytes
        .get(offset..end)
        .ok_or("Nova billboard u16 field is truncated")?;
    Ok(u16::from_le_bytes(
        field
            .try_into()
            .map_err(|_| "Nova billboard u16 field is malformed")?,
    ))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let end = offset.saturating_add(4);
    let field = bytes
        .get(offset..end)
        .ok_or("Nova billboard u32 field is truncated")?;
    Ok(u32::from_le_bytes(
        field
            .try_into()
            .map_err(|_| "Nova billboard u32 field is malformed")?,
    ))
}

/// Stable identity string for the Consonance-backed Nova campaign stream.
#[must_use]
pub fn identity(kernel: &[u8], initramfs: &[u8]) -> String {
    format!(
        "consonance-whole-vm-v1;kernel-sha256={:x};initramfs-sha256={:x};sdk-input=payload-v1;snapshot=portable-prefix-to-vm-snapshot-v1",
        Sha256::digest(kernel),
        Sha256::digest(initramfs),
    )
}

/// Read a Consonance Nova target from the two guest image paths.
pub fn from_paths(kernel: &Path, initramfs: &Path) -> Result<ConsonanceNovaTarget, String> {
    let kernel = std::fs::read(kernel).map_err(|error| format!("read kernel: {error}"))?;
    let initramfs = std::fs::read(initramfs).map_err(|error| format!("read initramfs: {error}"))?;
    ConsonanceNovaTarget::new(&kernel, &initramfs)
}
