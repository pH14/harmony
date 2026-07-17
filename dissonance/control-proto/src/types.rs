// SPDX-License-Identifier: AGPL-3.0-or-later
//! The control-plane value types: the opaque carried units the explorer ferries
//! schema-blind ([`Reproducer`], [`Answer`]), the handles and addressing
//! ([`SnapId`], [`Moment`], [`DecisionId`]), the request/reply verbs
//! ([`Request`], [`Reply`]), the run-control inputs ([`StopConditions`],
//! [`StopMask`], [`HashScope`]), and the guest-observable run outcomes
//! ([`StopReason`] and its payloads). All are plain data; the wire codec lives in
//! [`mod@crate::codec`].

/// One run's **reproducer** — the recorded artifact (entropy, scheduling,
/// payload, and faults) that reconstitutes its environment — carried as an
/// **opaque, versioned blob**. R2 is schema-blind: it never parses these
/// bytes (their structure is `environment::EnvSpec`'s contract). `blob_version`
/// lets the backend answer [`BadEnvVersion`](crate::ControlError::BadEnvVersion)
/// without the codec ever validating it (the codec carries any version through).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Reproducer {
    /// The `EnvSpec` blob-format version (validated by the backend, not the codec).
    pub blob_version: u16,
    /// The opaque serialized `EnvSpec`.
    pub bytes: Vec<u8>,
}

/// The opaque resolution of one [`Decision`](StopReason::Decision), carried
/// schema-blind. Its structure is `environment::Answer`'s contract; the backend
/// checks it for admissibility before staging, never the codec.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Answer(pub Vec<u8>);

/// An opaque host-plane perturbation, carried schema-blind — the host-plane
/// analogue of [`Answer`]. Its structure is `environment::HostFault`'s contract
/// (the bytes of `HostFault::encode`); the backend decodes and applies it, never
/// the codec. Staged by [`Perturb`](Request::Perturb).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct HostFault(pub Vec<u8>);

/// A point on the single deterministic axis. Mirrors `environment::Moment`
/// (conventions rule 2 — defined locally, not imported). Single-vCPU
/// determinism makes a bare axis value a unique moment: a host fault is staged
/// at one via [`Perturb`](Request::Perturb), a run deadline names one, and
/// every [`StopReason`] is stamped with the one it stopped at.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Moment(pub u64);

/// A pool-wide snapshot handle returned by [`Snapshot`](Request::Snapshot).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct SnapId(pub u64);

/// Identifies the one outstanding [`Decision`](StopReason::Decision). Single-vCPU
/// determinism guarantees at most one is ever outstanding.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct DecisionId(pub u64);

/// Decision-class discriminants, frozen to mirror `environment::DecisionClass`
/// (conventions rule 2 — defined locally, not imported). [`StopMask::arm`] takes
/// one of these as its `class_bit` and sets bit `1 << class_bit`; both crates
/// encode the identical bit so the armed-class set can never diverge. The numbers
/// are task 24's `DecisionClass` enum (`1..=6`, plus task 73's `Buggify` = `7`)
/// and never move — the `class_bit_mirrors_decision_class` test pins them against
/// the real enum so they cannot drift apart.
pub mod class_bit {
    /// `DecisionClass::Entropy` — the guest pulled entropy.
    pub const ENTROPY: u16 = 1;
    /// `DecisionClass::Payload` — the guest pulled a fuzz payload.
    pub const PAYLOAD: u16 = 2;
    /// `DecisionClass::Scheduler` — a schedulable yield point.
    pub const SCHEDULER: u16 = 3;
    /// `DecisionClass::NetFlow` — a per-flow network decision (the host decides a
    /// flow policy the guest enforces in-guest; task 50 reshaped this from the
    /// per-frame `NetSend` and retired `pv-net`). The `NET_SEND` const name is
    /// retained for wire stability — the discriminant `4` (and thus the `StopMask`
    /// bit) is unchanged.
    pub const NET_SEND: u16 = 4;
    /// `DecisionClass::BlockIo` — a block read/write/flush.
    pub const BLOCK_IO: u16 = 5;
    /// `DecisionClass::Process` — a node lifecycle point.
    pub const PROCESS: u16 = 6;
    /// `DecisionClass::Buggify` — a buggify decision (task 73). Per-**point**, not
    /// per-class, so it is never armed via [`StopMask::arm`] to auto-service a
    /// whole class; the mirror exists so the discriminant is pinned against the
    /// enum and reserved (bit `7`) alongside the standalone SDK-stop bits.
    pub const BUGGIFY: u16 = 7;
    /// The SDK lifecycle **snapshot point** stop (task 73, `setup_complete`) — a
    /// **standalone** stop class, NOT a `DecisionClass` (so it starts at 8, past
    /// the decision discriminants + the reserved buggify bit 7). A deferred
    /// snapshot point surfaces from a `Run` only when this bit is armed (round-7).
    pub const SNAPSHOT_POINT: u16 = 8;
    /// The SDK **assertion** stop (task 73) — a standalone stop class; an
    /// `assert_always` violation surfaces only when this bit is armed. `StopMask::
    /// NONE` runs a cooperating-SDK guest straight through to the terminal.
    pub const ASSERTION: u16 = 9;
}

