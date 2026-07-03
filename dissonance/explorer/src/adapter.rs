// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **R2 socket adapter** (task 58): the first non-toy [`Machine`] — an
//! implementation of the driver seam over a `control-proto` client stream —
//! plus [`SpecEnvCodec`], the binding of the [`EnvCodec`](crate::EnvCodec) proposal seam to
//! `environment`'s real reproducer codec per the task-93 ruling
//! (`docs/DISSONANCE.md` §"Ruling (task 93)").
//!
//! ## The adapter blob (`R2A1`)
//!
//! The explorer ferries [`Environment`] values it never parses; this adapter
//! owns their structure. An adapter blob wraps a task-24 [`EnvSpec`] with the
//! two positions the task-93 ruling requires ([`AdapterEnv`]):
//!
//! - **`base_offset`** — the absolute `Moment` the blob's overrides are keyed
//!   from (`0` = genesis-complete). This is the ruling's "`at` provenance"
//!   point: a branch-local delta carries only since-the-branch overrides, so
//!   the offset must ride the blob for [`compose`](crate::EnvCodec::compose) to recover `at`
//!   from the delta alone (the production analogue of the toy blob's
//!   `base_offset`).
//! - **`pos`** — the absolute `Moment` the blob was captured at (a corpus
//!   base records where its snapshot was taken, so a later
//!   [`mutate`](crate::EnvCodec::mutate) can slice the suffix at the right offset —
//!   the toy's `pos`).
//!
//! **The `Moment` axis, on this substrate.** `environment::Moment` is defined
//! as a retired-*instruction* count; vmm-core's only deterministic axis today
//! is effective V-time (nanoseconds ≡ retired conditional branches, 1 ns per
//! branch under the contract clock). Until the task-59 exact-count machinery
//! exists, the adapter keys its offsets by **effective V-time as stamped in
//! every [`StopReason`]** — a deterministic, monotone anchor recoverable from
//! the delta alone, which is all the seed-driven contract needs (there are no
//! overrides to re-key yet). The axis choice is confined to this module.
//!
//! Only the inner `EnvSpec` bytes travel on the wire (`blob_version` =
//! `EnvSpec::BLOB_VERSION`): the server speaks pure task-24 blobs and never
//! learns the wrapper, so the two sides share no adapter-private schema.
//!
//! ## The task-93 adapter contract, implemented here
//!
//! - **Tail-completeness.** [`Machine::recorded_env`] emits every decision
//!   answered since the branch as an override (the adapter mediates every
//!   `run(resolve)`, stamping the answer at the decision's stop `Moment`).
//!   Seed-driven runs surface no decisions, so the delta is trivially
//!   tail-complete (and always the [`EnvSpec::Recorded`] variant, never
//!   `Seeded`, so compose's variant check cannot fire on adapter-minted
//!   artifacts).
//! - **Fallibility.** [`SpecEnvCodec`]'s `compose` (and `mutate`) **panic** on
//!   `UnsupportedComposition`/`Overflow` or a malformed adapter blob — the
//!   ruling's chosen alternative: these seams receive only adapter-minted
//!   artifacts, so a failure is a defect in the adapter/contract, not a run
//!   outcome, and the campaign aborts loudly rather than minting a reproducer
//!   that does not replay. (A fallible seam remains the allowed API adjustment
//!   the ruling names; the panic is its default.)
//! - **Standing-fault confinement** is vacuous in the v1 vocabulary (no
//!   standing faults exist); `mutate` still refuses (panics on) a
//!   standing-fault-carrying base rather than slicing one into a branch-local
//!   delta, so the confinement rule is enforced here the day they appear.
//! - **Parent-rooted chains (task 68).** `mutate`/`compose` operate in the
//!   base's **own coordinate system**: a base's override keys are relative to
//!   its `base_offset` (absolute keys are the `base_offset == 0` special
//!   case). `compose` splices at the **relative** cut
//!   `d.base_offset − b.base_offset` and keeps the base's root, so folding a
//!   suffix chain (`compose(suffixᵢ, suffixᵢ₊₁)` down a lineage) yields one
//!   delta still rooted at the chain's retained ancestor — exactly what the
//!   task-68 materialization engine replays with **one** branch. `mutate`
//!   slices at `b.pos − b.base_offset`. A delta keyed **before** its base's
//!   root (`d.base_offset < b.base_offset`) is a mis-ordered chain — a defect,
//!   panicked on (the same fail-loud, never-silently-mis-key discipline).
//!
//! ## Error mapping (two categories, preserved)
//!
//! A wire [`ControlError`](control_proto::ControlError) maps onto [`MachineError`] without ever crossing
//! into [`StopReason`]: `UnknownSnapshot` → `UnknownSnapshot`, `NotQuiescent`
//! / `SnapshotWhileArmed` → `NotQuiescent`, `BadEnvVersion` /
//! `MalformedEnvironment` → `BadEnvironment`, and everything else (including
//! `Unsupported` and a torn transport) → `Transport` — which aborts the
//! campaign step loudly, exactly as the engine requires.

use std::collections::BTreeMap;
use std::io::{Read, Write};

use environment::{Action, EnvSpec, FaultPolicy};

use crate::error::MachineError;
use crate::{Answer, Environment, Machine, SnapId, StopConditions, StopReason, VTime};

/// The adapter blob format version, mirrored into [`Environment::blob_version`]
/// for every explorer-side blob this adapter mints. Distinct from the inner
/// [`EnvSpec::BLOB_VERSION`] (which is what travels on the wire).
pub const ADAPTER_BLOB_VERSION: u16 = 1;

/// Adapter-blob container magic, `"R2A1"` read little-endian.
const MAGIC: u32 = u32::from_le_bytes(*b"R2A1");

/// Fixed prefix: magic(4) + version(2) + base_offset(8) + pos(8).
const HEADER_LEN: usize = 22;

/// A decoded adapter blob: the task-24 reproducer [`EnvSpec`] plus the two
/// `Moment` positions the task-93 ruling requires (see the module doc).
#[derive(Clone, PartialEq, Debug)]
pub struct AdapterEnv {
    /// The absolute `Moment` (effective V-time) this blob's overrides are
    /// keyed from; `0` = genesis-complete.
    pub base_offset: u64,
    /// The absolute `Moment` the blob was captured at (a corpus base's
    /// snapshot point; the terminal stop for a finished run's delta).
    pub pos: u64,
    /// The wrapped task-24 reproducer.
    pub spec: EnvSpec,
}

impl AdapterEnv {
    /// Encode to an explorer-side [`Environment`] (canonical bytes; the inner
    /// spec's encoding is already byte-deterministic).
    pub fn encode(&self) -> Environment {
        let spec = self.spec.encode();
        let mut bytes = Vec::with_capacity(HEADER_LEN + spec.len());
        bytes.extend_from_slice(&MAGIC.to_le_bytes());
        bytes.extend_from_slice(&ADAPTER_BLOB_VERSION.to_le_bytes());
        bytes.extend_from_slice(&self.base_offset.to_le_bytes());
        bytes.extend_from_slice(&self.pos.to_le_bytes());
        bytes.extend_from_slice(&spec);
        Environment {
            blob_version: ADAPTER_BLOB_VERSION,
            bytes,
        }
    }

