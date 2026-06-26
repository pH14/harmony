# Task 24 — `dissonance/environment`: the Environment / decide seam + seeded faults

Read `tasks/00-CONVENTIONS.md` first. Touch only `dissonance/environment/`.

Design basis: `docs/DISSONANCE.md` (the Environment model + the `decide` seam). This crate is
the heart of dissonance — a seed → fault decisions is exactly `SeededEnv` below.

## Environment

Runs on: macOS and Linux. Requires: Rust (stable). Does **not** require `/dev/kvm`, a guest OS,
QEMU, root, or Intel hardware. Pure-logic, fully gate-testable on a laptop.

## Context

The guest runs against an **`Environment`** — the thing that answers every question the guest
cannot answer for itself: entropy, scheduling, payload, **and faults**. A fault is just an
environment that answers a service **non-nominally** ("EIO" instead of "ok", "dropped" instead
of "delivered"). There is no separate "fault component": fault *mechanism* lives in the
services (block, pv-net), fault *policy* lives in the explorer; they meet at exactly one seam,
`Environment::decide`.

This crate owns three things:
1. the **catalog** — the versioned enumeration of decision classes and fault kinds (the shared
   vocabulary every service and the explorer agree on);
2. the **seam** — the `Environment` trait (`decide(point) -> Answer`);
3. the **seeded backing** — `SeededEnv` (a PRNG answers every decision; pure DST, no host
   round-trip) and the **recorded** form (`seed + sparse overrides`) that reproduces a reactive
   session bit-for-bit.

The *reactive* backing (which suspends a run and asks the explorer over a socket) is **frontier**
(vmm-core) and out of scope — but `decide`'s contract must leave room for it (see `Outcome`).

Determinism is the whole point: same `Environment` ⇒ same answer sequence over the same decision
sequence ⇒ bit-identical replay. No `HashMap` order into any answer; no wall-clock; no `rand`
without the caller's seed (conventions rule 4).

## Public API

```rust
/// The shared catalog. Versioned: `CATALOG_VERSION` bumps when a class/fault is added.
/// `#[repr(u16)]`, stable discriminants (replay across versions depends on them).
pub const CATALOG_VERSION: u16 = 1;

#[repr(u16)]
pub enum DecisionClass {
    Entropy = 1,    // guest pulled entropy
    Payload = 2,    // guest pulled fuzz payload
    Scheduler = 3,  // a schedulable yield point between in-guest nodes
    NetSend = 4,    // a frame handed to the pv-net switch
    BlockIo = 5,    // a block read/write/flush
    Process = 6,    // a node lifecycle point (pause/kill/restart)
}

/// A concrete decision the platform must answer. Carries the class + service-specific context.
/// `ctx` fields are what a policy reads to choose an answer; they never reach a hash unsorted.
/// Max bytes one `Entropy`/`Payload` decision may supply. The faultable service clamps its request
/// to this before building the point, so `bytes ≤ MAX_SUPPLY_LEN` holds at the seam and a `Supply`
/// can never force an unbounded allocation from an untrusted guest count (conventions rule 4).
pub const MAX_SUPPLY_LEN: u32 = 1 << 20;   // 1 MiB
pub enum DecisionPoint {
    Entropy   { bytes: u32 },                                   // bytes ≤ MAX_SUPPLY_LEN (service-clamped)
    Payload   { bytes: u32 },                                   // bytes ≤ MAX_SUPPLY_LEN (service-clamped)
    Scheduler { ready: u32 },                                   // count of runnable nodes
    NetSend   { src: NodeId, dst: NodeId, conn: ConnId, len: u32 },
    BlockIo   { op: BlockOp, lba: u64, len: u32 },
    Process   { node: NodeId },
}
impl DecisionPoint { pub fn class(&self) -> DecisionClass; }

