// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`MockServer`] — the in-crate, in-process control-transport server the whole
//! laptop gate runs against (the task-58 loopback pattern, owned here).
//!
//! It is not a VM: it is a **scripted, fully deterministic guest**. Every
//! observable (registers, memory bytes, the whole-state hash) is a pure function
//! of the *world seed* — a digest of the active [`EnvSpec`] — and the current
//! [`Moment`], so the moment address behaves exactly as the real substrate
//! promises:
//!
//! - **Determinism.** `branch(genesis, env)` + `run(until = m)` twice yields
//!   bit-identical `regs`/`read`/`hash` — the same `(world_seed, moment)` maps to
//!   the same state.
//! - **Observation invariance** (task 80). `read`/`regs` are pure reads: they
//!   touch neither the moment, the taint bit, nor the hash.
//! - **Counterfactual divergence** (task 82). Editing one override changes the
//!   `EnvSpec` bytes, hence the world seed, hence every hash — so a
//!   [`vary`](crate::MomentRef::vary)'d run visibly diverges.
//! - **Improvisation + taint** (task 81). `exec` sets the timeline's taint bit,
//!   which folds into the whole-state hash (so an exec'd fork's hash differs
//!   while a re-materialized original's does not) and trips the
//!   [`recorded_env`](Server::recorded_env) guard.
//! - **A fault that bites** (scripted). An override that stages a
//!   [`CorruptMemory`](environment::HostFault::CorruptMemory) at a reachable
//!   `Moment` crashes the guest there — a real [`StopReason::Crash`], so the
//!   counterfactual can *change behaviour*, not just the hash.
//!
//! The real box connection (post-80/81) is a second [`Server`] implementor the
//! foreman wires up; nothing in the client's observable behaviour depends on
//! this being a mock rather than a socket.

use std::collections::BTreeMap;

use control_proto::{
    CapFlags, Caps, CoverageGeometry, CrashInfo, CrashKind, Environment, HashScope, SnapId,
    StopConditions, StopReason, VTime,
};
use environment::{Action, EnvSpec, HostFault, Moment};

use crate::server::{ExecResult, RegsView, Server, Snapshot};
use crate::{DEFAULT_RAM_BYTES, READ_CAP, SessionError};

/// A scripted timeline: a world defined by an [`EnvSpec`], a position on the
/// deterministic axis, and the task-81 taint bit.
#[derive(Clone, Debug)]
struct Timeline {
    /// The digest of the active env — the whole world in one `u64`.
    world_seed: u64,
    /// The env this timeline runs under (inspected for scripted faults + minted
    /// by [`recorded_env`](Server::recorded_env)).
    env: EnvSpec,
    /// The current position.
    moment: Moment,
    /// Whether an [`exec`](Server::exec) improvisation has tainted it.
    tainted: bool,
}

impl Timeline {
    /// Boot a timeline from an env at `moment`, inheriting `tainted` from the
    /// restored lineage.
    fn from_env(env: EnvSpec, moment: Moment, tainted: bool) -> Self {
        Self {
            world_seed: world_seed(&env),
            env,
            moment,
            tainted,
        }
    }

    /// The whole-state hash at the current point: a pure function of
    /// `(world_seed, moment, tainted)`. `read`/`regs` never change any of the
    /// three, so they are hash-invariant; `exec` flips `tainted`, so it is not.
    fn hash(&self, scope: HashScope) -> [u8; 32] {
        let (tag, a, b) = match scope {
            HashScope::Whole => (0u64, 0u64, 0u64),
            HashScope::Disk => (1, 0, 0),
            HashScope::Region { base, len } => (2, base, len),
        };
        digest32(&[
            self.world_seed,
            self.moment,
            u64::from(self.tainted),
            tag,
            a,
            b,
        ])
    }

    /// The register view at the current point — a pure function of
    /// `(world_seed, moment)`.
    fn regs(&self) -> RegsView {
        let field = |i: u64| splitmix64(self.world_seed ^ splitmix64(self.moment ^ (i << 1)));
        let mut gpr = [0u64; 16];
        for (i, slot) in gpr.iter_mut().enumerate() {
            *slot = field(0x100 + i as u64);
        }
        let mut seg = [0u16; 6];
        for (i, slot) in seg.iter_mut().enumerate() {
            *slot = field(0x200 + i as u64) as u16;
        }
        RegsView {
            version: RegsView::VERSION,
            gpr,
            rip: field(1),
            rflags: field(2) & 0x0000_0000_00FF_FFFF,
            seg,
            cr0: field(3),
            cr3: field(4) & !0xFFF, // page-aligned, like a real CR3
            cr4: field(5),
            moment: self.moment,
            vtime: self.moment,
        }
    }

    /// The guest-physical byte at `addr` — a pure function of
    /// `(world_seed, moment, addr)`.
    fn mem_byte(&self, addr: u64) -> u8 {
        splitmix64(
            self.world_seed ^ splitmix64(self.moment) ^ splitmix64(addr.wrapping_mul(0x9E37)),
        ) as u8
    }
}