    /// Decode an explorer-side [`Environment`] minted by this adapter. Strict
    /// and total: arbitrary bytes yield [`MachineError::BadEnvironment`]
    /// (carrying the blob's declared version), never a panic.
    pub fn decode(env: &Environment) -> Result<AdapterEnv, MachineError> {
        let bad = || MachineError::BadEnvironment(env.blob_version);
        if env.blob_version != ADAPTER_BLOB_VERSION {
            return Err(bad());
        }
        let b = &env.bytes;
        if b.len() < HEADER_LEN {
            return Err(bad());
        }
        // Indexing is in bounds: length checked against HEADER_LEN above.
        let magic = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        let version = u16::from_le_bytes([b[4], b[5]]);
        if magic != MAGIC || version != ADAPTER_BLOB_VERSION {
            return Err(bad());
        }
        let u64at = |o: usize| {
            let s = &b[o..o + 8];
            u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]])
        };
        let base_offset = u64at(6);
        let pos = u64at(14);
        let spec = EnvSpec::decode(&b[HEADER_LEN..]).map_err(|_| bad())?;
        Ok(AdapterEnv {
            base_offset,
            pos,
            spec,
        })
    }
}

/// Normalize a spec for a compose-safe artifact: the [`EnvSpec::Seeded`]
/// variant is promoted to [`EnvSpec::Recorded`] with no overrides (stream-wise
/// identical), so every adapter-emitted artifact is the `Recorded` variant the
/// production `compose` accepts.
fn recorded(spec: &EnvSpec) -> EnvSpec {
    match spec {
        EnvSpec::Recorded { .. } => spec.clone(),
        EnvSpec::Seeded { seed, policy } => EnvSpec::Recorded {
            seed: *seed,
            policy: policy.clone(),
            overrides: BTreeMap::new(),
            standing: Vec::new(),
        },
    }
}

// ---------------------------------------------------------------------------
// SpecEnvCodec — the EnvCodec seam bound to environment's real codec
// ---------------------------------------------------------------------------

/// The [`EnvCodec`](crate::EnvCodec) seam bound to `environment`'s real
/// reproducer codec, over [`AdapterEnv`] blobs — the production counterpart of
/// the test-side toy codec. A unit type; every operation is a pure function of
/// its inputs.
///
/// **Panics** (all three methods) on a malformed adapter blob, and `compose`
/// additionally on `UnsupportedComposition`/`Overflow` — per the task-93
/// ruling these seams receive only adapter-minted artifacts, so any failure is
/// an invariant violation that must abort the campaign loudly, never a run
/// outcome (see the module doc's contract section).
#[derive(Clone, Copy, Debug, Default)]
pub struct SpecEnvCodec;

impl SpecEnvCodec {
    /// Decode at a codec seam, where a malformed blob is an invariant
    /// violation (the ruling's loud abort), not untrusted input.
    fn require(env: &Environment, seam: &str) -> AdapterEnv {
        match AdapterEnv::decode(env) {
            Ok(e) => e,
            Err(e) => panic!(
                "SpecEnvCodec::{seam}: not an adapter-minted blob ({e}); the EnvCodec seam \
                 receives only adapter-minted artifacts (task-93 ruling — defect, not data)"
            ),
        }
    }
}

impl crate::EnvCodec for SpecEnvCodec {
    fn seeded(&self, seed: u64) -> Environment {
        // Genesis-complete, no overrides, the v1 default (never-fault) policy.
        // Minted as `Recorded` so even a bug found on a genesis-rooted first
        // run yields an artifact the production `compose` accepts as a base.
        AdapterEnv {
            base_offset: 0,
            pos: 0,
            spec: recorded(&environment::EnvCodec::seeded(seed, FaultPolicy::none())),
        }
        .encode()
    }

    fn mutate(&self, base: &Environment, salt: u64) -> Environment {
        let b = Self::require(base, "mutate");
        // Coordinate system (task 68): the base's override keys are relative
        // to its own `base_offset`, so the slice point is the **relative**
        // distance from the base's root to its capture position. A capture
        // position behind the root is a malformed blob — a defect, loud.
        let cut = b.pos.checked_sub(b.base_offset).unwrap_or_else(|| {
            panic!(
                "SpecEnvCodec::mutate: base captured at pos {} BEFORE its own root offset {} — \
                 a malformed chain blob (task-93 ruling: defect, never silently mis-key)",
                b.pos, b.base_offset
            )
        });
        // The branch this delta seeds runs from the base snapshot's capture
        // point, so slice the suffix at `cut` into a branch-local delta (keys
        // re-based to the branch origin), preserving seed/policy so a later
        // recompose is stream-consistent.
        //
        // NOTE (v1 sequencing): the underlying `environment::EnvCodec::mutate`
        // inserts a **host-plane** `Action::Host` override, so a mutate-minted
        // env is `Recorded` with an override. The seed-driven task-58 server
        // rejects any override-carrying `branch` env with `Unsupported` (host-
        // plane enforcement is task 59) — so mutate is only *usable* against the
        // server once task 59 lands. It is exercised here for the codec/rebasing
        // contract (compose consistency, slicing at `pos`), not as a live
        // campaign proposal yet.
        let (seed, policy) = (b.spec.seed(), b.spec.policy().clone());
        if let EnvSpec::Recorded { standing, .. } = &b.spec
            && !standing.is_empty()
        {
            // Standing-fault confinement (task-93 ruling): a standing-fault-
            // carrying base is never sliced into a branch-local delta. Vacuous
            // in the v1 vocabulary; enforced loudly the day they appear.
            panic!(
                "SpecEnvCodec::mutate: base carries standing faults — confined to genesis-based \
                 runs (task-93 ruling), never sliced into a branch-local delta"
            );
        }
        let suffix: BTreeMap<u64, Action> = b
            .spec
            .overrides()
            .iter()
            .filter(|(m, _)| **m >= cut)
            .map(|(m, a)| (m - cut, a.clone()))
            .collect();
        let sliced = EnvSpec::Recorded {
            seed,
            policy,
            overrides: suffix,
            standing: Vec::new(),
        };
        // One deterministic host-plane tweak via the real codec (guest
        // overrides are preserved verbatim by its contract).
        let mutated = environment::EnvCodec::mutate(&sliced, salt);
        AdapterEnv {
            base_offset: b.pos,
            pos: b.pos,
            spec: mutated,
        }
        .encode()
    }

    fn compose(&self, base: &Environment, branch_local: &Environment) -> Environment {
        let b = Self::require(base, "compose");
        let d = Self::require(branch_local, "compose");
        // Coordinate system (task 68), symmetric with `mutate`: the base's
        // keys are relative to its own `base_offset`, so the splice point is
        // the **relative** distance from the base's root to the delta's branch
        // origin (the ruling's "`at` provenance" — the delta carries the
        // absolute Moment it is keyed from, so the cut is recoverable from the
        // two blobs alone). With a genesis-complete base (`base_offset == 0`)
        // this reduces to the absolute splice the v1 flow always used; with a
        // parent-rooted base it is the chain fold the task-68 materialization
        // engine drives (`compose(suffixᵢ, suffixᵢ₊₁)` down a lineage), whose
        // result stays rooted at the base's own origin. A delta branched
        // BEFORE its base's root is a mis-ordered chain — a defect, loud.
        let cut = d.base_offset.checked_sub(b.base_offset).unwrap_or_else(|| {
            panic!(
                "SpecEnvCodec::compose: delta keyed from {} BEFORE the base's root offset {} — \
                 a mis-ordered chain (task-93 ruling: defect, never silently mis-key)",
                d.base_offset, b.base_offset
            )
        });
        let composed = match environment::EnvCodec::compose(&b.spec, &d.spec, cut) {
            Ok(spec) => spec,
            Err(e) => panic!(
                "SpecEnvCodec::compose failed ({e}) — unreachable under the task-93 adapter \
                 contract (tail-complete Recorded deltas, matching seed/policy, no standing \
                 faults); aborting rather than minting a reproducer that does not replay"
            ),
        };
        AdapterEnv {
            base_offset: b.base_offset,
            pos: d.pos,
            spec: composed,
        }
        .encode()
    }
}

