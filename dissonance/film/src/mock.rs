// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`MockBillboardServer`] — an in-crate control-transport server that serves
//! **synthetic billboard bytes** (the task-82 loopback pattern, specialized for
//! film).
//!
//! `resolution`'s own `MockServer` serves scripted *pure-function* memory; film
//! needs a server whose billboard window returns a well-formed
//! [`encode_billboard`] buffer whose frame counter matches the frame-clock
//! `Moment` the projector materialized at. This mock is that: construct a
//! [`BillboardScenario`] (a frame clock + region sizes), drive it through a
//! [`resolution::Session`], and materializing at frame `N`'s `Moment` then
//! reading the window yields a header stamped frame `N`.
//!
//! It also carries the two test knobs the projector's edge paths need — a
//! [`Corruption`] policy (to prove the header-mismatch **hard error**) and an
//! injected-drop counter (to prove **re-materialize recovery**) — so the whole
//! projector contract gates with no core and no socket.

use std::collections::BTreeMap;

use control_proto::{
    CapFlags, Caps, ControlError, CoverageGeometry, CrashInfo, CrashKind, HashScope, Reproducer,
    SnapId, StopConditions, StopReason,
};
use environment::{EnvSpec, Moment};
use resolution::{ExecResult, READ_CAP, RegsView, Server, SessionError, Snapshot};

use crate::billboard::{HEADER_LEN, encode_billboard};
use crate::plan::{BillboardWindow, FrameTick};

/// A quiescence point far past any frame the tests address, so `run(until = m)`
/// always lands exactly at `m` (a `Deadline`) rather than short.
const MOCK_QUIESCENT: Moment = u64::MAX;

/// How the mock corrupts the billboard it serves — for driving the projector's
/// hard-error path (a mismatch is never a silently misaligned frame).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Corruption {
    /// Serve a faithful billboard (the default).
    #[default]
    None,
    /// Stamp `frame + 1` into the header, so [`crate::BillboardHeader::verify`]
    /// rejects it.
    WrongFrame,
    /// Flip the magic, so the header fails to parse.
    BadMagic,
}

/// A scripted billboard scenario: the frame clock and the region sizes. The
/// billboard window length is derived from the sizes so the served buffer and the
/// read window match exactly.
#[derive(Clone, Debug)]
pub struct BillboardScenario {
    /// The billboard buffer's guest-physical base.
    pub gpa: u64,
    /// The frame clock — one `(frame, Moment)` per recorded vblank.
    pub ticks: Vec<FrameTick>,
    /// The synthetic savestate region length.
    pub savestate_len: usize,
    /// The synthetic work-RAM region length.
    pub workram_len: usize,
    /// Guest RAM size (the `read` range ceiling). Must exceed the window.
    pub ram_bytes: u64,
}

impl BillboardScenario {
    /// A scenario at `gpa` over `ticks` with default NES-ish region sizes (a
    /// small savestate + 2 KiB work RAM) and ample RAM.
    pub fn new(gpa: u64, ticks: Vec<FrameTick>) -> Self {
        Self {
            gpa,
            ticks,
            savestate_len: 64,
            workram_len: 2048,
            ram_bytes: 1 << 30,
        }
    }

    /// The billboard window (`gpa`, derived `len`) this scenario serves.
    pub fn window(&self) -> BillboardWindow {
        BillboardWindow {
            gpa: self.gpa,
            len: (HEADER_LEN + self.savestate_len + self.workram_len) as u32,
        }
    }
}

/// A scripted control-transport server serving synthetic billboards. Implements
/// [`resolution::Server`]; drive it through [`resolution::Session`].
#[derive(Clone, Debug)]
pub struct MockBillboardServer {
    window: BillboardWindow,
    savestate_len: usize,
    workram_len: usize,
    ram_bytes: u64,
    /// `Moment → frame`, for `O(log n)` frame lookup at the current point.
    frame_by_moment: BTreeMap<Moment, u32>,
    negotiated: bool,
    next_snap: u64,
    /// Snapshots capture only the `Moment` (the mock's whole observable state).
    snaps: BTreeMap<u64, Moment>,
    cur_moment: Moment,
    corrupt: Corruption,
    /// The number of upcoming `read` round-trips to fail with a transport drop
    /// before serving normally — exercises the projector's re-materialize
    /// recovery.
    drops_remaining: u32,
    /// If set, a `run` whose target is beyond this `Moment` stops **short** at it
    /// with a scripted crash (the guest crashed/quiesced before the requested
    /// frame) — exercises the projector's [`crate::FilmError::ShortRun`] path.
    stop_short_at: Option<Moment>,
}