/// A bitset over decision/exit **classes** that selects which non-terminal
/// decisions **and cooperating-SDK stops** surface from a [`Run`](Request::Run)
/// (vs. being auto-serviced / run through). The substrate **terminals** —
/// crash / quiescence / deadline — always stop regardless of the mask; the SDK
/// stops [`Assertion`](StopReason::Assertion) and
/// [`SnapshotPoint`](StopReason::SnapshotPoint) are gated on their class bits
/// ([`class_bit::ASSERTION`] / [`class_bit::SNAPSHOT_POINT`], round-7), so
/// `StopMask::NONE` runs an SDK guest straight through to the terminal.
///
/// Bit layout is the integrator-pinned mapping: `bit N == (1 << class_bit)` where
/// `class_bit` is the [`class_bit`] — the `environment::DecisionClass` discriminant
/// for decision classes (1..=6, plus 7 reserved for buggify), and standalone
/// constants (≥ 8) for the SDK stops. The same bit is computed in both crates so
/// the armed-class set can never diverge.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct StopMask(pub u32);

impl StopMask {
    /// The empty mask — only the always-on terminal classes surface.
    pub const NONE: Self = StopMask(0);

    /// Arm the given class so its decisions surface. Sets bit `1 << class_bit`.
    /// A `class_bit ≥ 32` cannot be represented and is a no-op (panic-free); the
    /// real discriminants are `1..=6`.
    #[must_use]
    pub fn arm(self, class_bit: u16) -> Self {
        match 1u32.checked_shl(u32::from(class_bit)) {
            Some(bit) => StopMask(self.0 | bit),
            None => self,
        }
    }

    /// Whether the given class is armed. `false` for any `class_bit ≥ 32`.
    pub fn armed(&self, class_bit: u16) -> bool {
        match 1u32.checked_shl(u32::from(class_bit)) {
            Some(bit) => self.0 & bit != 0,
            None => false,
        }
    }
}

/// What a [`Run`](Request::Run) advances toward: an optional V-time `deadline`
/// and the class mask `on` selecting which decisions surface.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct StopConditions {
    /// Stop with [`Deadline`](StopReason::Deadline) at this V-time, if set.
    pub deadline: Option<Moment>,
    /// Which decision classes surface (vs. auto-service).
    pub on: StopMask,
}

/// The scope of a [`Hash`](Request::Hash) digest — the determinism primitive.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum HashScope {
    /// The whole VM state.
    Whole,
    /// The disk only.
    Disk,
    /// A `[base, base + len)` region of guest physical memory.
    Region {
        /// Region base address.
        base: u64,
        /// Region length in bytes.
        len: u64,
    },
}