// ---------------------------------------------------------------------------
// SocketMachine — the driver seam over a control-proto client stream
// ---------------------------------------------------------------------------

/// Per-snapshot client bookkeeping captured at `snapshot` time so a later
/// `replay`/`branch` restores the adapter's recording state along with the VM.
#[derive(Clone, Debug)]
struct SnapMeta {
    /// The absolute `Moment` (effective V-time) the snapshot was captured at —
    /// the `pos` of the timeline at capture, and the origin a **`branch`** off
    /// this snapshot roots its new timeline at.
    vtime: u64,
    /// The **branch origin** that was active when the snapshot was taken (the
    /// `Moment` [`spec`](Self::spec)'s overrides are keyed relative to). For a
    /// snapshot taken mid-timeline this differs from [`vtime`](Self::vtime)
    /// (the origin is where the *branch* began; `vtime` is how far it has run).
    /// **`replay`** restores *this* as the branch offset — not `vtime` — so the
    /// restored recording's keys keep their true origin and `recorded_env`
    /// advertises the correct `base_offset`.
    branch_offset: u64,
    /// The (branch-local) spec active at capture — its overrides are keyed
    /// relative to [`branch_offset`](Self::branch_offset).
    spec: EnvSpec,
}

/// The socket-backed [`Machine`]: every verb is one request/reply exchange on
/// a `control-proto` stream (a connected unix socket or an in-process
/// socketpair end). See the module doc for the blob format, the `Moment` axis,
/// and the error mapping.
///
/// The adapter owns the recording side of the task-93 contract: it tracks the
/// env each Modulation was branched with, stamps every resolved decision at its
/// stop `Moment`, and emits the tail-complete branch-local delta from
/// [`recorded_env`](Machine::recorded_env).
pub struct SocketMachine<S: Read + Write> {
    stream: S,
    seq: u32,
    inbuf: Vec<u8>,
    /// The request-encode scratch buffer, reused (cleared) per [`call`](Self::call)
    /// so the hot request/reply path does not churn the heap every verb —
    /// mirroring the server's reused `outbuf`.
    outbuf: Vec<u8>,
    /// The coverage view for the most recent run. The negotiated geometry is
    /// zero-width (no producer exists), so this is empty and never updated.
    coverage: Vec<u8>,
    snaps: BTreeMap<u64, SnapMeta>,
    /// The (branch-local) spec the current Modulation runs under, plus every
    /// decision answered since the branch (stamped by `run(resolve)`).
    current: EnvSpec,
    /// The absolute `Moment` of the current branch origin.
    branch_offset: u64,
    /// The absolute `Moment` of the last observed stop (the current position).
    pos: u64,
    /// The stop `Moment` of the surfaced-but-unanswered decision, if any —
    /// where the next `run(resolve)`'s answer is stamped (tail-completeness).
    pending_decision: Option<u64>,
}

impl<S: Read + Write> SocketMachine<S> {
    /// Connect: send `hello` (the first frame of a session) and validate the
    /// server's negotiated [`Caps`](control_proto::Caps) — application
    /// protocol 1 and an env-version range containing
    /// [`EnvSpec::BLOB_VERSION`]. `initial` is the environment the server's
    /// live VM is currently running under (its boot seed/policy — composition-
    /// root knowledge), so a genesis snapshot taken before any branch records
    /// the right spec.
    ///
    /// **Coverage geometry is rejected unless zero-width.** v1 has no coverage
    /// producer, so a non-zero `map_bytes` is either a misconfigured or hostile
    /// server; it is refused with a loud [`MachineError`] rather than sized into
    /// an unbounded allocation from a transport-provided `u32` (conventions
    /// rule 4 — never allocate on an untrusted length).
    ///
    /// **The current V-time is probed before returning**, so `pos` (and the
    /// pre-branch `branch_offset`) reflect where the server's live VM actually
    /// sits — **not** `0`. This is load-bearing: [`Explorer::new`](crate::Explorer::new)
    /// snapshots the freshly-connected machine *immediately*, before any `run`,
    /// and the server's VM may be sitting mid-workload (post-readiness); without
    /// the probe the genesis `SnapMeta.vtime` would record `0`, and a later
    /// `branch` off it would key its `recorded_env` delta from `0` instead of
    /// the true origin — a silently-mis-keyed reproducer, exactly the class the
    /// task-93 ruling forbids (`compose` cannot detect it). The probe is a
    /// `run` with an already-met deadline (`0`): the server checks the deadline
    /// before entering the guest, so it returns the effective V-time without
    /// advancing the VM.
    pub fn connect(stream: S, initial: EnvSpec) -> Result<Self, MachineError> {
        let mut machine = SocketMachine {
            stream,
            seq: 0,
            inbuf: Vec::new(),
            outbuf: Vec::new(),
            coverage: Vec::new(),
            snaps: BTreeMap::new(),
            current: recorded(&initial),
            branch_offset: 0,
            pos: 0,
            pending_decision: None,
        };
        let hello = control_proto::Request::Hello(client_caps());
        let caps = match machine.call(&hello)? {
            control_proto::Reply::Hello(caps) => caps,
            other => {
                return Err(MachineError::Transport(format!(
                    "hello answered with a non-hello reply: {other:?}"
                )));
            }
        };
        if caps.protocol_version != control_proto::APP_PROTOCOL_VERSION {
            return Err(MachineError::Transport(format!(
                "incompatible control protocol version {} (need {})",
                caps.protocol_version,
                control_proto::APP_PROTOCOL_VERSION,
            )));
        }
        if caps.env_version_min > EnvSpec::BLOB_VERSION
            || caps.env_version_max < EnvSpec::BLOB_VERSION
        {
            return Err(MachineError::Transport(format!(
                "server env-version range {}..={} does not admit EnvSpec v{}",
                caps.env_version_min,
                caps.env_version_max,
                EnvSpec::BLOB_VERSION
            )));
        }
        // v1 has no coverage producer: refuse a non-zero geometry rather than
        // allocate `map_bytes` (a transport-provided u32, up to ~4 GiB) blind.
        if caps.coverage.map_bytes != 0 || caps.coverage.producer != 0 {
            return Err(MachineError::Transport(format!(
                "server advertised a non-zero coverage geometry (map_bytes={}, producer={}) but \
                 v1 has no coverage producer — refusing to allocate on an untrusted length",
                caps.coverage.map_bytes, caps.coverage.producer
            )));
        }
        machine.coverage = Vec::new();

        // Probe the server's current effective V-time so `pos`/`branch_offset`
        // are the true origin before the first (immediate) snapshot — see the
        // doc above. A deadline of `0` is already met, so this does not advance
        // the VM.
        let origin = machine
            .run(
                &StopConditions {
                    deadline: Some(VTime(0)),
                    on: crate::StopMask::NONE,
                },
                None,
            )?
            .vtime()
            .0;
        machine.pos = origin;
        machine.branch_offset = origin;
        Ok(machine)
    }