/// The answer the platform returns at a decision point.
/// - **Supply classes** (`Entropy`/`Payload`/`Scheduler`): the Environment *supplies* a value, so
///   a non-fault answer is `Supply(bytes)` — the entropy/payload bytes, or for `Scheduler` the
///   chosen runnable index as a little-endian `u32`. (These classes never `Fault`.)
/// - **Fault classes** (`NetSend`/`BlockIo`/`Process`): the service proceeds (`Nominal`) or is
///   perturbed (`Fault`). (These classes never `Supply`.)
pub enum Answer { Nominal, Supply(Vec<u8>), Fault(Fault) }
impl Answer {
    /// Versioned, byte-deterministic. The control plane carries an `Answer` as opaque bytes
    /// (control-proto's `Answer(Vec<u8>)`); vmm-core decodes a `Run{resolve}` payload back through
    /// this. `decode` never panics on bad bytes and rejects them with `EnvError` — the backend maps
    /// that to `ControlError::MalformedAnswer`. (Class/bounds admissibility for the *outstanding
    /// decision* is then checked as `RecordedEnv` does for overrides.)
    pub fn encode(&self) -> Vec<u8>;
    pub fn decode(b: &[u8]) -> Result<Self, EnvError>;
}

/// The fault catalog, grouped by the class it applies to. Vocabulary is convergent across
/// FDB/Antithesis (see `fault-injection-model`); `VTime` delays are in branch-count units.
pub enum Fault {
    // NetSend (per-frame faults only). A network *partition* is NOT a per-send fault: it is a
    // standing, correlated topology policy (a link + V-time window where ALL frames drop
    // together), armed via pv-net's `set_partition` (task 26) and consulted by the delivery
    // schedule. A one-frame "partition" would just be `NetDrop`; keeping it standing preserves
    // the correlation that makes partitions find split-brain / quorum bugs.
    NetDrop, NetDelay(VTime), NetReorder, NetDup, NetCorrupt(CorruptSpec),
    // BlockIo
    BlockEio, BlockLatency(VTime), BlockTorn(u32), BlockNospc,
    // Process
    ProcPause(VTime), ProcKill, ProcRestart,
}

/// What `decide` yields. A pure backing always returns `Resolved`; the (frontier) reactive
/// backing may return `NeedsHost` to suspend the run and ask the explorer. Defined here so the
/// seam is stable across both backings.
pub enum Outcome { Resolved(Answer), NeedsHost }

pub trait Environment {
    /// Consulted by a faultable service BEFORE any side effect. Deterministic given the
    /// backing's own state + the point. Never panics.
    fn decide(&mut self, point: &DecisionPoint) -> Outcome;
}

/// Newtypes (mirror the integration types; defined locally per conventions rule 2).
pub struct NodeId(pub u32);
pub struct ConnId(pub u64);
pub struct VTime(pub u64);
pub enum BlockOp { Read, Write, Flush }
pub struct CorruptSpec { pub offset: u32, pub xor: u8 }

/// Per-class fault eligibility + probability, sampled by SeededEnv. Integer/fixed-point only.
pub struct FaultPolicy { /* per-class: numerator/denominator probability + eligible Fault set */ }
impl FaultPolicy {
    pub fn none() -> Self;                  // all-nominal (the baseline run)
    pub fn from_bytes(b: &[u8]) -> Result<Self, EnvError>;
    pub fn to_bytes(&self) -> Vec<u8>;      // byte-deterministic
}

/// DST backing: a PRNG answers every decision from a seed. Never suspends.
pub struct SeededEnv { /* prng state + FaultPolicy */ }
impl SeededEnv {
    pub fn new(seed: u64, policy: FaultPolicy) -> Self;
}
impl Environment for SeededEnv { /* decide → Resolved(Answer), PRNG-driven */ }

/// The reproducer: a seed (auto-answers high-frequency decisions) + sparse explorer overrides
/// (the interesting faults), keyed by decision index. This is the serialized blob R2 carries
/// as an opaque `Environment` (control-proto's `Environment { blob_version, bytes }`).
pub enum EnvSpec {
    // Both variants carry the `FaultPolicy`: a seed alone can't reproduce a campaign whose answer
    // sequence depended on the eligible-faults/probabilities, so the policy is part of the artifact.
    Seeded { seed: u64, policy: FaultPolicy },                    // pure DST: seed+policy, no overrides
    Recorded {
        seed: u64,
        policy: FaultPolicy,
        overrides: Vec<(DecisionId, Answer)>,                    // per-decision faults
        standing: Vec<StandingFault>,                            // correlated, V-time-windowed faults
    },
}