impl MockBillboardServer {
    /// Boot a mock at genesis (`Moment` 0) from a scenario.
    pub fn boot(scenario: BillboardScenario) -> Self {
        let window = scenario.window();
        let frame_by_moment = scenario.ticks.iter().map(|t| (t.moment, t.frame)).collect();
        Self {
            window,
            savestate_len: scenario.savestate_len,
            workram_len: scenario.workram_len,
            ram_bytes: scenario.ram_bytes,
            frame_by_moment,
            negotiated: false,
            next_snap: 0,
            snaps: BTreeMap::new(),
            cur_moment: 0,
            corrupt: Corruption::None,
            drops_remaining: 0,
            stop_short_at: None,
        }
    }

    /// Set the corruption policy the served billboards carry.
    pub fn with_corruption(mut self, corrupt: Corruption) -> Self {
        self.corrupt = corrupt;
        self
    }

    /// Inject `n` transport drops on the next `n` `read` round-trips, then serve
    /// normally — drives the projector's drop-recovery path.
    pub fn with_read_drops(mut self, n: u32) -> Self {
        self.drops_remaining = n;
        self
    }

    /// Make every `run` past `moment` stop **short** at it with a scripted crash
    /// (the guest never reaches later frames) — drives the projector's
    /// [`crate::FilmError::ShortRun`] path.
    pub fn with_stop_short_at(mut self, moment: Moment) -> Self {
        self.stop_short_at = Some(moment);
        self
    }

    /// The caps this mock advertises — the same pins the real server uses (so
    /// [`resolution::Session::connect`] negotiates).
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

    /// The frame displayed at the current `Moment`: the exact tick, else the
    /// nearest earlier one (a real billboard shows the last frame rendered). Falls
    /// back to `0` before the first tick.
    fn current_frame(&self) -> u32 {
        self.frame_by_moment
            .range(..=self.cur_moment)
            .next_back()
            .map(|(_, &f)| f)
            .unwrap_or(0)
    }

    /// The full synthetic billboard buffer for `frame`, sized to the window. The
    /// savestate/work-RAM bytes are a deterministic function of `frame`, so
    /// distinct frames render to distinct pictures under the fake renderer.
    fn billboard_bytes(&self, frame: u32) -> Vec<u8> {
        let savestate: Vec<u8> = (0..self.savestate_len)
            .map(|i| (frame as usize).wrapping_add(i) as u8)
            .collect();
        let workram: Vec<u8> = (0..self.workram_len)
            .map(|i| (i as u8) ^ (frame as u8))
            .collect();
        let joypad = (frame & 0xFF) as u8;
        let stamped = match self.corrupt {
            Corruption::WrongFrame => frame.wrapping_add(1),
            _ => frame,
        };
        let mut buf = encode_billboard(stamped, joypad, &savestate, &workram);
        if self.corrupt == Corruption::BadMagic {
            buf[0] ^= 0xFF;
        }
        // The window is exactly header + regions, so no resize is needed; guard
        // anyway so a mis-sized scenario pads rather than under-reads.
        buf.resize(self.window.len as usize, 0);
        buf
    }

    /// Whether this `read` round-trip should be dropped (and decrement the
    /// budget).
    fn take_drop(&mut self) -> bool {
        if self.drops_remaining > 0 {
            self.drops_remaining -= 1;
            true
        } else {
            false
        }
    }
}

impl Server for MockBillboardServer {
    fn hello(&mut self, _caps: Caps) -> Result<Caps, SessionError> {
        self.negotiated = true;
        Ok(self.caps())
    }

    fn snapshot(&mut self) -> Result<Snapshot, SessionError> {
        if !self.negotiated {
            return Err(SessionError::Control(ControlError::Unsupported));
        }
        let id = self.next_snap;
        self.next_snap += 1;
        self.snaps.insert(id, self.cur_moment);
        Ok(Snapshot {
            id: SnapId(id),
            tainted: false,
        })
    }

    fn drop_snap(&mut self, snap: SnapId) -> Result<(), SessionError> {
        if self.snaps.remove(&snap.0).is_none() {
            return Err(SessionError::Control(ControlError::UnknownSnapshot(snap)));
        }
        Ok(())
    }