    /// One request/reply exchange. A transport failure, a mismatched reply
    /// sequence number, or an error reply all surface as [`MachineError`].
    fn call(&mut self, req: &control_proto::Request) -> Result<control_proto::Reply, MachineError> {
        self.seq = self.seq.wrapping_add(1);
        self.outbuf.clear();
        control_proto::encode_request(self.seq, req, &mut self.outbuf)
            .map_err(|e| MachineError::Transport(format!("request encode failed: {e}")))?;
        self.stream
            .write_all(&self.outbuf)
            .and_then(|()| self.stream.flush())
            .map_err(|e| MachineError::Transport(format!("socket write failed: {e}")))?;
        let mut chunk = [0u8; 4096];
        loop {
            match control_proto::decode_reply(&self.inbuf)
                .map_err(|e| MachineError::Transport(format!("reply framing error: {e}")))?
            {
                Some((seq, reply, consumed)) => {
                    self.inbuf.drain(..consumed);
                    if seq != self.seq {
                        return Err(MachineError::Transport(format!(
                            "reply seq {seq} does not echo request seq {}",
                            self.seq
                        )));
                    }
                    return reply.map_err(control_error_to_machine);
                }
                None => {
                    let n = self
                        .stream
                        .read(&mut chunk)
                        .map_err(|e| MachineError::Transport(format!("socket read failed: {e}")))?;
                    if n == 0 {
                        return Err(MachineError::Transport(
                            "server closed the session (fatal server-side failure)".into(),
                        ));
                    }
                    self.inbuf.extend_from_slice(&chunk[..n]);
                }
            }
        }
    }
}

/// The client half of the caps exchange: same pins as the server (the negotiated
/// [`control_proto::APP_PROTOCOL_VERSION`], `EnvSpec` blobs only, no coverage
/// producer, no SDK).
pub fn client_caps() -> control_proto::Caps {
    control_proto::Caps {
        protocol_version: control_proto::APP_PROTOCOL_VERSION,
        env_version_min: EnvSpec::BLOB_VERSION,
        env_version_max: EnvSpec::BLOB_VERSION,
        coverage: control_proto::CoverageGeometry {
            map_bytes: 0,
            producer: 0,
        },
        flags: control_proto::CapFlags::NONE,
    }
}

/// Map a wire [`control_proto::ControlError`] to the seam's [`MachineError`],
/// preserving the two result categories (an error reply is never a
/// [`StopReason`]).
fn control_error_to_machine(err: control_proto::ControlError) -> MachineError {
    use control_proto::ControlError as Ce;
    match err {
        Ce::UnknownSnapshot(id) => MachineError::UnknownSnapshot(id.0),
        Ce::NotQuiescent | Ce::SnapshotWhileArmed => MachineError::NotQuiescent,
        Ce::BadEnvVersion(v) => MachineError::BadEnvironment(v),
        Ce::MalformedEnvironment => MachineError::BadEnvironment(EnvSpec::BLOB_VERSION),
        other => MachineError::Transport(format!("control error: {other}")),
    }
}

/// Convert a wire stop into the explorer's [`StopReason`]. A crash's opaque
/// `info` is the kind byte (0 panic / 1 triple-fault / 2 shutdown) followed by
/// the wire detail, so distinct crash kinds fingerprint differently.
fn stop_from_wire(stop: control_proto::StopReason) -> StopReason {
    use control_proto::StopReason as Ws;
    match stop {
        Ws::Deadline { vtime } => StopReason::Deadline {
            vtime: VTime(vtime.0),
        },
        Ws::Quiescent { vtime } => StopReason::Quiescent {
            vtime: VTime(vtime.0),
        },
        Ws::Crash { vtime, info } => {
            let kind = match info.kind {
                control_proto::CrashKind::Panic => 0u8,
                control_proto::CrashKind::TripleFault => 1,
                control_proto::CrashKind::Shutdown => 2,
            };
            let mut bytes = Vec::with_capacity(1 + info.detail.len());
            bytes.push(kind);
            bytes.extend_from_slice(&info.detail);
            StopReason::Crash {
                vtime: VTime(vtime.0),
                info: bytes,
            }
        }
        Ws::Decision { vtime, id, ctx } => StopReason::Decision {
            vtime: VTime(vtime.0),
            id: id.0,
            ctx,
        },
        Ws::SnapshotPoint { vtime } => StopReason::SnapshotPoint {
            vtime: VTime(vtime.0),
        },
        Ws::Assertion { vtime, ev } => StopReason::Assertion {
            vtime: VTime(vtime.0),
            id: ev.id,
            data: ev.data,
        },
    }
}

impl<S: Read + Write> Machine for SocketMachine<S> {
    fn branch(&mut self, snap: SnapId, env: &Environment) -> Result<(), MachineError> {
        // Decode the adapter blob first (a caller error must not touch the
        // session), then ship only the inner EnvSpec bytes on the wire.
        let decoded = AdapterEnv::decode(env)?;
        let wire_env = control_proto::Environment {
            blob_version: EnvSpec::BLOB_VERSION,
            bytes: decoded.spec.encode(),
        };
        let Some(meta) = self.snaps.get(&snap.0) else {
            return Err(MachineError::UnknownSnapshot(snap.0));
        };
        let origin = meta.vtime;
        match self.call(&control_proto::Request::Branch {
            snap: control_proto::SnapId(snap.0),
            env: wire_env,
        })? {
            control_proto::Reply::Unit => {
                // The new Modulation: its overrides are keyed from the snapshot's
                // capture Moment (the blob's own base_offset is advisory — the
                // authoritative origin is where the branch actually restored to).
                self.current = recorded(&decoded.spec);
                self.branch_offset = origin;
                self.pos = origin;
                self.pending_decision = None;
                Ok(())
            }
            other => Err(MachineError::Transport(format!(
                "branch answered with an unexpected reply: {other:?}"
            ))),
        }
    }

    fn replay(&mut self, snap: SnapId) -> Result<(), MachineError> {
        let Some(meta) = self.snaps.get(&snap.0) else {
            return Err(MachineError::UnknownSnapshot(snap.0));
        };
        // Verbatim restore of the SAME Modulation: the recording reverts to what
        // was active at capture, keyed relative to the **branch origin at
        // capture** (`meta.branch_offset`) — NOT the snapshot's V-time. A
        // snapshot taken mid-timeline has `branch_offset < vtime`; restoring the
        // branch offset to `vtime` (the pre-fix bug) would leave `self.current`'s
        // keys relative to the true origin while `branch_offset` claimed the
        // snapshot point, so a post-replay `recorded_env` would advertise the
        // wrong `base_offset` — a silent mis-key once keys exist (dormant in v1:
        // no decisions ⇒ no keys). `pos` is the snapshot point (`vtime`).
        let (branch_offset, pos, spec) = (meta.branch_offset, meta.vtime, meta.spec.clone());
        match self.call(&control_proto::Request::Replay(control_proto::SnapId(
            snap.0,
        )))? {
            control_proto::Reply::Unit => {
                self.current = spec;
                self.branch_offset = branch_offset;
                self.pos = pos;
                self.pending_decision = None;
                Ok(())
            }
            other => Err(MachineError::Transport(format!(
                "replay answered with an unexpected reply: {other:?}"
            ))),
        }
    }