/// An out-of-band control-plane request. [`Hello`](Self::Hello) must be the first
/// frame on a session.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Request {
    /// Negotiate protocol/blob versions and coverage geometry. Must be first.
    Hello(Caps),
    /// Capture state at a quiescent point → [`Snapshot`](Reply::Snapshot), the
    /// seal-bound reply carrying the handle **and** its evidence cut (task 127).
    /// A failed or non-quiescent seal is an error reply carrying neither.
    Snapshot,
    /// Release a snapshot (pool GC) → [`Unit`](Reply::Unit).
    Drop(SnapId),
    /// Restore + reseed from `env` — the explore path → [`Unit`](Reply::Unit).
    Branch {
        /// The base snapshot to restore.
        snap: SnapId,
        /// The new environment to reseed with.
        env: Reproducer,
    },
    /// Restore verbatim — the reproduce / determinism-gate path →
    /// [`Unit`](Reply::Unit).
    Replay(SnapId),
    /// Advance the VM. `resolve` answers the immediately-prior
    /// [`Decision`](StopReason::Decision); a `resolve` with no outstanding
    /// decision is a loud [`ResolveWithoutDecision`](crate::ControlError::ResolveWithoutDecision),
    /// never silently dropped. Returns a [`Stop`](Reply::Stop).
    Run {
        /// When and on which classes to stop.
        until: StopConditions,
        /// The staged answer to the prior decision, if any.
        resolve: Option<Answer>,
    },
    /// Canonical state digest → [`Hash`](Reply::Hash).
    Hash {
        /// What to hash.
        scope: HashScope,
    },
    /// Stage a host-plane [`HostFault`] at `at`, recorded into the active
    /// environment → [`Unit`](Reply::Unit). The host plane rides this out-of-band
    /// channel (the guest never sees it); the backend decodes `fault` and applies
    /// it at its `Moment` during a `Run`. Mirrors the dissonance ruling's
    /// `perturb(fault, at)` verb.
    Perturb {
        /// The opaque host fault to stage (`environment::HostFault` bytes).
        fault: HostFault,
        /// The `Moment` (retired-instruction count) to apply it at.
        at: Moment,
    },
    /// Fetch a **page** of the link-tier SDK event capture of the current run
    /// (task 73), starting at event index `offset` → [`SdkEvents`](Reply::SdkEvents).
    /// The `Moment`-stamped `(moment, event_id, bytes)` stream a cooperating guest
    /// SDK emitted, so a remote client (the campaign's `SocketMachine`) can decode
    /// it into `RunTrace.events` — the server-side capture a socket client cannot
    /// otherwise see. The server bounds each page to the control frame limit, so a
    /// long capture is fetched by paging (`offset += page.len()`) until an empty
    /// page. Empty for a guest with no SDK, or once `offset` reaches the end.
    SdkEvents {
        /// The event index to start the page at.
        offset: u32,
    },
    /// Fetch a **page** of the guest **console** (serial) capture, starting at
    /// byte `offset` → [`Console`](Reply::Console). The scrape tier reads this to
    /// fill `RunTrace.records`, the input the log-template sensor (task 67
    /// `logtmpl`) clusters into the primary signal — the server-side serial
    /// capture a socket client (the campaign's `SocketMachine`) cannot otherwise
    /// see. Like [`SdkEvents`](Self::SdkEvents) it is a **pure read** (it never
    /// advances the VM or touches hashable state, so it is determinism-neutral)
    /// and **paged**: the reply carries the capture's total length so the client
    /// pages `offset..total` until it is drained, each page bounded to the control
    /// frame limit. The capture is per-snapshot cumulative (restored with the VM),
    /// so a client baselines `offset` at branch/replay time to read only a run's
    /// new bytes — exactly the cursor discipline the in-process recorder uses.
    Console {
        /// The byte offset into the serial capture to start the page at.
        offset: u32,
    },
    /// **Observation** (task 80): read `len` bytes of guest **physical** memory at
    /// `gpa` → [`Bytes`](Reply::Bytes). A pure observation — it never mutates guest
    /// state, V-time, or any hash, and is never recorded into an
    /// [`Reproducer`] (the `docs/RESOLUTION.md` search-surface criterion:
    /// observation, not a move). `len` is bounded by the backend's read cap
    /// ([`READ_CAP`](crate::READ_CAP)); an over-cap `len` is a loud
    /// [`ReadTooLarge`](crate::ControlError::ReadTooLarge) and a `[gpa, gpa+len)`
    /// range past guest RAM a loud [`ReadOutOfRange`](crate::ControlError::ReadOutOfRange)
    /// — **never** a truncated success.
    Read {
        /// The guest-physical base address to read from.
        gpa: u64,
        /// The number of bytes to read (bounded by [`READ_CAP`](crate::READ_CAP)).
        len: u32,
    },
    /// **Observation** (task 80): the current [`RegsView`] → [`Regs`](Reply::Regs).
    /// Like [`Read`](Request::Read) it is a pure observation (no state/V-time/hash
    /// mutation, never recorded into an [`Reproducer`]). Returns a **versioned**
    /// register *view*, not the save/restore format — additive evolution, no
    /// round-trip obligation.
    Regs,
    /// **Improvisation** (task 81): inject `cmd` on the guest's serial input (as if
    /// typed at the serial shell), run until a completion sentinel or the V-time
    /// `deadline`, and capture the serial output → [`ExecResult`](Reply::ExecResult).
    ///
    /// **Off the record, by ruling** (`docs/RESOLUTION.md` §Improvisations). `exec`
    /// is a one-off *improvisation*: it is **never recorded into any [`Reproducer`]**
    /// and carries **no determinism guarantee** — the serial byte channel is
    /// deliberately crude. What is airtight is the **taint guard** the server
    /// enforces around it: the first `exec` against a timeline sets that timeline's
    /// **taint bit**, every snapshot taken from it reports [`tainted`](Reply::Snapshot)
    /// `= true`, and minting a reproducer from it
    /// ([`RecordedEnv`](Request::RecordedEnv)) is a loud
    /// [`Tainted`](crate::ControlError::Tainted). The server **refuses nothing** — a
    /// caller may deliberately sacrifice a timeline; the taint bit makes the
    /// consequence structural rather than conventional (fork-first is a usage
    /// discipline, not a server rule).
    Exec {
        /// The command to inject on the serial shell (crude — no protocol beyond
        /// the shell itself). Carried as a UTF-8 string.
        cmd: String,
        /// The V-time deadline: the run stops at the completion sentinel or here,
        /// whichever is first.
        deadline: Moment,
    },
    /// **Reproducer mint** (task 81): return the **recorded reproducer** — the
    /// genesis-complete [`Reproducer`] that replays the current point — →
    /// [`Recorded`](Reply::Recorded), **or** a loud
    /// [`Tainted`](crate::ControlError::Tainted) when the current timeline has been
    /// tainted by an [`Exec`](Request::Exec) improvisation. This is the fail-loud
    /// site of the taint guard: an improvised timeline is off the record and has no
    /// honest reproducer, so the server refuses to mint one rather than hand back an
    /// [`Reproducer`] that does not reproduce (mirrors resolution's `recorded_env`).
    RecordedEnv,
}