/// The in-crate scripted control-transport server. Construct with
/// [`boot`](MockServer::boot), then drive it through a [`Session`](crate::Session).
#[derive(Clone, Debug)]
pub struct MockServer {
    /// Guest RAM size, the ceiling for `read` range checks.
    ram_bytes: u64,
    /// Whether `hello` has negotiated the session (a verb before it is
    /// `Unsupported`, mirroring the real server).
    negotiated: bool,
    next_snap: u64,
    /// Captured snapshots — the **whole** [`Timeline`] verbatim (world_seed +
    /// env + moment + taint), so `replay` restores the exact world and `branch`
    /// reseeds from the captured position + lineage taint. Capturing only
    /// `{moment, tainted}` would let a `replay` after a `branch` restore the old
    /// position inside the *new* world (the `read`/`hash` observables are
    /// functions of `world_seed`), silently violating `replay`'s verbatim
    /// contract.
    snaps: BTreeMap<u64, Timeline>,
    /// The live timeline.
    cur: Timeline,
}

impl MockServer {
    /// Boot the server at genesis under `boot_env` with a default-sized guest
    /// RAM. The genesis timeline sits at `Moment` 0, untainted — the client
    /// snapshots it at `connect` and branches every materialization off it.
    pub fn boot(boot_env: EnvSpec) -> Self {
        Self::boot_with_ram(boot_env, DEFAULT_RAM_BYTES)
    }

    /// Boot with an explicit guest RAM size (for exercising `read` range
    /// checks).
    pub fn boot_with_ram(boot_env: EnvSpec, ram_bytes: u64) -> Self {
        Self {
            ram_bytes,
            negotiated: false,
            next_snap: 0,
            snaps: BTreeMap::new(),
            cur: Timeline::from_env(boot_env, 0, false),
        }
    }

    /// The quiescence moment for the *current* world: far past any mid-workload
    /// target, derived from `world_seed` so it is deterministic and env-specific.
    /// Computed on demand (never a stored field), so it is always consistent
    /// with whatever timeline `branch`/`replay` last installed.
    fn quiescent(&self) -> Moment {
        1_000_000 + (self.cur.world_seed % 1_000_000)
    }

    /// The caps this mock advertises: the negotiated app-protocol version, the
    /// single `EnvSpec` blob version, no coverage producer, no SDK — the same
    /// pins the real server uses.
    fn caps(&self) -> Caps {
        Caps {
            protocol_version: control_proto::APP_PROTOCOL_VERSION,
            env_version_min: EnvSpec::BLOB_VERSION,
            env_version_max: EnvSpec::BLOB_VERSION,
            coverage: CoverageGeometry {
                map_bytes: 0,
                producer: 0,
            },
            flags: CapFlags::NONE,
        }
    }

    /// The first scripted fault-triggered crash strictly after `from` and at or
    /// before `to`: a staged [`CorruptMemory`](HostFault::CorruptMemory)
    /// override whose `Moment` the run would reach. Returns the crash `Moment`.
    fn crash_between(&self, from: Moment, to: Moment) -> Option<Moment> {
        self.cur
            .env
            .overrides()
            .iter()
            .find(|(m, a)| {
                **m > from
                    && **m <= to
                    && matches!(a, Action::Host(HostFault::CorruptMemory { .. }))
            })
            .map(|(m, _)| *m)
    }
}

impl Server for MockServer {
    fn hello(&mut self, _caps: Caps) -> Result<Caps, SessionError> {
        self.negotiated = true;
        Ok(self.caps())
    }

    fn snapshot(&mut self) -> Result<Snapshot, SessionError> {
        if !self.negotiated {
            return Err(SessionError::Control(
                control_proto::ControlError::Unsupported,
            ));
        }
        let id = self.next_snap;
        self.next_snap += 1;
        // Capture the whole timeline verbatim.
        self.snaps.insert(id, self.cur.clone());
        Ok(Snapshot {
            id: SnapId(id),
            tainted: self.cur.tainted,
        })
    }

    fn drop_snap(&mut self, snap: SnapId) -> Result<(), SessionError> {
        if self.snaps.remove(&snap.0).is_none() {
            return Err(SessionError::Control(
                control_proto::ControlError::UnknownSnapshot(snap),
            ));
        }
        Ok(())
    }

    fn branch(&mut self, snap: SnapId, env: &Environment) -> Result<(), SessionError> {
        let Some(meta) = self.snaps.get(&snap.0).cloned() else {
            return Err(SessionError::Control(
                control_proto::ControlError::UnknownSnapshot(snap),
            ));
        };
        if env.blob_version != EnvSpec::BLOB_VERSION {
            return Err(SessionError::Control(
                control_proto::ControlError::BadEnvVersion(env.blob_version),
            ));
        }
        let spec = EnvSpec::decode(&env.bytes)
            .map_err(|_| control_proto::ControlError::MalformedEnvironment)?;
        // Restore the snapshot's position and inherit its lineage taint (task
        // 81: a branch off an untainted genesis is untainted), then reseed the
        // world with the new env.
        self.cur = Timeline::from_env(spec, meta.moment, meta.tainted);
        Ok(())
    }