    /// Advance the server VM. `until.on` (the [`StopMask`](crate::StopMask)) is
    /// carried to the server verbatim, but in **v1 it selects nothing**: no
    /// decision class can surface (there is no cooperating SDK / reactive
    /// service yet — task 61), so a `run` only ever ends at the always-on
    /// terminal classes (crash / quiescence / deadline). The mask becomes
    /// live when a decision-surfacing guest exists; until then any mask value
    /// yields the same terminal-only behavior. `resolve` is likewise inert on
    /// the seed-driven server (it answers `ResolveWithoutDecision`), but the
    /// recording machinery below is exercised the moment task 61 surfaces a
    /// decision.
    fn run(
        &mut self,
        until: &StopConditions,
        resolve: Option<&Answer>,
    ) -> Result<StopReason, MachineError> {
        let req = control_proto::Request::Run {
            until: control_proto::StopConditions {
                deadline: until.deadline.map(|v| control_proto::VTime(v.0)),
                on: control_proto::StopMask(until.on.0),
            },
            resolve: resolve.map(|a| control_proto::Answer(a.0.clone())),
        };
        let reply = self.call(&req)?;
        let control_proto::Reply::Stop(stop) = reply else {
            return Err(MachineError::Transport(format!(
                "run answered with a non-stop reply: {reply:?}"
            )));
        };
        // Tail-completeness (task-93): the server accepted the resolve, so the
        // answered decision is stamped into the current Modulation's recording
        // at the Moment it surfaced (branch-local key). An answer that is not
        // a valid catalog Answer cannot be recorded faithfully — loud abort.
        // `pending_decision` is consumed **only** when a resolve is actually
        // applied: a `run(None)` issued while a decision is still outstanding
        // (a probe/continue between the Decision stop and its answer) must NOT
        // discard it, or the answering `run(resolve)` would record nothing and
        // `recorded_env` would emit a delta missing that decision's answer — a
        // reproducer that replays to a different hash. (Dormant under task-58
        // seed-driven runs, which never surface a decision; correct for the
        // task-61 reactive path this machinery exists for.)
        if let Some(answer) = resolve
            && let Some(at) = self.pending_decision.take()
        {
            let decoded = environment::Answer::decode(&answer.0).map_err(|e| {
                MachineError::Transport(format!(
                    "resolve accepted but the answer bytes are not a catalog Answer ({e:?}) — \
                     cannot record a tail-complete delta"
                ))
            })?;
            self.current.record(
                at.saturating_sub(self.branch_offset),
                Action::Guest(decoded),
            );
        }
        let stop = stop_from_wire(stop);
        self.pos = stop.vtime().0;
        if let StopReason::Decision { vtime, .. } = &stop {
            self.pending_decision = Some(vtime.0);
        }
        Ok(stop)
    }

    fn snapshot(&mut self) -> Result<SnapId, MachineError> {
        match self.call(&control_proto::Request::Snapshot)? {
            control_proto::Reply::SnapId(id) => {
                self.snaps.insert(
                    id.0,
                    SnapMeta {
                        vtime: self.pos,
                        branch_offset: self.branch_offset,
                        spec: self.current.clone(),
                    },
                );
                Ok(SnapId(id.0))
            }
            other => Err(MachineError::Transport(format!(
                "snapshot answered with an unexpected reply: {other:?}"
            ))),
        }
    }

    fn drop_snap(&mut self, snap: SnapId) -> Result<(), MachineError> {
        match self.call(&control_proto::Request::Drop(control_proto::SnapId(snap.0)))? {
            control_proto::Reply::Unit => {
                self.snaps.remove(&snap.0);
                Ok(())
            }
            other => Err(MachineError::Transport(format!(
                "drop answered with an unexpected reply: {other:?}"
            ))),
        }
    }

    fn hash(&mut self) -> Result<[u8; 32], MachineError> {
        match self.call(&control_proto::Request::Hash {
            scope: control_proto::HashScope::Whole,
        })? {
            control_proto::Reply::Hash(digest) => Ok(digest),
            other => Err(MachineError::Transport(format!(
                "hash answered with an unexpected reply: {other:?}"
            ))),
        }
    }

    fn coverage(&self) -> &[u8] {
        &self.coverage
    }

    fn recorded_env(&self) -> Result<Environment, MachineError> {
        // The tail-complete branch-local delta: the branched spec (normalized
        // to `Recorded`) plus every decision stamped since the branch, keyed
        // from the branch origin, with the capture position for later slicing.
        Ok(AdapterEnv {
            base_offset: self.branch_offset,
            pos: self.pos,
            spec: recorded(&self.current),
        }
        .encode())
    }
}

#[cfg(test)]
mod tests {
    //! Pure-logic adapter tests: the blob codec, the `EnvCodec` binding (incl.
    //! its ruling-mandated panics), the wire↔seam conversions, and the
    //! tail-completeness recording state machine (driven over a scripted
    //! reply stream). The live loopback (this adapter against the real vmm-core
    //! server) is `dissonance/conductor`'s integration suite.

    use std::collections::BTreeMap;
    use std::io::{Cursor, Read, Write};

    use environment::{Action, EnvSpec, FaultPolicy, HostFault};

    use super::{
        ADAPTER_BLOB_VERSION, AdapterEnv, SocketMachine, SpecEnvCodec, control_error_to_machine,
    };
    use crate::{
        Answer, EnvCodec, Environment, Machine, MachineError, StopConditions, StopMask, VTime,
    };

    fn spec_with_overrides(seed: u64, keys: &[u64]) -> EnvSpec {
        let mut overrides = BTreeMap::new();
        for &k in keys {
            overrides.insert(k, Action::Host(HostFault::InjectInterrupt { vector: 32 }));
        }
        EnvSpec::Recorded {
            seed,
            policy: FaultPolicy::none(),
            overrides,
            standing: Vec::new(),
        }
    }

    #[test]
    fn adapter_blob_round_trips() {
        let env = AdapterEnv {
            base_offset: 1234,
            pos: 5678,
            spec: spec_with_overrides(0xAB, &[10, 20]),
        };
        let encoded = env.encode();
        assert_eq!(encoded.blob_version, ADAPTER_BLOB_VERSION);
        assert_eq!(AdapterEnv::decode(&encoded).unwrap(), env);
    }

    #[test]
    fn adapter_blob_decode_is_total_on_junk() {
        for bytes in [
            vec![],
            vec![0xFF; 4],
            vec![0xFF; 21],
            vec![0xFF; 64],
            b"R2A1".to_vec(),
        ] {
            let env = Environment {
                blob_version: ADAPTER_BLOB_VERSION,
                bytes,
            };
            assert_eq!(
                AdapterEnv::decode(&env),
                Err(MachineError::BadEnvironment(ADAPTER_BLOB_VERSION))
            );
        }
        // Wrong wrapper version is rejected before anything is read.
        let good = AdapterEnv {
            base_offset: 0,
            pos: 0,
            spec: spec_with_overrides(1, &[]),
        }
        .encode();
        let wrong_version = Environment {
            blob_version: 99,
            bytes: good.bytes,
        };
        assert_eq!(
            AdapterEnv::decode(&wrong_version),
            Err(MachineError::BadEnvironment(99))
        );
    }