/// A successful reply to a [`Request`]. Pairs with [`ControlError`](crate::ControlError)
/// in the `Result<Reply, ControlError>` the codec carries.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Reply {
    /// The negotiated capabilities (reply to [`Hello`](Request::Hello)).
    Hello(Caps),
    /// An acknowledgement with no value (reply to `Drop`/`Branch`/`Replay`).
    Unit,
    /// A guest-observable run outcome (reply to [`Run`](Request::Run)).
    Stop(StopReason),
    /// A 32-byte canonical digest (reply to [`Hash`](Request::Hash)).
    Hash([u8; 32]),
    /// The link-tier SDK event capture (reply to [`SdkEvents`](Request::SdkEvents)):
    /// the `Moment`-stamped `(moment, event_id, bytes)` stream, order-preserving.
    /// Empty for a guest with no SDK.
    SdkEvents(Vec<(u64, u32, Vec<u8>)>),
    /// A **page** of the guest console capture (reply to
    /// [`Console`](Request::Console)): `total` is the capture's full byte length
    /// (so the client knows when it has drained `offset..total`), and `chunk` is
    /// the bytes `serial[offset..]` bounded to the control frame limit. `chunk`
    /// is empty once `offset` reaches the end (or when there is no live VM).
    Console {
        /// The full byte length of the serial capture — the paging bound.
        total: u32,
        /// The requested page: `serial[offset..]`, frame-limit bounded.
        chunk: Vec<u8>,
    },
    /// The bytes of a [`Read`](Request::Read) — exactly `len` bytes of guest
    /// physical memory (never short; an under-range read is an error reply, not a
    /// truncated `Bytes`).
    Bytes(Vec<u8>),
    /// The current register view (reply to [`Regs`](Request::Regs)).
    Regs(RegsView),
    /// The result of an [`Exec`](Request::Exec) improvisation (task 81): the serial
    /// output captured while the command ran and whether it reached its completion
    /// sentinel before the deadline. There is **no** determinism guarantee on this
    /// payload — it is off the record by ruling; the taint bit (surfaced on
    /// [`Snapshot`](Reply::Snapshot) and enforced at [`RecordedEnv`](Request::RecordedEnv))
    /// is the airtight part, not this.
    ExecResult {
        /// The serial output captured while the command ran (crude; may include the
        /// shell's echo of the injected line — see the sentinel scheme in
        /// vmm-core's `IMPLEMENTATION.md`).
        output: Vec<u8>,
        /// Whether the command reached its completion sentinel before the deadline
        /// (`false` on a deadline timeout — the output is then whatever was captured
        /// so far).
        ok: bool,
    },
    /// The **seal-bound** snapshot reply (task 127) — the ONE reply to
    /// [`Snapshot`](Request::Snapshot), tainted or not. It binds, atomically
    /// from the **same stopped server state**: the pool-wide handle, the
    /// synchronized seal [`Moment`], the timeline taint (task 81), and the
    /// seal's **evidence cut** over the ordered SDK capture — the included
    /// SDK-event count. The stamp is the **sole authority** for the cut: a
    /// client never reconstructs it from a second read
    /// (`docs/DISSONANCE-STRATEGY.md`, "The cut is captured with the seal").
    ///
    /// **The cut is half-open, by prefix length — never by `Moment`
    /// comparison.** Persisted SDK-capture vector positions `< sdk_events` are
    /// included (including the exact subset emitted *at* the seal's `Moment`);
    /// positions `>= sdk_events` are excluded. Several events may share one
    /// stamped `Moment` (a V-time-anchor stamp, bead `hm-ynt`); the prefix
    /// length still cuts them exactly.
    ///
    /// **Console bytes are structurally outside this cut**: the serial capture
    /// is a distinct source-local, stop-granular byte stream (paged by
    /// [`Console`](Request::Console)) with no cursor here — it can never enter
    /// `sdk_events`. A later seal-relative source gets its **own** declared
    /// cursor field; independent cursors never imply cross-source order.
    ///
    /// (The pre-127 taint-free bare-handle reply — wire tag 2, `Reply::SnapId`
    /// — is retired: it carried no cut, so it could not honor the seal-evidence
    /// binding. `APP_PROTOCOL_VERSION` 8 gates the reshape at `hello`.)
    Snapshot {
        /// The pool-wide snapshot handle.
        id: SnapId,
        /// The synchronized seal `Moment` — the sealed state's own exact
        /// V-time (the same value a later restore's floor validates against).
        at: Moment,
        /// The included SDK-event count: the ordered SDK-capture vector's
        /// **prefix length** at the seal. Positions below it are included,
        /// at/after excluded. `0` for a guest with no SDK.
        sdk_events: u64,
        /// Whether the captured timeline is tainted by an
        /// [`Exec`](Request::Exec) improvisation (task 81), so an
        /// Archive/donation path can refuse admission without asking.
        tainted: bool,
    },
    /// The recorded reproducer (reply to [`RecordedEnv`](Request::RecordedEnv)): the
    /// genesis-complete [`Reproducer`] that replays the current point. Only ever
    /// sent for an **untainted** timeline — a tainted one is a loud
    /// [`Tainted`](crate::ControlError::Tainted) instead, never a lying reproducer.
    Recorded(Reproducer),
}

