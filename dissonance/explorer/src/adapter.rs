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
//! ## Coordinate frames (AUTHORITATIVE — the one place this is settled)
//!
//! Two frames exist, and exactly one code point converts between them:
//!
//! - **The blob frame** (`R2A1` / this adapter / every [`EnvCodec`](crate::EnvCodec)
//!   seam): an [`AdapterEnv`]'s override keys are **relative** to its
//!   `base_offset` — the origin of the blob's frame. `seeded` mints at origin
//!   0; `mutate` slices and `compose` splices **within** this frame (the
//!   relative cut, task 68), so a fold of lineage suffixes stays rooted at
//!   its base's origin; [`Machine::recorded_env`] records into it (an answer
//!   at absolute stop `m` is stamped at `m − branch_offset`).
//! - **The wire frame** (`control-proto` / `ControlServer`): an `EnvSpec`'s
//!   override `Moment`s are **absolute** on the deterministic axis — the
//!   task-59 contract: a branch env's host faults are validated against the
//!   restored snapshot's floor and applied at `vns == Moment`. The server
//!   knows nothing of blob origins.
//!
//! **The single conversion point is [`Machine::branch`]** (`SocketMachine`):
//! outbound, it re-anchors the blob-frame keys at the **actual restore
//! origin** — the branched snapshot's capture moment — shipping
//! `origin + relative` (checked; overflow is a malformed blob, refused before
//! any wire traffic). The blob's own `base_offset` names its frame for
//! provenance/compose; the anchor at branch time is authoritative (a
//! genesis-complete env branched off a mid-run seal re-anchors there, exactly
//! like its overrides' decision-index semantics). `recorded_env` is the
//! inverse direction (absolute stop → relative key), and **no other code
//! converts frames**. Two deliberate edges: the session-initial spec handed
//! to [`SocketMachine::connect`] must be override-free (v1 boots are — a
//! boot-time override's frame would be ambiguous), and standing faults ride
//! the wire unconverted (v1 rejects them server-side as `Unsupported`; their
//! window axis is unsettled until a `Moment → VTime` map exists, per the
//! task-93 ruling).
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
//! - **Fallibility (task 99, bead `hm-5d9`).** [`SpecEnvCodec`]'s `compose` and
//!   `mutate` are **fallible**: a malformed adapter blob, a mis-ordered chain,
//!   an `UnsupportedComposition`/`Overflow`, or a standing-fault-carrying base
//!   returns a typed [`EnvCodecError`] — **never a
//!   panic**. A serialized reproducer is the artifact users pass around, load
//!   from disk, and feed back in, so it is untrusted by definition and the
//!   library rule (conventions rule 4: never panic on untrusted input) governs
//!   it. The failure is still a **loud control error**: the engine and campaign
//!   loops surface it via [`MachineError::EnvCodec`],
//!   which aborts the step and is never recorded as a guest [`Bug`](crate::Bug)
//!   — a bad reproducer artifact fails the run/campaign, it does not mint a
//!   finding. (This supersedes the earlier task-93 default, which panicked; the
//!   ruling named a fallible seam as the allowed adjustment, and it is now
//!   taken.)
//! - **Standing-fault confinement** is vacuous in the v1 vocabulary (no
//!   standing faults exist); `mutate` still refuses a standing-fault-carrying
//!   base (with `EnvCodecError::UnsupportedComposition`) rather than slicing one
//!   into a branch-local delta, so the confinement rule is enforced here the day
//!   they appear.
//! - **Parent-rooted chains (task 68).** `mutate`/`compose` operate in the
//!   base's **own coordinate system**: a base's override keys are relative to
//!   its `base_offset` (absolute keys are the `base_offset == 0` special
//!   case). `compose` splices at the **relative** cut
//!   `d.base_offset − b.base_offset` and keeps the base's root, so folding a
//!   suffix chain (`compose(suffixᵢ, suffixᵢ₊₁)` down a lineage) yields one
//!   delta still rooted at the chain's retained ancestor — exactly what the
//!   task-68 materialization engine replays with **one** branch. `mutate`
//!   slices at `b.pos − b.base_offset`. The `compose` operand-pair contract is
//!   total and enumerated on the [`EnvCodecError`](crate::EnvCodecError) doc; the
//!   two positional invariants are: each operand's **own** `pos ≥ base_offset`
//!   (checked once at the codec-seam decode, `require`, so `mutate` and both
//!   `compose` operands reject an internally-inconsistent blob with the same
//!   `MisorderedChain`), and **adjacency** `d.base_offset == b.pos` (the delta
//!   was recorded off the base's snapshot). Adjacency implies root ordering, so
//!   the relative cut `d.base_offset − b.base_offset` reduces to `b.pos −
//!   b.base_offset` and cannot underflow; a gap or overlap is refused with
//!   `EnvCodecError::NonAdjacentChain` (never silently mis-keyed).
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

use crate::error::{EnvCodecError, MachineError};
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