    #[test]
    fn seeded_mints_a_genesis_complete_recorded_variant() {
        let env = SpecEnvCodec.seeded(0xC0FFEE);
        let decoded = AdapterEnv::decode(&env).unwrap();
        assert_eq!(decoded.base_offset, 0, "genesis-complete");
        assert_eq!(decoded.pos, 0);
        assert_eq!(decoded.spec.seed(), 0xC0FFEE);
        assert!(
            matches!(decoded.spec, EnvSpec::Recorded { .. }),
            "always the Recorded variant, so compose's variant check cannot fire"
        );
        assert!(decoded.spec.overrides().is_empty());
    }

    #[test]
    fn compose_rekeys_the_delta_at_its_base_offset() {
        // base: genesis-complete with overrides below and above the snapshot
        // point; delta: branch-local (keyed from 100).
        let base = AdapterEnv {
            base_offset: 0,
            pos: 100,
            spec: spec_with_overrides(7, &[40, 150]),
        }
        .encode();
        let delta = AdapterEnv {
            base_offset: 100,
            pos: 260,
            spec: spec_with_overrides(7, &[5, 60]),
        }
        .encode();
        let composed = SpecEnvCodec.compose(&base, &delta);
        let decoded = AdapterEnv::decode(&composed).unwrap();
        assert_eq!(decoded.base_offset, 0, "composed is genesis-complete");
        assert_eq!(decoded.pos, 260, "pos is the delta's capture point");
        let keys: Vec<u64> = decoded.spec.overrides().keys().copied().collect();
        // base keeps only m < at (40; the 150 is superseded by the branch),
        // delta re-keys by +at (5→105, 60→160).
        assert_eq!(keys, vec![40, 105, 160]);
        assert_eq!(decoded.spec.seed(), 7);
    }

    #[test]
    #[should_panic(expected = "SpecEnvCodec::compose failed")]
    fn compose_panics_on_a_seed_mismatch_per_the_ruling() {
        let base = AdapterEnv {
            base_offset: 0,
            pos: 100,
            spec: spec_with_overrides(7, &[]),
        }
        .encode();
        let delta = AdapterEnv {
            base_offset: 100,
            pos: 200,
            spec: spec_with_overrides(8, &[]), // different seed
        }
        .encode();
        let _ = SpecEnvCodec.compose(&base, &delta);
    }

    #[test]
    #[should_panic(expected = "SpecEnvCodec::compose failed")]
    fn compose_panics_on_a_rekey_overflow_per_the_ruling() {
        let base = AdapterEnv {
            base_offset: 0,
            pos: u64::MAX - 1,
            spec: spec_with_overrides(7, &[]),
        }
        .encode();
        let delta = AdapterEnv {
            base_offset: u64::MAX - 1,
            pos: u64::MAX,
            spec: spec_with_overrides(7, &[10]), // 10 + (MAX-1) overflows
        }
        .encode();
        let _ = SpecEnvCodec.compose(&base, &delta);
    }

    #[test]
    #[should_panic(expected = "not an adapter-minted blob")]
    fn codec_seams_panic_on_a_non_adapter_blob() {
        let junk = Environment {
            blob_version: ADAPTER_BLOB_VERSION,
            bytes: vec![1, 2, 3],
        };
        let _ = SpecEnvCodec.compose(&junk, &junk);
    }

    // The task-68 coordinate system: `mutate`/`compose` operate in the base's
    // OWN coordinate system (keys relative to its `base_offset`), so a
    // parent-rooted chain folds correctly — and a mis-ordered chain (a delta
    // keyed before its base's root, or a capture behind the root) is a defect
    // that panics rather than silently mis-keys (task-93 discipline).

    #[test]
    fn compose_folds_a_parent_rooted_chain_at_the_relative_cut() {
        // suffix₁: rooted at 100 (keys relative to it), captured at 250.
        let base = AdapterEnv {
            base_offset: 100,
            pos: 250,
            spec: spec_with_overrides(7, &[20, 180]),
        }
        .encode();
        // suffix₂: branched at 250, captured at 400.
        let delta = AdapterEnv {
            base_offset: 250,
            pos: 400,
            spec: spec_with_overrides(7, &[5, 60]),
        }
        .encode();
        let folded = SpecEnvCodec.compose(&base, &delta);
        let decoded = AdapterEnv::decode(&folded).unwrap();
        assert_eq!(
            decoded.base_offset, 100,
            "the fold stays rooted at the base's own origin (the retained ancestor)"
        );
        assert_eq!(decoded.pos, 400, "pos is the delta's capture point");
        // Relative cut = 250 − 100 = 150: base keeps keys < 150 (20; the 180
        // is superseded by the branch), delta re-keys by +150 (5→155, 60→210).
        let keys: Vec<u64> = decoded.spec.overrides().keys().copied().collect();
        assert_eq!(keys, vec![20, 155, 210]);
        assert_eq!(decoded.spec.seed(), 7);
    }

    #[test]
    fn compose_then_compose_folds_a_two_hop_chain_onto_genesis() {
        // A genesis-complete base + two chain suffixes: fold(fold(b, s1), s2)
        // must equal splicing each at its own relative cut — the exact shape
        // the materialization engine replays from genesis in the worst case.
        let b = AdapterEnv {
            base_offset: 0,
            pos: 100,
            spec: spec_with_overrides(7, &[40]),
        }
        .encode();
        let s1 = AdapterEnv {
            base_offset: 100,
            pos: 250,
            spec: spec_with_overrides(7, &[20]),
        }
        .encode();
        let s2 = AdapterEnv {
            base_offset: 250,
            pos: 300,
            spec: spec_with_overrides(7, &[10]),
        }
        .encode();
        let folded = SpecEnvCodec.compose(&SpecEnvCodec.compose(&b, &s1), &s2);
        let decoded = AdapterEnv::decode(&folded).unwrap();
        assert_eq!(decoded.base_offset, 0, "genesis-complete");
        assert_eq!(decoded.pos, 300);
        let keys: Vec<u64> = decoded.spec.overrides().keys().copied().collect();
        assert_eq!(keys, vec![40, 120, 260], "each suffix keyed at its own cut");
    }

    #[test]
    fn mutate_slices_a_parent_rooted_base_at_the_relative_cut() {
        // Guest overrides are preserved verbatim by the underlying codec's
        // contract (only a host-plane tweak is applied), so the slice is
        // exactly assertable. Rooted at 100, captured at 160 → relative cut
        // 60: the pre-capture override (5) is dropped, the suffix re-keys
        // (70→10, 90→30).
        let mut overrides = BTreeMap::new();
        for k in [5u64, 70, 90] {
            overrides.insert(k, Action::Guest(environment::Answer::Nominal));
        }
        let base = AdapterEnv {
            base_offset: 100,
            pos: 160,
            spec: EnvSpec::Recorded {
                seed: 7,
                policy: FaultPolicy::none(),
                overrides,
                standing: Vec::new(),
            },
        }
        .encode();
        let out = SpecEnvCodec.mutate(&base, 0x5A17);
        let decoded = AdapterEnv::decode(&out).unwrap();
        assert_eq!(
            decoded.base_offset, 160,
            "keyed from the base's (absolute) capture point"
        );
        assert_eq!(
            decoded.spec.seed(),
            7,
            "seed preserved (compose-consistent)"
        );
        let guest_keys: Vec<u64> = decoded
            .spec
            .overrides()
            .iter()
            .filter(|(_, a)| a.guest_answer().is_some())
            .map(|(k, _)| *k)
            .collect();
        assert_eq!(
            guest_keys,
            vec![10, 30],
            "the suffix sliced at the RELATIVE cut (60), not the absolute pos"
        );
        // Deterministic: same (base, salt) ⇒ same blob.
        assert_eq!(out, SpecEnvCodec.mutate(&base, 0x5A17));
    }

