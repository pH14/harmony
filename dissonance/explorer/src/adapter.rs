// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **R2 socket adapter** (task 58): the first non-toy [`Machine`] — an
//! implementation of the driver seam over a `control-proto` client stream —
//! plus [`SpecEnvCodec`], the binding of the [`EnvCodec`] proposal seam to
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
//!   the offset must ride the blob for [`EnvCodec::compose`] to recover `at`
//!   from the delta alone (the production analogue of the toy blob's
//!   `base_offset`).
//! - **`pos`** — the absolute `Moment` the blob was captured at (a corpus
//!   base records where its snapshot was taken, so a later
//!   [`mutate`](EnvCodec::mutate) can slice the suffix at the right offset —
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
//!
//! ## Error mapping (two categories, preserved)
//!
//! A wire [`ControlError`] maps onto [`MachineError`] without ever crossing
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
        // A corpus base is genesis-complete; the branch it seeds runs from the
        // base snapshot's capture point, so slice the suffix at `pos` into a
        // branch-local delta (keys re-based to the branch origin), preserving
        // seed/policy so a later genesis recompose is stream-consistent.
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
            .filter(|(m, _)| **m >= b.pos)
            .map(|(m, a)| (m - b.pos, a.clone()))
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
        // The ruling's "`at` provenance": the delta carries the absolute
        // Moment it is keyed from, so `at` is recoverable from the delta alone.
        let at = d.base_offset;
        let composed = match environment::EnvCodec::compose(&b.spec, &d.spec, at) {
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

/// Per-snapshot client bookkeeping: the absolute `Moment` the snapshot was
/// captured at, and the (branch-local) spec that was active — so `replay`
/// restores the adapter's recording state along with the VM, and a later
/// branch below the snapshot keys its delta from the right offset.
#[derive(Clone, Debug)]
struct SnapMeta {
    vtime: u64,
    spec: EnvSpec,
}

/// The socket-backed [`Machine`]: every verb is one request/reply exchange on
/// a `control-proto` stream (a connected unix socket or an in-process
/// socketpair end). See the module doc for the blob format, the `Moment` axis,
/// and the error mapping.
///
/// The adapter owns the recording side of the task-93 contract: it tracks the
/// env each Timeline was branched with, stamps every resolved decision at its
/// stop `Moment`, and emits the tail-complete branch-local delta from
/// [`recorded_env`](Machine::recorded_env).
pub struct SocketMachine<S: Read + Write> {
    stream: S,
    seq: u32,
    inbuf: Vec<u8>,
    /// The coverage view for the most recent run. The negotiated geometry is
    /// zero-width (no producer exists), so this is empty and never updated.
    coverage: Vec<u8>,
    snaps: BTreeMap<u64, SnapMeta>,
    /// The (branch-local) spec the current Timeline runs under, plus every
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
    /// The coverage buffer is sized from the negotiated geometry — zero-width
    /// until a coverage producer exists.
    pub fn connect(stream: S, initial: EnvSpec) -> Result<Self, MachineError> {
        let mut machine = SocketMachine {
            stream,
            seq: 0,
            inbuf: Vec::new(),
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
        if caps.protocol_version != 1 {
            return Err(MachineError::Transport(format!(
                "incompatible control protocol version {} (need 1)",
                caps.protocol_version
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
        machine.coverage = vec![0; caps.coverage.map_bytes as usize];
        Ok(machine)
    }

    /// One request/reply exchange. A transport failure, a mismatched reply
    /// sequence number, or an error reply all surface as [`MachineError`].
    fn call(&mut self, req: &control_proto::Request) -> Result<control_proto::Reply, MachineError> {
        self.seq = self.seq.wrapping_add(1);
        let mut out = Vec::new();
        control_proto::encode_request(self.seq, req, &mut out)
            .map_err(|e| MachineError::Transport(format!("request encode failed: {e}")))?;
        self.stream
            .write_all(&out)
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

/// The client half of the caps exchange: same pins as the server (protocol 1,
/// `EnvSpec` blobs only, no coverage producer, no SDK).
pub fn client_caps() -> control_proto::Caps {
    control_proto::Caps {
        protocol_version: 1,
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
                // The new Timeline: its overrides are keyed from the snapshot's
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
        let (origin, spec) = (meta.vtime, meta.spec.clone());
        match self.call(&control_proto::Request::Replay(control_proto::SnapId(
            snap.0,
        )))? {
            control_proto::Reply::Unit => {
                // Verbatim: the recording state reverts to what was active
                // when the snapshot was taken.
                self.current = spec;
                self.branch_offset = origin;
                self.pos = origin;
                self.pending_decision = None;
                Ok(())
            }
            other => Err(MachineError::Transport(format!(
                "replay answered with an unexpected reply: {other:?}"
            ))),
        }
    }

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
        // answered decision is stamped into the current Timeline's recording
        // at the Moment it surfaced (branch-local key). An answer that is not
        // a valid catalog Answer cannot be recorded faithfully — loud abort.
        if let (Some(answer), Some(at)) = (resolve, self.pending_decision.take()) {
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
    //! its ruling-mandated panics), and the wire↔seam conversions, driven with
    //! no server. The live loopback (this adapter against the vmm-core server)
    //! is `dissonance/conductor`'s integration suite.

    use std::collections::BTreeMap;

    use environment::{Action, EnvSpec, FaultPolicy, HostFault};

    use super::{ADAPTER_BLOB_VERSION, AdapterEnv, SpecEnvCodec, control_error_to_machine};
    use crate::{EnvCodec, Environment, MachineError};

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
}