/// A **versioned** register view (task 80) — the observation surface for
/// `docs/RESOLUTION.md`. It is a *view*, not the save/restore format
/// ([`VcpuState`]-equivalent): additive evolution only, no round-trip obligation,
/// so [`version`](RegsView::VERSION) may gain fields without breaking a reader
/// that pins the older shape. Carries the general-purpose registers, `rip`,
/// `rflags`, the segment selectors, the control registers `cr0`/`cr3`/`cr4`, and
/// the current [`Moment`]/V-time the view is of.
///
/// `Moment` and `vtime` are the two names of the single deterministic axis on the
/// substrate (a retired-branch count == whole nanoseconds, ratio 1), so they
/// coincide there; both are carried so a reader need not know the ratio.
///
/// [`VcpuState`]: the backend's full save/restore vCPU record — not this view.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RegsView {
    /// The view schema version (task-80 additive-evolution contract). See
    /// [`RegsView::VERSION`].
    pub version: u16,
    /// The 16 general-purpose registers in canonical order:
    /// `rax rbx rcx rdx rsi rdi rbp rsp r8 r9 r10 r11 r12 r13 r14 r15`.
    pub gpr: [u64; 16],
    /// The instruction pointer.
    pub rip: u64,
    /// The flags register.
    pub rflags: u64,
    /// The segment selectors in canonical order: `cs ss ds es fs gs`.
    pub seg: [u16; 6],
    /// Control register `cr0`.
    pub cr0: u64,
    /// Control register `cr3` (the page-table base).
    pub cr3: u64,
    /// Control register `cr4`.
    pub cr4: u64,
    /// The current [`Moment`] (retired-instruction count) this view is of.
    pub moment: Moment,
    /// The current effective V-time.
    pub vtime: u64,
}