    fn replay(&mut self, snap: SnapId) -> Result<(), SessionError> {
        let Some(meta) = self.snaps.get(&snap.0).cloned() else {
            return Err(SessionError::Control(
                control_proto::ControlError::UnknownSnapshot(snap),
            ));
        };
        // Verbatim restore of the whole captured timeline (world_seed + env +
        // moment + taint) — `replay`'s contract. Restoring only moment/taint
        // would leave the *current* world in place, so a replay after a branch
        // would read the wrong world.
        self.cur = meta;
        Ok(())
    }

    fn run(&mut self, until: StopConditions) -> Result<StopReason, SessionError> {
        let quiescent = self.quiescent();
        let target = match until.deadline {
            // A deadline at or behind the current point is already met: report
            // the effective V-time without advancing (the adapter's probe
            // semantics).
            Some(v) if v.0 <= self.cur.moment => {
                return Ok(StopReason::Deadline {
                    vtime: VTime(self.cur.moment),
                });
            }
            Some(v) => v.0.min(quiescent),
            // No deadline: run to quiescence.
            None => quiescent,
        };
        // A staged CorruptMemory the run reaches crashes the guest there.
        if let Some(crash_at) = self.crash_between(self.cur.moment, target) {
            self.cur.moment = crash_at;
            return Ok(StopReason::Crash {
                vtime: VTime(crash_at),
                info: CrashInfo {
                    kind: CrashKind::Panic,
                    detail: b"scripted fault: CorruptMemory".to_vec(),
                },
            });
        }
        self.cur.moment = target;
        if target >= quiescent {
            Ok(StopReason::Quiescent {
                vtime: VTime(quiescent),
            })
        } else {
            Ok(StopReason::Deadline {
                vtime: VTime(target),
            })
        }
    }

    fn hash(&mut self, scope: HashScope) -> Result<[u8; 32], SessionError> {
        Ok(self.cur.hash(scope))
    }

    fn read(&mut self, gpa: u64, len: u32) -> Result<Vec<u8>, SessionError> {
        if len > READ_CAP {
            return Err(SessionError::ReadTooLarge { len, cap: READ_CAP });
        }
        let end = gpa.checked_add(u64::from(len));
        match end {
            Some(end) if end <= self.ram_bytes => {
                let mut out = Vec::with_capacity(len as usize);
                for i in 0..u64::from(len) {
                    out.push(self.cur.mem_byte(gpa + i));
                }
                Ok(out)
            }
            _ => Err(SessionError::ReadOutOfRange {
                gpa,
                len,
                ram_len: self.ram_bytes,
            }),
        }
    }

    fn regs(&mut self) -> Result<RegsView, SessionError> {
        Ok(self.cur.regs())
    }

    fn exec(&mut self, cmd: &str, _deadline: VTime) -> Result<ExecResult, SessionError> {
        // The improvisation taints the timeline (structural, not conventional).
        self.cur.tainted = true;
        // Crude scripted "serial shell": echo a prompt + the command + a
        // deterministic canned line, exactly the off-the-record channel task 81
        // rules is exempt from the determinism discipline.
        let mut output = Vec::new();
        output.extend_from_slice(b"# ");
        output.extend_from_slice(cmd.as_bytes());
        output.extend_from_slice(b"\n");
        output.extend_from_slice(format!("(scripted output for {} bytes)\n", cmd.len()).as_bytes());
        Ok(ExecResult {
            output,
            ok: true,
            tainted: true,
        })
    }

    fn recorded_env(&mut self) -> Result<EnvSpec, SessionError> {
        if self.cur.tainted {
            // The task-81 guard: a tainted timeline never mints a reproducer.
            return Err(SessionError::Tainted);
        }
        Ok(self.cur.env.clone())
    }
}

/// The world seed: a digest of the env's canonical bytes. Two envs that differ
/// in one override differ here, so the whole scripted world diverges — the
/// engine of the counterfactual.
fn world_seed(env: &EnvSpec) -> u64 {
    splitmix64(fnv1a(&env.encode()))
}

/// FNV-1a over bytes — a small, dependency-free, deterministic 64-bit hash (the
/// mock's observables are scripted, not cryptographic; integer math only,
/// conventions rule 4).
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h = 0xCBF2_9CE4_8422_2325u64;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

/// SplitMix64 — a deterministic bijective mixer used to expand scripted state.
fn splitmix64(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Expand a set of `u64` inputs into a 32-byte digest by chaining
/// [`splitmix64`]. Deterministic and order-sensitive.
fn digest32(inputs: &[u64]) -> [u8; 32] {
    let mut s = 0x0123_4567_89AB_CDEFu64;
    for &v in inputs {
        s = splitmix64(s ^ v);
    }
    let mut out = [0u8; 32];
    for chunk in out.chunks_mut(8) {
        s = splitmix64(s);
        chunk.copy_from_slice(&s.to_le_bytes());
    }
    out
}