/// Convert a blob-frame spec (override keys **relative** to the blob's
/// origin) to the wire frame (**absolute** `Moment`s) by re-anchoring at
/// `origin` — the branched snapshot's capture moment. The single outbound
/// frame conversion (module doc, "Coordinate frames"); its inverse is
/// `recorded_env`'s `m − branch_offset` stamping. A key that overflows the
/// axis is a malformed blob: refused with [`MachineError::BadEnvironment`]
/// **before** any wire traffic. Standing faults are carried unconverted (v1
/// rejects them server-side; their window axis is unsettled — module doc).
fn rebase_to_wire(spec: &EnvSpec, origin: u64) -> Result<EnvSpec, MachineError> {
    match spec {
        EnvSpec::Seeded { .. } => Ok(spec.clone()),
        EnvSpec::Recorded {
            seed,
            policy,
            overrides,
            standing,
            reseeds,
        } => {
            let mut absolute = BTreeMap::new();
            for (rel, action) in overrides {
                let at = origin
                    .checked_add(*rel)
                    .ok_or(MachineError::BadEnvironment(ADAPTER_BLOB_VERSION))?;
                absolute.insert(at, action.clone());
            }
            // Reseed markers re-anchor exactly like overrides (task 78): the
            // blob-frame key is relative to the blob's origin, the server's
            // contract is absolute Moments.
            let mut absolute_reseeds = BTreeMap::new();
            for (rel, s) in reseeds {
                let at = origin
                    .checked_add(*rel)
                    .ok_or(MachineError::BadEnvironment(ADAPTER_BLOB_VERSION))?;
                absolute_reseeds.insert(at, *s);
            }
            Ok(EnvSpec::Recorded {
                seed: *seed,
                policy: policy.clone(),
                overrides: absolute,
                standing: standing.clone(),
                reseeds: absolute_reseeds,
            })
        }
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
            reseeds: std::collections::BTreeMap::new(),
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
/// **Fallible** (task 99): `mutate` and `compose` decode untrusted serialized
/// reproducers and return a typed [`EnvCodecError`] — never a panic. `compose`'s
/// acceptance contract is total and enumerated on the [`EnvCodecError`] doc
/// (byte well-formedness, per-operand `pos >= base_offset`, pair adjacency,
/// spec compatibility, no `Moment`-axis overflow). `seeded` mints from a
/// caller-supplied seed and is infallible. See the module doc's contract section
/// for how callers surface the error as a loud control failure.
#[derive(Clone, Copy, Debug, Default)]
pub struct SpecEnvCodec;

impl SpecEnvCodec {
    /// Decode a reproducer blob at the codec seam and validate its internal
    /// invariants. The blob is untrusted input (a user-supplied artifact), so a
    /// byte-level decode failure is a typed [`EnvCodecError::Malformed`] carrying
    /// the declared version, and a structurally-decodable but semantically
    /// impossible lineage is [`EnvCodecError::MisorderedChain`] — never a panic
    /// (task 99).
    ///
    /// The one internal invariant a lone blob can violate is `pos >= base_offset`:
    /// a capture position **before** the blob's own root would mean a snapshot
    /// taken before the branch it is keyed from. Enforcing it here — the single
    /// codec-seam decode point — means `mutate` and **both** `compose` operands
    /// are guarded uniformly, so neither can splice an operand whose capture
    /// precedes its splice into an inconsistent artifact (round-3 finding).
    fn require(env: &Environment) -> Result<AdapterEnv, EnvCodecError> {
        let decoded =
            AdapterEnv::decode(env).map_err(|_| EnvCodecError::Malformed(env.blob_version))?;
        if decoded.pos < decoded.base_offset {
            return Err(EnvCodecError::MisorderedChain(
                "capture position precedes the blob's own root offset",
            ));
        }
        Ok(decoded)
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

    fn mutate(&self, base: &Environment, salt: u64) -> Result<Environment, EnvCodecError> {
        let b = Self::require(base)?;
        // Coordinate system (task 68): the base's override keys are relative
        // to its own `base_offset`, so the slice point is the **relative**
        // distance from the base's root to its capture position. `require`
        // already refused a capture behind the root (`pos < base_offset` is a
        // `MisorderedChain`), so this subtraction cannot underflow.
        let cut = b.pos - b.base_offset;
        // The branch this delta seeds runs from the base snapshot's capture
        // point, so slice the suffix at `cut` into a branch-local delta (keys
        // re-based to the branch origin), preserving seed/policy so a later
        // recompose is stream-consistent.
        //
        // NOTE: the underlying `environment::EnvCodec::mutate` inserts a
        // **host-plane** `Action::Host` override (at a blob-frame, relative
        // key), so a mutate-minted env is `Recorded` with an override. Task 59
        // landed host-plane enforcement server-side, and `branch`'s wire-frame
        // conversion (module doc, "Coordinate frames") re-anchors the relative
        // key at the restore origin — so mutate-minted envs are live campaign
        // proposals now, not just codec/rebasing exercises.
        let (seed, policy) = (b.spec.seed(), b.spec.policy().clone());
        if let EnvSpec::Recorded { standing, .. } = &b.spec
            && !standing.is_empty()
        {
            // Standing-fault confinement (task-93 ruling): a standing-fault-
            // carrying base is never sliced into a branch-local delta. Vacuous
            // in the v1 vocabulary; enforced the day they appear — as a typed
            // error (task 99), never a panic.
            return Err(EnvCodecError::UnsupportedComposition);
        }
        let suffix: BTreeMap<u64, Action> = b
            .spec
            .overrides()
            .iter()
            .filter(|(m, _)| **m >= cut)
            .map(|(m, a)| (m - cut, a.clone()))
            .collect();
        // Reseed markers slice consistently (task 78): the suffix keeps the
        // markers at-or-past the cut, re-keyed to the branch origin — and it
        // must ALSO carry the origin marker (0 → seed) when none lands
        // exactly at the cut (PR #62 round-2 blocking fix): a marker-carrying
        // env's table is authoritative on the server, so a non-empty sliced
        // table with no floor marker would make the branch CONTINUE the
        // parent stream instead of reseeding — and `branch`'s is-empty stamp
        // never fires on a non-empty table. A base marker exactly at the cut
        // wins (its value IS what the stream became at that point).
        let mut suffix_reseeds: BTreeMap<u64, u64> = b
            .spec
            .reseeds()
            .iter()
            .filter(|(m, _)| **m >= cut)
            .map(|(m, s)| (m - cut, *s))
            .collect();
        if !suffix_reseeds.is_empty() {
            suffix_reseeds.entry(0).or_insert(seed);
        }
        let sliced = EnvSpec::Recorded {
            seed,
            policy,
            overrides: suffix,
            standing: Vec::new(),
            reseeds: suffix_reseeds,
        };
        // One deterministic host-plane tweak via the real codec (guest
        // overrides are preserved verbatim by its contract).
        let mutated = environment::EnvCodec::mutate(&sliced, salt);
        Ok(AdapterEnv {
            base_offset: b.pos,
            pos: b.pos,
            spec: mutated,
        }
        .encode())
    }

    fn compose(
        &self,
        base: &Environment,
        branch_local: &Environment,
    ) -> Result<Environment, EnvCodecError> {
        let b = Self::require(base)?;
        let d = Self::require(branch_local)?;
        // The complete operand-pair contract (see the `EnvCodecError` doc for the
        // full enumeration and why each is necessary). `require` above already
        // established per-operand `pos >= base_offset` for both.
        //
        // **Adjacency** (task 99, round 4): the trait defines `branch_local` as
        // recorded from a run branched off *base's snapshot*, so the delta's
        // origin must be exactly where the base was captured. A gap
        // (`d.base_offset > b.pos`) would splice a base prefix that never
        // produced this tail; an overlap (`d.base_offset < b.pos`) would discard
        // base state the tail assumed. Either mints a reproducer that does not
        // replay, so refuse it. This subsumes root ordering: with adjacency and
        // `b.pos >= b.base_offset`, `d.base_offset == b.pos >= b.base_offset`.
        if d.base_offset != b.pos {
            return Err(EnvCodecError::NonAdjacentChain(
                "branch-local delta's origin does not meet the base's capture point",
            ));
        }
        // Coordinate system (task 68): the splice point is the base's capture
        // relative to its own root. Adjacency makes this `d.base_offset -
        // b.base_offset`; `require`'s `b.pos >= b.base_offset` makes the
        // subtraction underflow-free. With a genesis-complete base this is the
        // absolute splice the v1 flow always used; with a parent-rooted base it
        // is the chain fold the task-68 materialization engine drives
        // (`compose(suffixᵢ, suffixᵢ₊₁)` down a lineage), whose result stays
        // rooted at the base's own origin.
        let cut = b.pos - b.base_offset;
        // A positionally-valid pair can still be an unsupported or overflowing
        // composition — both `Recorded`, equal seed/policy, no standing faults,
        // no Moment re-key past the axis. These spec-content invariants are
        // delegated to the wire codec and surfaced as typed errors, rather than
        // minting a reproducer that does not replay.
        let composed =
            environment::EnvCodec::compose(&b.spec, &d.spec, cut).map_err(map_env_err)?;
        Ok(AdapterEnv {
            base_offset: b.base_offset,
            pos: d.pos,
            spec: composed,
        }
        .encode())
    }
}

/// Map the underlying `environment` codec's [`EnvError`](environment::EnvError)
/// onto the seam's [`EnvCodecError`]. `compose` can only return `Overflow` or
/// `UnsupportedComposition` (both inputs already decoded cleanly via
/// [`AdapterEnv::decode`]); a residual `Malformed`/`BadVersion` is mapped
/// defensively so the match stays total.
fn map_env_err(e: environment::EnvError) -> EnvCodecError {
    use environment::EnvError as E;
    match e {
        E::Overflow => EnvCodecError::Overflow,
        E::UnsupportedComposition => EnvCodecError::UnsupportedComposition,
        E::BadVersion(v) => EnvCodecError::Malformed(v),
        E::Malformed => EnvCodecError::Malformed(ADAPTER_BLOB_VERSION),
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
    /// The server-side serial capture length at capture time — the byte offset a
    /// `branch`/`replay` off this snapshot restores the console to. A run off this
    /// snapshot appends past it, so [`console`](Machine::console) baselines its
    /// cursor here to read only that run's NEW bytes (task 69, mirroring the
    /// in-process recorder's `serial_len` cursor).
    serial_len: u32,
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
    /// The serial-capture byte offset the current Modulation started at — set at
    /// `branch`/`replay` to the snapshot's [`SnapMeta::serial_len`], so
    /// [`console`](Machine::console) reads only the current run's new bytes.
    console_cursor: u32,
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
            console_cursor: 0,
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

    /// Probe the server-side serial-capture length without draining it (task 69):
    /// a `Console` request at `offset = u32::MAX` returns an empty chunk and the
    /// capture's total length. A pure read used to baseline a snapshot's console
    /// cursor at capture time.
    fn console_total(&mut self) -> Result<u32, MachineError> {
        match self.call(&control_proto::Request::Console { offset: u32::MAX })? {
            control_proto::Reply::Console { total, .. } => Ok(total),
            other => Err(MachineError::Transport(format!(
                "console answered with an unexpected reply: {other:?}"
            ))),
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
        // A well-formed PROPOSAL the backend rejects as inadmissible — a distinct,
        // recoverable category from a torn transport (task-69 M2). All four decode
        // cleanly; the backend simply refuses to apply the fault: an out-of-range
        // `CorruptMemory` gpa (`PerturbOutOfRange`), a `Moment` behind the restore
        // point (`PerturbPastMoment`) or already carrying a fault
        // (`PerturbMomentTaken`), or an out-of-scope fault / capability this
        // backend does not service (`Unsupported`). Mapping these to the DISTINCT
        // `Inadmissible` variant lets a proposing loop discard the proposal and
        // continue — which the benchmark campaign loop (`conductor::benchcampaign`)
        // does. The generic `Explorer::modulation` does NOT yet handle this variant:
        // it propagates `Inadmissible` as an error and aborts (skip/retry for the
        // generic explorer is tracked in bead hm-f30). Either way, the genuine
        // failures below fall through to `Transport` and abort — so this remap NEVER
        // masks a backend death or a determinism divergence.
        e @ (Ce::PerturbOutOfRange { .. }
        | Ce::PerturbPastMoment { .. }
        | Ce::PerturbMomentTaken { .. }
        | Ce::Unsupported) => MachineError::Inadmissible(format!("control error: {e}")),
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
        // Decode the adapter blob and resolve the branch origin first (a
        // caller error must not touch the session).
        let decoded = AdapterEnv::decode(env)?;
        let Some(meta) = self.snaps.get(&snap.0) else {
            return Err(MachineError::UnknownSnapshot(snap.0));
        };
        let origin = meta.vtime;
        let restored_serial_len = meta.serial_len;
        // The single outbound frame conversion (module doc, "Coordinate
        // frames"): the blob's override keys are RELATIVE to its origin, the
        // server's contract is ABSOLUTE Moments (task 59 validates against
        // the restored floor and applies at `vns == Moment`) — re-anchor at
        // the actual restore point before shipping. Shipping the blob-frame
        // keys raw would mis-key every host fault under a parent-rooted fold
        // (PR #58 round-1 blocking finding).
        let wire_spec = rebase_to_wire(&decoded.spec, origin)?;
        let wire_env = control_proto::Environment {
            blob_version: EnvSpec::BLOB_VERSION,
            bytes: wire_spec.encode(),
        };
        match self.call(&control_proto::Request::Branch {
            snap: control_proto::SnapId(snap.0),
            env: wire_env,
        })? {
            control_proto::Reply::Unit => {
                // The new Modulation: its overrides are keyed from the snapshot's
                // capture Moment (the blob's own base_offset is advisory — the
                // authoritative origin is where the branch actually restored to).
                self.current = recorded(&decoded.spec);
                // Record the branch reseed into the blob frame (task 78): a
                // no-marker env made the server reseed from the env's seed at
                // the restore origin — stamp that as a marker at relative 0 so
                // the emitted delta composes reseed-aware (a fold re-executes
                // it at the collapsed hop's position). A marker-carrying env
                // already names its own reseeds (the server honored exactly
                // those); they ride through `recorded` verbatim.
                if self.current.reseeds().is_empty() {
                    self.current.record_reseed(0, decoded.spec.seed());
                }
                self.branch_offset = origin;
                self.pos = origin;
                self.pending_decision = None;
                // The restored console starts at the snapshot's capture length;
                // this run's console appends past it (task 69).
                self.console_cursor = restored_serial_len;
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
        let (branch_offset, pos, spec, serial_len) = (
            meta.branch_offset,
            meta.vtime,
            meta.spec.clone(),
            meta.serial_len,
        );
        match self.call(&control_proto::Request::Replay(control_proto::SnapId(
            snap.0,
        )))? {
            control_proto::Reply::Unit => {
                self.current = spec;
                self.branch_offset = branch_offset;
                self.pos = pos;
                self.pending_decision = None;
                // The verbatim restore also restores the console to the snapshot's
                // capture length; a re-run's console appends past it (task 69).
                self.console_cursor = serial_len;
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
        let id = match self.call(&control_proto::Request::Snapshot)? {
            control_proto::Reply::SnapId(id) => id,
            other => {
                return Err(MachineError::Transport(format!(
                    "snapshot answered with an unexpected reply: {other:?}"
                )));
            }
        };
        // Baseline the console cursor for branches off this snapshot: probe the
        // server-side serial length now (the length a later `branch`/`replay` will
        // restore the console to), so `console()` reads only a run's NEW bytes.
        // A pure read — `snapshot` did not advance the VM, so this is the captured
        // length. Probed once per snapshot (rare), never per branch (hot).
        let serial_len = self.console_total()?;
        self.snaps.insert(
            id.0,
            SnapMeta {
                vtime: self.pos,
                branch_offset: self.branch_offset,
                spec: self.current.clone(),
                serial_len,
            },
        );
        Ok(SnapId(id.0))
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

    fn sdk_events(&mut self) -> Result<Vec<(u64, u32, Vec<u8>)>, MachineError> {
        // Unlike `recorded_env` (client-local state), the SDK event capture lives
        // only server-side, so this is a wire round-trip (task 73). The server
        // bounds each reply to the control frame limit (round-5 P4), so page
        // through: fetch from the running offset until an empty page.
        let mut all: Vec<(u64, u32, Vec<u8>)> = Vec::new();
        loop {
            let page = match self.call(&control_proto::Request::SdkEvents {
                offset: all.len() as u32,
            })? {
                control_proto::Reply::SdkEvents(events) => events,
                other => {
                    return Err(MachineError::Transport(format!(
                        "sdk_events answered with an unexpected reply: {other:?}"
                    )));
                }
            };
            if page.is_empty() {
                break;
            }
            all.extend(page);
        }
        Ok(all)
    }

    /// The guest console (serial) capture of the current run (task 69): the
    /// server-side serial bytes emitted since the branch/replay that started this
    /// Modulation, split into `Moment`-stamped lines. Like [`sdk_events`] the
    /// capture lives only server-side, so this pages the wire `Console` verb from
    /// the branch-baselined [`console_cursor`](SocketMachine::console_cursor) until
    /// the capture is drained. Lines are split exactly as the in-process recorder's
    /// `runtrace::decode_chunks` does over a single chunk — each completed line
    /// keeps its `\n`; a trailing unterminated remainder is a final line — and all
    /// are stamped at the run's stop `Moment` (`pos`, stop-granular), so the
    /// scrape signal (task 67 `logtmpl`) sees the same records the recording loop
    /// would produce. A pure read: it never advances the VM, so a run's
    /// `state_hash` is identical whether or not this is called.
    fn console(&mut self) -> Result<Vec<(u64, Vec<u8>)>, MachineError> {
        let mut bytes: Vec<u8> = Vec::new();
        loop {
            let offset = self.console_cursor.saturating_add(bytes.len() as u32);
            let (total, chunk) = match self.call(&control_proto::Request::Console { offset })? {
                control_proto::Reply::Console { total, chunk } => (total, chunk),
                other => {
                    return Err(MachineError::Transport(format!(
                        "console answered with an unexpected reply: {other:?}"
                    )));
                }
            };
            if chunk.is_empty() {
                break;
            }
            bytes.extend(chunk);
            if self.console_cursor.saturating_add(bytes.len() as u32) >= total {
                break;
            }
        }
        Ok(split_console_lines(&bytes, self.pos))
    }
}

/// Split a console byte run into `Moment`-stamped lines, matching
/// `runtrace::decode_chunks` for a single chunk: a line ends at (and includes)
/// each `\n`; a trailing run with no terminator is a final line. All stamped at
/// `at` — the run's stop `Moment`, exactly as the in-process recorder stamps the
/// whole console at the stop (stop-granular). Explorer cannot depend on `runtrace`
/// (a layering cycle), so the single-chunk split is replicated here.
fn split_console_lines(bytes: &[u8], at: u64) -> Vec<(u64, Vec<u8>)> {
    let mut out = Vec::new();
    let mut line = Vec::new();
    for &b in bytes {
        line.push(b);
        if b == b'\n' {
            out.push((at, std::mem::take(&mut line)));
        }
    }
    if !line.is_empty() {
        out.push((at, line));
    }
    out
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
        Answer, EnvCodec, EnvCodecError, Environment, Machine, MachineError, StopConditions,
        StopMask, VTime,
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
            reseeds: std::collections::BTreeMap::new(),
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
        let composed = SpecEnvCodec.compose(&base, &delta).unwrap();
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
    fn compose_errors_on_a_seed_mismatch() {
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
        // A well-formed pair the wire codec cannot compose (task 99): a typed
        // error, never a panic.
        assert_eq!(
            SpecEnvCodec.compose(&base, &delta),
            Err(EnvCodecError::UnsupportedComposition)
        );
    }

    #[test]
    fn compose_errors_on_a_rekey_overflow() {
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
        assert_eq!(
            SpecEnvCodec.compose(&base, &delta),
            Err(EnvCodecError::Overflow)
        );
    }

    #[test]
    fn codec_seams_error_on_a_non_adapter_blob() {
        let junk = Environment {
            blob_version: ADAPTER_BLOB_VERSION,
            bytes: vec![1, 2, 3],
        };
        // The untrusted-input class: arbitrary bytes are a typed `Malformed`,
        // never a panic (task 99).
        assert_eq!(
            SpecEnvCodec.compose(&junk, &junk),
            Err(EnvCodecError::Malformed(ADAPTER_BLOB_VERSION))
        );
        assert_eq!(
            SpecEnvCodec.mutate(&junk, 0),
            Err(EnvCodecError::Malformed(ADAPTER_BLOB_VERSION))
        );
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
        let folded = SpecEnvCodec.compose(&base, &delta).unwrap();
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
        let folded = SpecEnvCodec
            .compose(&SpecEnvCodec.compose(&b, &s1).unwrap(), &s2)
            .unwrap();
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
                reseeds: std::collections::BTreeMap::new(),
            },
        }
        .encode();
        let out = SpecEnvCodec.mutate(&base, 0x5A17).unwrap();
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
        assert_eq!(out, SpecEnvCodec.mutate(&base, 0x5A17).unwrap());
    }

    #[test]
    fn compose_errors_on_a_non_adjacent_chain() {
        // Both operands are individually well-formed (pos >= base_offset), so
        // this exercises the pair-adjacency invariant, not `require`.
        let base = AdapterEnv {
            base_offset: 200,
            pos: 300,
            spec: spec_with_overrides(7, &[]),
        }
        .encode();
        // Gap: delta origin 400 is beyond the base's capture point 300.
        let gap = AdapterEnv {
            base_offset: 400,
            pos: 500,
            spec: spec_with_overrides(7, &[]),
        }
        .encode();
        assert!(matches!(
            SpecEnvCodec.compose(&base, &gap),
            Err(EnvCodecError::NonAdjacentChain(_))
        ));
        // Overlap: delta origin 250 is before the base's capture point 300
        // (also before the base's root would be the same taxonomy, since
        // adjacency subsumes root ordering).
        let overlap = AdapterEnv {
            base_offset: 250,
            pos: 350,
            spec: spec_with_overrides(7, &[]),
        }
        .encode();
        assert!(matches!(
            SpecEnvCodec.compose(&base, &overlap),
            Err(EnvCodecError::NonAdjacentChain(_))
        ));
        // The adjacent pair (delta origin 300 == base pos 300) composes.
        let adjacent = AdapterEnv {
            base_offset: 300,
            pos: 400,
            spec: spec_with_overrides(7, &[]),
        }
        .encode();
        assert!(SpecEnvCodec.compose(&base, &adjacent).is_ok());
    }

    #[test]
    fn mutate_errors_on_a_capture_behind_the_root() {
        let base = AdapterEnv {
            base_offset: 200,
            pos: 100, // mis-ordered: captured before its own root
            spec: spec_with_overrides(7, &[]),
        }
        .encode();
        assert!(matches!(
            SpecEnvCodec.mutate(&base, 0x1),
            Err(EnvCodecError::MisorderedChain(_))
        ));
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
        let out = SpecEnvCodec.mutate(&base, 0x5A17).unwrap();
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
        assert_eq!(out, SpecEnvCodec.mutate(&base, 0x5A17).unwrap());
        assert_ne!(
            out,
            SpecEnvCodec.mutate(&base, 0x5A18).unwrap(),
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
        // Well-formed but inadmissible PROPOSALS map to the recoverable
        // `Inadmissible` category (task-69 M2), NOT `Transport` — a proposing
        // driver discards them and continues. `Unsupported` (out-of-scope fault /
        // absent capability) rides with the stage-time perturb rejections here.
        for err in [
            Ce::PerturbOutOfRange {
                gpa: 0xdead_beef_dead_beef,
                ram_len: 1 << 31,
            },
            Ce::PerturbPastMoment { at: 3, floor: 10 },
            Ce::PerturbMomentTaken { at: 7 },
            Ce::Unsupported,
        ] {
            assert!(
                matches!(control_error_to_machine(err), MachineError::Inadmissible(_)),
                "inadmissible proposals are the recoverable category"
            );
        }
        // Genuine failures remain `Transport` — never conflated with a rejected
        // proposal, so skipping the latter can never mask one of these.
        for err in [
            Ce::RestoreFailed,
            Ce::ResolveWithoutDecision,
            Ce::MalformedAnswer,
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

    /// Build a scripted stream from an ordered list of reply frames. Sequence
    /// numbers are assigned **by position** (1, 2, …) so the seq-echo check passes
    /// and inserting a frame — e.g. the console-length probe a `snapshot` now
    /// issues (task 69) — never desyncs the rest of the script.
    fn scripted(frames: &[control_proto::Reply]) -> ScriptedStream {
        let mut bytes = Vec::new();
        for (i, reply) in frames.iter().enumerate() {
            control_proto::encode_reply((i + 1) as u32, &Ok(reply.clone()), &mut bytes).unwrap();
        }
        ScriptedStream {
            replies: Cursor::new(bytes),
        }
    }

    /// A `Console` reply carrying the capture's total length and an empty chunk —
    /// the shape a `snapshot`'s console-length probe (`offset = u32::MAX`) gets
    /// back. The scripted/mock guest emits no serial, so `total` is 0.
    fn console_reply(total: u32) -> control_proto::Reply {
        control_proto::Reply::Console {
            total,
            chunk: Vec::new(),
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
            server_caps_reply(),        // hello
            probe_reply(5000),          // connect's V-time probe: post-readiness
            Reply::SnapId(WsSnapId(1)), // snapshot (taken immediately, no run)
            console_reply(0),           // snapshot's console-length probe (cursor baseline)
            Reply::Unit,                // branch
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
            server_caps_reply(),        // hello
            probe_reply(50),            // connect probe → origin 50
            Reply::SnapId(WsSnapId(1)), // snapshot S1 @ 50 (branch base)
            console_reply(0),           // S1 console-length probe
            Reply::Unit,                // branch(S1, seeded) → timeline rooted at 50
            Reply::Stop(Ws::Deadline {
                vtime: WsVTime(200),
            }), // run → advance pos to 200 (branch_offset stays 50)
            Reply::SnapId(WsSnapId(2)), // snapshot S2 @ vtime 200, branch_offset 50
            console_reply(0),           // S2 console-length probe
            Reply::Unit,                // replay(S2)
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
        let stream = scripted(&[server_caps_reply_geo(u32::MAX, 0)]);
        assert!(matches!(
            SocketMachine::connect(stream, seeded_env(1)),
            Err(MachineError::Transport(_))
        ));
        // A non-zero producer tag (with zero map_bytes) is likewise unexpected in v1.
        let stream = scripted(&[server_caps_reply_geo(0, 3)]);
        assert!(matches!(
            SocketMachine::connect(stream, seeded_env(1)),
            Err(MachineError::Transport(_))
        ));
        // Zero-width geometry is accepted (the negotiated v1 shape); the probe
        // then completes the handshake.
        let stream = scripted(&[server_caps_reply(), probe_reply(0)]);
        assert!(SocketMachine::connect(stream, seeded_env(1)).is_ok());
    }

    // ---- the wire-frame conversion (PR #58 round-1 blocking fix) ----------

    /// A scripted stream that additionally CAPTURES the client's request
    /// bytes, so a test can decode exactly what went on the wire.
    struct CapturingStream {
        replies: Cursor<Vec<u8>>,
        written: std::rc::Rc<std::cell::RefCell<Vec<u8>>>,
    }

    impl Read for CapturingStream {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.replies.read(buf)
        }
    }

    impl Write for CapturingStream {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.written.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Decode every request frame the client wrote.
    fn captured_requests(bytes: &[u8]) -> Vec<control_proto::Request> {
        let mut out = Vec::new();
        let mut rest = bytes;
        while let Some((_seq, req, used)) =
            control_proto::decode_request(rest).expect("request framing")
        {
            out.push(req);
            rest = &rest[used..];
        }
        assert!(rest.is_empty(), "no partial trailing frame");
        out
    }

    /// The round-1 blocking fix, pinned at the exact wire bytes: `branch`
    /// re-anchors the blob-frame (relative) override keys at the branched
    /// snapshot's capture moment, so the server — whose task-59 contract is
    /// ABSOLUTE Moments — sees `origin + relative`. A host fault at relative
    /// 5 below a snapshot at 200 goes on the wire at Moment 205, never 5.
    #[test]
    fn branch_re_anchors_blob_frame_keys_to_absolute_wire_moments() {
        use control_proto::{Reply, SnapId as WsSnapId};
        let written = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut reply_bytes = Vec::new();
        for (i, reply) in [
            server_caps_reply(),        // hello
            probe_reply(200),           // connect's V-time probe → origin 200
            Reply::SnapId(WsSnapId(1)), // snapshot @ 200
            console_reply(0),           // snapshot's console-length probe
            Reply::Unit,                // branch
        ]
        .into_iter()
        .enumerate()
        {
            control_proto::encode_reply((i + 1) as u32, &Ok(reply), &mut reply_bytes).unwrap();
        }
        let stream = CapturingStream {
            replies: Cursor::new(reply_bytes),
            written: std::rc::Rc::clone(&written),
        };
        let mut m = SocketMachine::connect(stream, seeded_env(7)).unwrap();
        let snap = m.snapshot().unwrap();

        let fault = Action::Host(HostFault::CorruptMemory {
            gpa: 64,
            mask: environment::BitMask(0xFF),
        });
        let mut overrides = BTreeMap::new();
        overrides.insert(5u64, fault.clone()); // blob frame: relative to 200
        let env = AdapterEnv {
            base_offset: 200,
            pos: 260,
            spec: EnvSpec::Recorded {
                seed: 7,
                policy: FaultPolicy::none(),
                overrides,
                standing: Vec::new(),
                reseeds: std::collections::BTreeMap::new(),
            },
        }
        .encode();
        m.branch(snap, &env).unwrap();

        let reqs = captured_requests(&written.borrow());
        let wire = reqs
            .iter()
            .find_map(|r| match r {
                control_proto::Request::Branch { env, .. } => Some(env.clone()),
                _ => None,
            })
            .expect("a Branch request went on the wire");
        let spec = EnvSpec::decode(&wire.bytes).expect("wire spec decodes");
        let keys: Vec<u64> = spec.overrides().keys().copied().collect();
        assert_eq!(
            keys,
            vec![205],
            "the wire carries the ABSOLUTE Moment (origin 200 + relative 5)"
        );
        assert_eq!(spec.overrides().get(&205), Some(&fault), "action preserved");
        assert_eq!(spec.seed(), 7);

        // The adapter's OWN state stays in the blob frame: the recorded delta
        // re-emits the relative key, ready for compose.
        let recorded = AdapterEnv::decode(&m.recorded_env().unwrap()).unwrap();
        let keys: Vec<u64> = recorded.spec.overrides().keys().copied().collect();
        assert_eq!(keys, vec![5], "recorded_env stays blob-frame (relative)");
        assert_eq!(recorded.base_offset, 200);
    }

    /// A rebase that would overflow the axis is a malformed blob: refused
    /// loudly BEFORE any wire traffic (no Branch frame is ever written).
    #[test]
    fn branch_refuses_an_axis_overflowing_rebase_before_the_wire() {
        use control_proto::{Reply, SnapId as WsSnapId};
        let written = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut reply_bytes = Vec::new();
        for (i, reply) in [
            server_caps_reply(),
            probe_reply(200),
            Reply::SnapId(WsSnapId(1)),
            console_reply(0), // snapshot's console-length probe
        ]
        .into_iter()
        .enumerate()
        {
            control_proto::encode_reply((i + 1) as u32, &Ok(reply), &mut reply_bytes).unwrap();
        }
        let stream = CapturingStream {
            replies: Cursor::new(reply_bytes),
            written: std::rc::Rc::clone(&written),
        };
        let mut m = SocketMachine::connect(stream, seeded_env(7)).unwrap();
        let snap = m.snapshot().unwrap();

        let env = AdapterEnv {
            base_offset: 200,
            pos: 260,
            spec: spec_with_overrides(7, &[u64::MAX]), // 200 + MAX overflows
        }
        .encode();
        assert_eq!(
            m.branch(snap, &env),
            Err(MachineError::BadEnvironment(ADAPTER_BLOB_VERSION))
        );
        let reqs = captured_requests(&written.borrow());
        assert!(
            !reqs
                .iter()
                .any(|r| matches!(r, control_proto::Request::Branch { .. })),
            "the malformed rebase never reached the wire"
        );
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
            server_caps_reply(), // hello
            probe_reply(0),      // connect's V-time probe (origin 0)
            Reply::Stop(Ws::Decision {
                vtime: WsVTime(100),
                id: DecisionId(5),
                ctx: vec![],
            }), // run #1
            Reply::Stop(Ws::SnapshotPoint {
                vtime: WsVTime(110),
            }), // run #2: the None-resolve probe
            Reply::Stop(Ws::Quiescent {
                vtime: WsVTime(120),
            }), // run #3: the answering resolve
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

    // ---- reseed markers (task 78) ------------------------------------------

    /// A `Recorded` spec carrying only reseed markers.
    fn spec_with_reseeds(seed: u64, markers: &[(u64, u64)]) -> EnvSpec {
        EnvSpec::Recorded {
            seed,
            policy: FaultPolicy::none(),
            overrides: BTreeMap::new(),
            standing: Vec::new(),
            reseeds: markers.iter().copied().collect(),
        }
    }

    /// `branch` with a no-marker env stamps the branch reseed at relative 0,
    /// so the emitted delta is reseed-aware (a fold re-executes it at the
    /// collapsed hop's position); a marker-carrying env's own markers ride
    /// through verbatim and re-anchor on the wire like overrides.
    #[test]
    fn branch_records_the_branch_reseed_and_re_anchors_markers_on_the_wire() {
        use control_proto::{Reply, SnapId as WsSnapId};
        let written = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut reply_bytes = Vec::new();
        for (i, reply) in [
            server_caps_reply(),        // hello
            probe_reply(200),           // connect probe → origin 200
            Reply::SnapId(WsSnapId(1)), // snapshot @ 200
            console_reply(0),           // snapshot's console-length probe
            Reply::Unit,                // branch (no-marker env)
            Reply::Unit,                // branch (marker env)
        ]
        .into_iter()
        .enumerate()
        {
            control_proto::encode_reply((i + 1) as u32, &Ok(reply), &mut reply_bytes).unwrap();
        }
        let stream = CapturingStream {
            replies: Cursor::new(reply_bytes),
            written: std::rc::Rc::clone(&written),
        };
        let mut m = SocketMachine::connect(stream, seeded_env(7)).unwrap();
        let snap = m.snapshot().unwrap();

        // (1) A no-marker env: the branch reseed is stamped at relative 0.
        m.branch(snap, &SpecEnvCodec.seeded(0xD1CE)).unwrap();
        let recorded = AdapterEnv::decode(&m.recorded_env().unwrap()).unwrap();
        let got: Vec<(u64, u64)> = recorded
            .spec
            .reseeds()
            .iter()
            .map(|(k, v)| (*k, *v))
            .collect();
        assert_eq!(
            got,
            vec![(0, 0xD1CE)],
            "the branch reseed is recorded into the blob frame at relative 0"
        );

        // (2) A marker-carrying env: markers ride verbatim (no extra stamp) and
        // cross the wire re-anchored at the restore origin (200).
        let env = AdapterEnv {
            base_offset: 200,
            pos: 260,
            spec: spec_with_reseeds(7, &[(0, 0xAA), (40, 0xBB)]),
        }
        .encode();
        m.branch(snap, &env).unwrap();
        let recorded = AdapterEnv::decode(&m.recorded_env().unwrap()).unwrap();
        let got: Vec<(u64, u64)> = recorded
            .spec
            .reseeds()
            .iter()
            .map(|(k, v)| (*k, *v))
            .collect();
        assert_eq!(
            got,
            vec![(0, 0xAA), (40, 0xBB)],
            "markers preserved blob-frame"
        );

        let reqs = captured_requests(&written.borrow());
        let wire = reqs
            .iter()
            .filter_map(|r| match r {
                control_proto::Request::Branch { env, .. } => Some(env.clone()),
                _ => None,
            })
            .next_back()
            .expect("the marker Branch went on the wire");
        let spec = EnvSpec::decode(&wire.bytes).expect("wire spec decodes");
        let keys: Vec<(u64, u64)> = spec.reseeds().iter().map(|(k, v)| (*k, *v)).collect();
        assert_eq!(
            keys,
            vec![(200, 0xAA), (240, 0xBB)],
            "the wire carries ABSOLUTE marker Moments (origin + relative)"
        );
    }

    /// The PR #62 round-2 blocking fix: slicing a marker-carrying base at a
    /// NON-marker cut retains the future markers AND inserts the origin
    /// marker (0 → env seed) — a non-empty table with no floor marker would
    /// otherwise make the server continue the parent stream (the
    /// authoritative-table path; `branch`'s is-empty stamp never fires), so
    /// the branch's first draws must come from the env seed, which the
    /// vmm-core floor-marker gate
    /// (`branch_with_a_floor_marker_reseeds_from_the_marker_not_the_env_seed`)
    /// pins server-side for exactly this marker shape.
    #[test]
    fn mutate_slicing_at_a_non_marker_cut_inserts_the_origin_reseed() {
        // Base rooted at 0, captured at pos 100 (the cut — no marker there):
        // its own branch reseed at 0 and a future marker at 140.
        let base = AdapterEnv {
            base_offset: 0,
            pos: 100,
            spec: spec_with_reseeds(7, &[(0, 0xAA), (140, 0xBB)]),
        }
        .encode();
        let out = SpecEnvCodec.mutate(&base, 0x5A17).unwrap();
        let decoded = AdapterEnv::decode(&out).unwrap();
        let got: Vec<(u64, u64)> = decoded
            .spec
            .reseeds()
            .iter()
            .map(|(k, v)| (*k, *v))
            .collect();
        assert_eq!(
            got,
            vec![(0, 7), (40, 0xBB)],
            "the sliced suffix carries the origin marker (0 → env seed) plus the re-keyed \
             future marker — never a floor-markerless non-empty table"
        );
        // A base marker exactly at the cut wins over the synthetic origin
        // marker (its value IS the stream at that point).
        let base = AdapterEnv {
            base_offset: 0,
            pos: 100,
            spec: spec_with_reseeds(7, &[(100, 0xCC), (140, 0xBB)]),
        }
        .encode();
        let out = SpecEnvCodec.mutate(&base, 0x5A17).unwrap();
        let decoded = AdapterEnv::decode(&out).unwrap();
        let got: Vec<(u64, u64)> = decoded
            .spec
            .reseeds()
            .iter()
            .map(|(k, v)| (*k, *v))
            .collect();
        assert_eq!(got, vec![(0, 0xCC), (40, 0xBB)]);
        // And a fully-sliced-away table stays empty (the branch stamp path).
        let base = AdapterEnv {
            base_offset: 0,
            pos: 100,
            spec: spec_with_reseeds(7, &[(0, 0xAA)]),
        }
        .encode();
        let out = SpecEnvCodec.mutate(&base, 0x5A17).unwrap();
        let decoded = AdapterEnv::decode(&out).unwrap();
        assert!(
            decoded.spec.reseeds().is_empty(),
            "no future markers ⇒ empty table ⇒ branch's is-empty stamp handles the origin"
        );
    }

    /// `mutate` slices reseed markers at the relative cut, consistently with
    /// overrides; `compose` splices them positionally (through the underlying
    /// codec) so a folded delta stays reseed-aware.
    #[test]
    fn mutate_slices_and_compose_splices_reseed_markers() {
        // Base rooted at 100, captured at 160 → relative cut 60: the marker at
        // 0 (the base's own branch reseed) is dropped, the suffix re-keys
        // (70→10).
        let base = AdapterEnv {
            base_offset: 100,
            pos: 160,
            spec: spec_with_reseeds(7, &[(0, 0xAA), (70, 0xBB)]),
        }
        .encode();
        let out = SpecEnvCodec.mutate(&base, 0x5A17).unwrap();
        let decoded = AdapterEnv::decode(&out).unwrap();
        let got: Vec<(u64, u64)> = decoded
            .spec
            .reseeds()
            .iter()
            .map(|(k, v)| (*k, *v))
            .collect();
        assert_eq!(
            got,
            vec![(0, 7), (10, 0xBB)],
            "reseeds sliced at the relative cut, with the origin marker inserted (round-2 fix)"
        );

        // Compose: suffix₁ rooted at 100 (marker at its own 0), suffix₂
        // branched at 250 (marker at its own 0) → the fold carries both, the
        // second at the relative cut 150.
        let s1 = AdapterEnv {
            base_offset: 100,
            pos: 250,
            spec: spec_with_reseeds(7, &[(0, 0xAA)]),
        }
        .encode();
        let s2 = AdapterEnv {
            base_offset: 250,
            pos: 400,
            spec: spec_with_reseeds(7, &[(0, 0xBB), (40, 0xCC)]),
        }
        .encode();
        let folded = SpecEnvCodec.compose(&s1, &s2).unwrap();
        let decoded = AdapterEnv::decode(&folded).unwrap();
        let got: Vec<(u64, u64)> = decoded
            .spec
            .reseeds()
            .iter()
            .map(|(k, v)| (*k, *v))
            .collect();
        assert_eq!(
            got,
            vec![(0, 0xAA), (150, 0xBB), (190, 0xCC)],
            "the fold carries every collapsed hop's reseed at its position"
        );
    }
}