impl RegsView {
    /// The current [`RegsView`] schema version. **Additive-only**: a bump adds
    /// fields, never reshapes or drops one, so a reader pinning an older version
    /// keeps reading the prefix it knows.
    pub const VERSION: u16 = 1;
}

/// The guest-observable outcome of a [`Run`](Request::Run) — the explorer's
/// reaction surface. The substrate terminals (`Deadline`/`Quiescent`/`Crash`)
/// always surface; the cooperating-SDK stops (`Decision`/`SnapshotPoint`/
/// `Assertion`) appear only with a cooperating guest AND only when their
/// [`StopMask`] class bit is armed (round-7) — so `StopMask::NONE` runs an SDK
/// guest straight through to the terminal.
///
/// There is deliberately no `Host` variant: an in-band hypercall is serviced by
/// the consonance plane and the run continues; anything R2 must react to arrives
/// as [`Decision`](Self::Decision) / [`SnapshotPoint`](Self::SnapshotPoint) /
/// [`Assertion`](Self::Assertion).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum StopReason {
    /// The run reached its [`deadline`](StopConditions::deadline).
    Deadline {
        /// The V-time at which the run stopped.
        vtime: Moment,
    },
    /// HLT with an empty timer queue — the test ended.
    Quiescent {
        /// The V-time of quiescence.
        vtime: Moment,
    },
    /// The guest crashed.
    Crash {
        /// The V-time of the crash.
        vtime: Moment,
        /// What kind of crash, plus detail.
        info: CrashInfo,
    },
    /// A decision surfaced (its class was armed in the [`StopMask`]); answer it
    /// with the next [`Run`](Request::Run)'s `resolve`.
    Decision {
        /// The V-time of the decision.
        vtime: Moment,
        /// The outstanding decision's identity.
        id: DecisionId,
        /// Opaque service context for the explorer's policy.
        ctx: Vec<u8>,
    },
    /// An SDK lifecycle "ready" point.
    SnapshotPoint {
        /// The V-time of the snapshot point.
        vtime: Moment,
    },
    /// An SDK assertion fired (an `Always` violated or a `Sometimes` hit).
    Assertion {
        /// The V-time of the assertion.
        vtime: Moment,
        /// The event reference identifying the assertion.
        ev: EventRef,
    },
}

/// The kind of a guest [`Crash`](StopReason::Crash).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum CrashKind {
    /// A guest kernel/userspace panic.
    Panic,
    /// An unrecoverable CPU fault: the guest CPU entered a fault state it
    /// cannot return from and the substrate surfaced a reset/shutdown (on x86,
    /// a triple fault).
    UnrecoverableFault,
    /// An orderly guest-requested shutdown that the test treats as a crash.
    Shutdown,
}

/// Detail accompanying a [`Crash`](StopReason::Crash): its [`CrashKind`] and an
/// opaque diagnostic blob.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CrashInfo {
    /// The crash classification.
    pub kind: CrashKind,
    /// Opaque diagnostic bytes (a message, register dump, etc.).
    pub detail: Vec<u8>,
}

/// A reference to an SDK event surfaced by an [`Assertion`](StopReason::Assertion).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EventRef {
    /// The event identifier (an SDK assertion id).
    pub id: u32,
    /// Opaque event payload.
    pub data: Vec<u8>,
}

/// Session capabilities, exchanged in [`Hello`](Request::Hello) and its reply.
/// Version mismatches are detectable from these fields alone; the coverage-map
/// bytes themselves never travel on the socket (only their geometry).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Caps {
    /// The negotiated application protocol version (distinct from the wire
    /// [`PROTO_VERSION`](crate::PROTO_VERSION) carried in the frame header).
    pub protocol_version: u16,
    /// Lowest `Reproducer` blob version this peer accepts.
    pub env_version_min: u16,
    /// Highest `Reproducer` blob version this peer accepts.
    pub env_version_max: u16,
    /// Where/shape of the coverage shmem map (its bytes are never serialized).
    pub coverage: CoverageGeometry,
    /// Capability flags (e.g. `guest_has_sdk`, coverage-producer kind).
    pub flags: CapFlags,
}