    fn branch(&mut self, snap: SnapId, env: &Reproducer) -> Result<(), SessionError> {
        let Some(&moment) = self.snaps.get(&snap.0) else {
            return Err(SessionError::Control(ControlError::UnknownSnapshot(snap)));
        };
        if env.blob_version != EnvSpec::BLOB_VERSION {
            return Err(SessionError::Control(ControlError::BadEnvVersion(
                env.blob_version,
            )));
        }
        // The env content does not change the mock's observables (the frame clock
        // is fixed by the scenario), but decode it so a malformed reproducer is
        // rejected exactly as the real server would.
        EnvSpec::decode(&env.bytes).map_err(|_| ControlError::MalformedEnvironment)?;
        self.cur_moment = moment;
        Ok(())
    }

    fn replay(&mut self, snap: SnapId) -> Result<(), SessionError> {
        let Some(&moment) = self.snaps.get(&snap.0) else {
            return Err(SessionError::Control(ControlError::UnknownSnapshot(snap)));
        };
        self.cur_moment = moment;
        Ok(())
    }

    fn run(&mut self, until: StopConditions) -> Result<StopReason, SessionError> {
        let target = until.deadline.map(|v| v.0).unwrap_or(MOCK_QUIESCENT);
        // Scripted short landing: the guest crashes before `target` — the run
        // stops at `stop_short_at` and reports a crash, never advancing to the
        // requested frame.
        if let Some(s) = self.stop_short_at
            && target > s
            && s >= self.cur_moment
        {
            self.cur_moment = s;
            return Ok(StopReason::Crash {
                vtime: control_proto::Moment(s),
                info: CrashInfo {
                    kind: CrashKind::Panic,
                    detail: b"mock: scripted stop-short".to_vec(),
                },
            });
        }
        // V-time is monotonic — never rewind.
        if target <= self.cur_moment {
            return Ok(StopReason::Deadline {
                vtime: control_proto::Moment(self.cur_moment),
            });
        }
        self.cur_moment = target;
        Ok(StopReason::Deadline {
            vtime: control_proto::Moment(self.cur_moment),
        })
    }

    fn hash(&mut self, scope: HashScope) -> Result<[u8; 32], SessionError> {
        // A deterministic digest of (moment, scope) — hash-invariant under
        // read/regs since neither touches cur_moment.
        let tag: u64 = match scope {
            HashScope::Whole => 0,
            HashScope::Disk => 1,
            HashScope::Region { base, len } => base ^ len ^ 2,
        };
        let mut out = [0u8; 32];
        for (i, chunk) in out.chunks_mut(8).enumerate() {
            let v = self
                .cur_moment
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                .wrapping_add(tag)
                .wrapping_add(i as u64);
            chunk.copy_from_slice(&v.to_le_bytes());
        }
        Ok(out)
    }

    fn read(&mut self, gpa: u64, len: u32) -> Result<Vec<u8>, SessionError> {
        if len > READ_CAP {
            return Err(SessionError::ReadTooLarge { len, cap: READ_CAP });
        }
        let end = match gpa.checked_add(u64::from(len)) {
            Some(e) if e <= self.ram_bytes => e,
            _ => {
                return Err(SessionError::ReadOutOfRange {
                    gpa,
                    len,
                    ram_len: self.ram_bytes,
                });
            }
        };
        // Injected transport drop (before serving) — exercises drop recovery.
        if self.take_drop() {
            return Err(SessionError::Transport("mock: injected read drop".into()));
        }
        let win_start = self.window.gpa;
        // Overflow-safe window end (scenarios control gpa, but stay total).
        let win_end = win_start.checked_add(u64::from(self.window.len));
        if let Some(win_end) = win_end
            && gpa >= win_start
            && end <= win_end
        {
            let full = self.billboard_bytes(self.current_frame());
            let off = (gpa - win_start) as usize;
            return Ok(full[off..off + len as usize].to_vec());
        }
        // Outside the billboard window: deterministic filler (film never reads
        // here, but a read must still be a total function).
        Ok(vec![0u8; len as usize])
    }

    fn regs(&mut self) -> Result<RegsView, SessionError> {
        Ok(RegsView {
            version: RegsView::VERSION,
            gpr: [0; 16],
            rip: 0,
            rflags: 0,
            seg: [0; 6],
            cr0: 0,
            cr3: 0,
            cr4: 0,
            moment: self.cur_moment,
            vtime: self.cur_moment,
        })
    }

    fn exec(
        &mut self,
        _cmd: &str,
        _deadline: control_proto::Moment,
    ) -> Result<ExecResult, SessionError> {
        // Film is observation-only; it never improvises. Refuse loudly so a
        // stray exec would fail a test rather than pass silently.
        Err(SessionError::Control(ControlError::Unsupported))
    }

    fn recorded_env(&mut self) -> Result<EnvSpec, SessionError> {
        // Likewise never used by film.
        Err(SessionError::Control(ControlError::Unsupported))
    }
}