    #[test]
    #[should_panic(expected = "mis-ordered chain")]
    fn compose_panics_on_a_mis_ordered_chain() {
        let base = AdapterEnv {
            base_offset: 200,
            pos: 300,
            spec: spec_with_overrides(7, &[]),
        }
        .encode();
        // A delta keyed BEFORE the base's root: not a suffix of it.
        let delta = AdapterEnv {
            base_offset: 100,
            pos: 250,
            spec: spec_with_overrides(7, &[]),
        }
        .encode();
        let _ = SpecEnvCodec.compose(&base, &delta);
    }

    #[test]
    #[should_panic(expected = "BEFORE its own root offset")]
    fn mutate_panics_on_a_capture_behind_the_root() {
        let base = AdapterEnv {
            base_offset: 200,
            pos: 100, // malformed: captured before its own root
            spec: spec_with_overrides(7, &[]),
        }
        .encode();
        let _ = SpecEnvCodec.mutate(&base, 0x1);
    }

    #[test]
    fn mutate_slices_a_corpus_base_into_a_branch_local_delta() {
        // Overrides at 40 (before the snapshot) and 150/220 (after): the
        // delta keeps only the suffix, re-keyed from pos=100.
        let base = AdapterEnv {
            base_offset: 0,
            pos: 100,
            spec: spec_with_overrides(7, &[40, 150, 220]),
        }
        .encode();
        let out = SpecEnvCodec.mutate(&base, 0x5A17);
        let decoded = AdapterEnv::decode(&out).unwrap();
        assert_eq!(
            decoded.base_offset, 100,
            "keyed from the base's snapshot point"
        );
        assert_eq!(
            decoded.spec.seed(),
            7,
            "seed preserved (compose-consistent)"
        );
        assert_eq!(decoded.spec.policy(), &FaultPolicy::none());
        // The pre-snapshot override (40) is gone; the suffix (150→50, 220→120)
        // survives modulo the codec's single host-plane tweak.
        assert!(!decoded.spec.overrides().contains_key(&40));
        // Deterministic: same (base, salt) ⇒ same blob.
        assert_eq!(out, SpecEnvCodec.mutate(&base, 0x5A17));
        assert_ne!(
            out,
            SpecEnvCodec.mutate(&base, 0x5A18),
            "salt selects the tweak"
        );
    }

    #[test]
    fn control_errors_map_onto_the_machine_error_categories() {
        use control_proto::ControlError as Ce;
        assert_eq!(
            control_error_to_machine(Ce::UnknownSnapshot(control_proto::SnapId(9))),
            MachineError::UnknownSnapshot(9)
        );
        assert_eq!(
            control_error_to_machine(Ce::NotQuiescent),
            MachineError::NotQuiescent
        );
        assert_eq!(
            control_error_to_machine(Ce::SnapshotWhileArmed),
            MachineError::NotQuiescent
        );
        assert_eq!(
            control_error_to_machine(Ce::BadEnvVersion(9)),
            MachineError::BadEnvironment(9)
        );
        assert_eq!(
            control_error_to_machine(Ce::MalformedEnvironment),
            MachineError::BadEnvironment(EnvSpec::BLOB_VERSION)
        );
        for err in [
            Ce::RestoreFailed,
            Ce::ResolveWithoutDecision,
            Ce::MalformedAnswer,
            Ce::Unsupported,
            Ce::Protocol(control_proto::ProtocolError::ShortFrame),
        ] {
            assert!(
                matches!(control_error_to_machine(err), MachineError::Transport(_)),
                "non-recoverable control errors are transport failures"
            );
        }
    }

    #[test]
    fn crash_info_keeps_kinds_distinguishable() {
        use control_proto::{CrashInfo, CrashKind, StopReason as Ws, VTime as WsVTime};
        let stop = |kind| {
            super::stop_from_wire(Ws::Crash {
                vtime: WsVTime(5),
                info: CrashInfo {
                    kind,
                    detail: vec![0xEE],
                },
            })
        };
        let (a, b) = (stop(CrashKind::Panic), stop(CrashKind::Shutdown));
        match (&a, &b) {
            (
                crate::StopReason::Crash { info: ia, .. },
                crate::StopReason::Crash { info: ib, .. },
            ) => {
                assert_ne!(ia, ib, "the kind byte reaches the opaque info");
                assert_eq!(ia[1..], [0xEE], "the wire detail follows the kind byte");
            }
            other => panic!("expected two crashes, got {other:?}"),
        }
    }

    // ---- tail-completeness recording over a scripted reply stream ----------

    /// A duplex stream that replays pre-encoded reply frames to the client and
    /// swallows the client's requests — enough to drive `SocketMachine`'s
    /// request/reply state machine without a real server.
    struct ScriptedStream {
        replies: Cursor<Vec<u8>>,
    }