/// The shape of the coverage shmem map. Only geometry crosses the socket; the map
/// bytes live in shared memory the integrator maps out of band.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct CoverageGeometry {
    /// The map size in bytes.
    pub map_bytes: u32,
    /// The coverage-producer kind (an opaque tag the integrator interprets).
    pub producer: u8,
}

/// A capability bitset carried in [`Caps::flags`]. The bit meanings are the
/// backend's contract; the codec only round-trips the `u32`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct CapFlags(pub u32);

impl CapFlags {
    /// No flags set.
    pub const NONE: Self = CapFlags(0);
    /// The guest carries a cooperating SDK (decisions/assertions/snapshot points
    /// can surface).
    pub const GUEST_HAS_SDK: Self = CapFlags(1);

    /// Whether every bit in `other` is set in `self`.
    pub fn contains(self, other: CapFlags) -> bool {
        self.0 & other.0 == other.0
    }

    /// `self` with every bit in `other` also set.
    #[must_use]
    pub fn with(self, other: CapFlags) -> Self {
        CapFlags(self.0 | other.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_mask_arm_sets_one_shifted_bit() {
        // The integrator-pinned mapping: armed bit == 1 << class_bit.
        for cb in [
            class_bit::ENTROPY,
            class_bit::PAYLOAD,
            class_bit::SCHEDULER,
            class_bit::NET_SEND,
            class_bit::BLOCK_IO,
            class_bit::PROCESS,
        ] {
            let m = StopMask::NONE.arm(cb);
            assert_eq!(m.0, 1u32 << cb);
            assert!(m.armed(cb));
            // No other class is armed.
            for other in 0u16..32 {
                if other != cb {
                    assert!(!m.armed(other));
                }
            }
        }
    }

    #[test]
    fn stop_mask_arm_is_idempotent_and_composes() {
        let m = StopMask::NONE
            .arm(class_bit::BLOCK_IO)
            .arm(class_bit::NET_SEND)
            .arm(class_bit::BLOCK_IO);
        assert!(m.armed(class_bit::BLOCK_IO));
        assert!(m.armed(class_bit::NET_SEND));
        assert!(!m.armed(class_bit::ENTROPY));
        assert_eq!(
            m.0,
            (1u32 << class_bit::BLOCK_IO) | (1u32 << class_bit::NET_SEND)
        );
    }

    #[test]
    fn stop_mask_out_of_range_class_is_a_total_noop() {
        // class_bit >= 32 cannot be represented; arm is a no-op and armed is
        // false — never a shift-overflow panic.
        for cb in [32u16, 33, 100, u16::MAX] {
            assert_eq!(StopMask::NONE.arm(cb), StopMask::NONE);
            assert!(!StopMask::NONE.arm(class_bit::BLOCK_IO).armed(cb));
        }
        assert!(!StopMask(u32::MAX).armed(32));
    }

    #[test]
    fn cap_flags_contains_and_with() {
        assert!(CapFlags::GUEST_HAS_SDK.contains(CapFlags::GUEST_HAS_SDK));
        assert!(CapFlags::GUEST_HAS_SDK.contains(CapFlags::NONE));
        assert!(!CapFlags::NONE.contains(CapFlags::GUEST_HAS_SDK));
        let both = CapFlags(0b10).with(CapFlags::GUEST_HAS_SDK);
        assert!(both.contains(CapFlags::GUEST_HAS_SDK));
        assert!(both.contains(CapFlags(0b10)));
        assert_eq!(both.0, 0b11);
        // Overlapping bits distinguish set-union (`|`) from XOR: `with` is
        // idempotent (re-adding a set bit keeps it; XOR would clear it).
        assert_eq!(
            CapFlags::GUEST_HAS_SDK.with(CapFlags::GUEST_HAS_SDK),
            CapFlags::GUEST_HAS_SDK,
            "with is a set-union, not XOR"
        );
        assert_eq!(CapFlags(0b11).with(CapFlags(0b01)), CapFlags(0b11));
    }
}