/// A correlated, V-time-windowed fault that is NOT a per-decision `Answer` — e.g. a network
/// partition (a link + window where ALL frames drop together). It is part of the reproducer so a
/// `Branch`/`Replay` re-applies it deterministically: the frontier translates each entry into the
/// service's standing-fault API (e.g. pv-net `Switch::set_partition`, task 26) on branch — it is
/// applied imperatively by the frontier, NOT through `decide`. Never armed out-of-band where it
/// would escape replay. `target` is service-interpreted (e.g. encodes the `(NodeId, NodeId)` link);
/// byte-deterministic, no `HashMap` order into it.
pub struct StandingFault { pub class: DecisionClass, pub target: Vec<u8>, pub window: (VTime, VTime) }
pub struct DecisionId(pub u64);              // i-th decision since branch (monotonic)
impl EnvSpec {
    pub const BLOB_VERSION: u16 = 1;
    pub fn encode(&self) -> Vec<u8>;                              // versioned, byte-deterministic
    pub fn decode(b: &[u8]) -> Result<Self, EnvError>;           // never panics on bad bytes
    pub fn materialize(&self) -> RecordedEnv;                    // an Environment backing
}

/// Answers from overrides first, else falls back to the seeded base. Records the decision index.
/// An override whose `Answer` is **inadmissible for the decision** is deterministically **ignored**
/// — the seeded base answers instead — so a mutated/hostile `EnvSpec` can never hand a service an
/// impossible answer or panic `decide` (conventions rule 4). Inadmissible = wrong class (a supply
/// class with a `Fault`; a fault class with a `Supply` or a foreign-class `Fault`) **or** out of the
/// point's bounds: a `Supply` whose length ≠ the requested `Entropy`/`Payload` `bytes`, a `Scheduler`
/// `Supply` not exactly 4 bytes or whose decoded selection ≥ `ready`, a `BlockTorn(n)`/payload longer
/// than the request. A well-formed recorder never emits a mismatch; the guard exists purely for
/// untrusted bytes.
pub struct RecordedEnv { /* SeededEnv base + override map + counter */ }
impl Environment for RecordedEnv { /* decide → Resolved(Answer) */ }

pub enum EnvError { BadVersion(u16), Malformed, /* thiserror */ }
```

Provide a documented PRNG (reuse the `hypercall-proto` xorshift64\* algorithm — **defined
locally**, no sibling dep — so fault sampling is portable and golden-testable). The PRNG used for
fault sampling is independent of the guest entropy stream.

## Acceptance gates

Beyond the standard gates in conventions:

1. **Replay determinism (the core invariant).** Two `SeededEnv::new(seed, policy)` answer an
   identical `DecisionPoint` sequence identically. A `RecordedEnv` materialized from an
   `EnvSpec::Recorded` reproduces the recorded answers exactly. Property test, ≥256 cases.
2. **Override semantics.** For every overridden `DecisionId` the override wins **iff its `Answer` is
   admissible for that decision** — right class AND within the point's bounds (a `Supply` length
   matching the requested `bytes`, a `Scheduler` `Supply` of exactly 4 bytes decoding to a selection
   `< ready`, a `BlockTorn`/payload `≤` the request); an inadmissible override (wrong class, or an
   out-of-bounds value such as a 1-byte `Supply` for `Entropy { bytes: 32 }`, a 3-byte `Scheduler`
   supply, or a `Scheduler` index `≥ ready`) is deterministically
   ignored — the seeded base answers and `decide` never panics; for every other decision the seeded
   base answers; the decision counter advances by exactly one per `decide`.
3. **Golden answers.** Hand-written expected `Answer` sequences for at least one seed under a
   known `FaultPolicy` across each `DecisionClass` (pins PRNG + sampling against drift).
4. **Codec.** `EnvSpec::encode`→`decode` round-trips for arbitrary specs (proptest); `decode`
   on arbitrary/mutated bytes never panics and rejects off-version with `EnvError::BadVersion`.
5. **`FaultPolicy` byte-determinism.** `to_bytes` is identical for equal policies; `from_bytes`
   round-trips; malformed input errors cleanly.
6. **No order leakage.** A test asserts no `HashMap`/`HashSet` iteration reaches an `Answer` or
   an encoded byte (use `BTreeMap`/sorted vecs for the override map).

## Non-goals

The reactive/socket backing (frontier, vmm-core); the actual service behavior (block, pv-net
interpret `Fault`, they do not live here); coverage; the wire framing of the control plane (that
is task 25). Do not invent network/block *enforcement* — this crate only **decides**.