    impl Read for ScriptedStream {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.replies.read(buf)
        }
    }

    impl Write for ScriptedStream {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            Ok(buf.len()) // discard requests
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Build a scripted stream from a sequence of `(seq, reply)` frames.
    fn scripted(frames: &[(u32, control_proto::Reply)]) -> ScriptedStream {
        let mut bytes = Vec::new();
        for (seq, reply) in frames {
            control_proto::encode_reply(*seq, &Ok(reply.clone()), &mut bytes).unwrap();
        }
        ScriptedStream {
            replies: Cursor::new(bytes),
        }
    }

    fn server_caps_reply() -> control_proto::Reply {
        server_caps_reply_geo(0, 0)
    }

    /// A `Hello` reply with a chosen coverage geometry (for the guard test).
    fn server_caps_reply_geo(map_bytes: u32, producer: u8) -> control_proto::Reply {
        control_proto::Reply::Hello(control_proto::Caps {
            protocol_version: control_proto::APP_PROTOCOL_VERSION,
            env_version_min: EnvSpec::BLOB_VERSION,
            env_version_max: EnvSpec::BLOB_VERSION,
            coverage: control_proto::CoverageGeometry {
                map_bytes,
                producer,
            },
            flags: control_proto::CapFlags::NONE,
        })
    }

    /// The connect-time V-time probe reply: a `run(deadline:0)` stops immediately
    /// with `Deadline{vtime}`.
    fn probe_reply(vtime: u64) -> control_proto::Reply {
        control_proto::Reply::Stop(control_proto::StopReason::Deadline {
            vtime: control_proto::VTime(vtime),
        })
    }

    fn seeded_env(seed: u64) -> EnvSpec {
        EnvSpec::Seeded {
            seed,
            policy: FaultPolicy::none(),
        }
    }

    /// The snapshot-origin fix (blocking, task-93 class): `connect` probes the
    /// server's current V-time, so a snapshot taken *immediately* (as
    /// `Explorer::new` does, before any `run`) records the true origin — not
    /// `0`. Against a server whose live VM sits post-readiness (here V-time
    /// 5000), a `branch` off that genesis snapshot then keys its `recorded_env`
    /// delta from 5000, not 0 — otherwise the reproducer would be silently
    /// mis-keyed (undetectable by `compose`).
    #[test]
    fn snapshot_immediately_after_connect_records_the_true_origin_not_zero() {
        use control_proto::{Reply, SnapId as WsSnapId};
        let stream = scripted(&[
            (1, server_caps_reply()),        // hello
            (2, probe_reply(5000)),          // connect's V-time probe: post-readiness
            (3, Reply::SnapId(WsSnapId(1))), // snapshot (taken immediately, no run)
            (4, Reply::Unit),                // branch
        ]);
        let mut m = SocketMachine::connect(stream, seeded_env(7)).unwrap();
        let snap = m.snapshot().unwrap();
        m.branch(snap, &SpecEnvCodec.seeded(7)).unwrap();
        let recorded = AdapterEnv::decode(&m.recorded_env().unwrap()).unwrap();
        assert_eq!(
            recorded.base_offset, 5000,
            "the branch-local delta is keyed from the true post-readiness origin, not 0"
        );
        assert_eq!(
            recorded.pos, 5000,
            "pos is the branch origin before any run"
        );
    }

    /// The replay-origin fix (latent mis-key, task-58/61): `replay(snap)`
    /// restores the branch origin that was active **at capture**, not the
    /// snapshot's V-time. A snapshot taken mid-timeline (`branch_offset <
    /// vtime`) then replays with the correct origin, so `recorded_env`
    /// advertises the true `base_offset` — otherwise a post-replay delta would
    /// be mis-keyed once decisions exist (dormant in v1).
    #[test]
    fn replay_restores_the_branch_origin_at_capture_not_the_snapshot_vtime() {
        use control_proto::{Reply, SnapId as WsSnapId, StopReason as Ws, VTime as WsVTime};
        let stream = scripted(&[
            (1, server_caps_reply()),        // hello
            (2, probe_reply(50)),            // connect probe → origin 50
            (3, Reply::SnapId(WsSnapId(1))), // snapshot S1 @ 50 (branch base)
            (4, Reply::Unit),                // branch(S1, seeded) → timeline rooted at 50
            (
                5,
                Reply::Stop(Ws::Deadline {
                    vtime: WsVTime(200),
                }),
            ), // run → advance pos to 200 (branch_offset stays 50)
            (6, Reply::SnapId(WsSnapId(2))), // snapshot S2 @ vtime 200, branch_offset 50
            (7, Reply::Unit),                // replay(S2)
        ]);
        let mut m = SocketMachine::connect(stream, seeded_env(7)).unwrap();
        let s1 = m.snapshot().unwrap();
        m.branch(s1, &SpecEnvCodec.seeded(7)).unwrap();
        let _ = m
            .run(
                &StopConditions {
                    deadline: Some(VTime(200)),
                    on: StopMask::NONE,
                },
                None,
            )
            .unwrap();
        let s2 = m.snapshot().unwrap();
        m.replay(s2).unwrap();
        let recorded = AdapterEnv::decode(&m.recorded_env().unwrap()).unwrap();
        assert_eq!(
            recorded.base_offset, 50,
            "replay restores the branch origin at capture (50), not the snapshot V-time (200)"
        );
        assert_eq!(recorded.pos, 200, "pos is the snapshot point");
    }

    /// The coverage-geometry guard (blocking): a server advertising a non-zero
    /// coverage geometry is refused (v1 has no producer) rather than sized into
    /// an unbounded allocation from the transport-provided `u32`.
    #[test]
    fn connect_refuses_a_non_zero_coverage_geometry() {
        // A huge map_bytes is exactly the unbounded-allocation risk.
        let stream = scripted(&[(1, server_caps_reply_geo(u32::MAX, 0))]);
        assert!(matches!(
            SocketMachine::connect(stream, seeded_env(1)),
            Err(MachineError::Transport(_))
        ));
        // A non-zero producer tag (with zero map_bytes) is likewise unexpected in v1.
        let stream = scripted(&[(1, server_caps_reply_geo(0, 3))]);
        assert!(matches!(
            SocketMachine::connect(stream, seeded_env(1)),
            Err(MachineError::Transport(_))
        ));
        // Zero-width geometry is accepted (the negotiated v1 shape); the probe
        // then completes the handshake.
        let stream = scripted(&[(1, server_caps_reply()), (2, probe_reply(0))]);
        assert!(SocketMachine::connect(stream, seeded_env(1)).is_ok());
    }

    /// The task-93 tail-completeness fix: a `run(None)` issued **while a
    /// decision is still outstanding** (a probe/continue between the `Decision`
    /// stop and its answer) must NOT discard the pending decision, so the later
    /// answering `run(resolve)` still records it. (Dormant under seed-driven
    /// task 58, which never surfaces a decision; correct for the task-61
    /// reactive path this recording machinery exists for.)
    #[test]
    fn a_none_resolve_between_a_decision_and_its_answer_keeps_the_pending_decision() {
        use control_proto::{DecisionId, Reply, StopReason as Ws, VTime as WsVTime};
        let stream = scripted(&[
            (1, server_caps_reply()), // hello
            (2, probe_reply(0)),      // connect's V-time probe (origin 0)
            (
                3,
                Reply::Stop(Ws::Decision {
                    vtime: WsVTime(100),
                    id: DecisionId(5),
                    ctx: vec![],
                }),
            ), // run #1
            (
                4,
                Reply::Stop(Ws::SnapshotPoint {
                    vtime: WsVTime(110),
                }),
            ), // run #2: the None-resolve probe
            (
                5,
                Reply::Stop(Ws::Quiescent {
                    vtime: WsVTime(120),
                }),
            ), // run #3: the answering resolve
        ]);
        let mut m = SocketMachine::connect(stream, seeded_env(7)).unwrap();
        let until = StopConditions {
            deadline: None,
            on: StopMask::NONE,
        };
        // run #1 surfaces the decision at Moment 100.
        assert!(matches!(
            m.run(&until, None).unwrap(),
            crate::StopReason::Decision { .. }
        ));
        // run #2 is a probe with no resolve — the pending decision must survive.
        assert!(matches!(
            m.run(&until, None).unwrap(),
            crate::StopReason::SnapshotPoint { .. }
        ));
        // run #3 answers it; the answer is recorded tail-completely at Moment 100.
        let answer = Answer(environment::Answer::Nominal.encode());
        assert!(matches!(
            m.run(&until, Some(&answer)).unwrap(),
            crate::StopReason::Quiescent { .. }
        ));
        let recorded = AdapterEnv::decode(&m.recorded_env().unwrap()).unwrap();
        let keys: Vec<u64> = recorded.spec.overrides().keys().copied().collect();
        assert_eq!(
            keys,
            vec![100],
            "the decision answered after an intervening run(None) is still recorded (branch \
             offset 0, so the absolute Moment 100)"
        );
        assert!(matches!(
            recorded.spec.overrides().get(&100),
            Some(Action::Guest(environment::Answer::Nominal))
        ));
    }
}
