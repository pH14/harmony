# Task 25 — `dissonance/control-proto`: out-of-band control-plane wire protocol

Read `tasks/00-CONVENTIONS.md` first. Touch only `dissonance/control-proto/`.

Design basis: `docs/DISSONANCE.md` (the control plane + the Environment model). This is the
out-of-band twin of task 01 (`hypercall-proto`): **protocol layer only**.

## Environment

Runs on: macOS and Linux. Requires: Rust (stable). Does **not** require `/dev/kvm`, a guest OS,
or a real socket — this crate is host `std`, the wire types + codec, exercised over an in-process
loopback. The actual unix socket + verb→backend binding is **frontier** (vmm-core), built later
against these types.

## Context

R2 is the **out-of-band** plane: the explorer drives a VM as a black box —
`snapshot`/`branch`/`replay`/`run`/`hash` — over a versioned, length-delimited,
request/response protocol on a unix `SOCK_STREAM`. It is distinct from the **in-band** guest↔host
hypercall plane (task 01). Two design rules from the ruling are load-bearing here:

- **No bare `restore`.** Every restore is `replay` (verbatim — the determinism-gate / repro path)
  or `branch` (reseed with a new `Environment` — the explore path). The choice is explicit at
  every call site.
- **Schema-blind to `Environment`.** R2 ferries the variation unit as an **opaque, versioned
  blob** (`Environment { blob_version, bytes }`) and the per-decision answer as opaque
  `Answer(Vec<u8>)`. It never parses them — their structure is task 24's
  (`environment::EnvSpec`/`Answer`). This is what lets R2 be coded ahead of the fault model.

Two result categories, fail-loud: a guest-observable outcome is a `StopReason` (data); a
VM/transport failure is a `ControlError` (a loud protocol error). Never report one as the other.

Encoding must be **bit-deterministic and versioned from day one**; the decoder is a
`docs/CODE-QUALITY.md` **Tier-1 fuzz target**.

## Public API

```rust
// ---- opaque carried types (R2 is schema-blind) ----
pub struct Environment { pub blob_version: u16, pub bytes: Vec<u8> }   // = task 24 EnvSpec, opaque here
pub struct Answer(pub Vec<u8>);                                        // opaque resolution of one Decision

// ---- handles & addressing ----
pub struct SnapId(pub u64);     // pool-wide snapshot handle
pub struct VTime(pub u64);      // a moment = bare V-time (single-vCPU ⇒ unique)
pub struct DecisionId(pub u64); // identifies the one outstanding decision

// ---- requests ----
pub enum Request {
    Hello(Caps),                                    // negotiate; must be first frame
    Snapshot,                                       // -> SnapId  (quiescent point only)
    Drop(SnapId),                                   // -> ()
    Branch { snap: SnapId, env: Environment },      // restore + reseed from env  -> ()
    Replay(SnapId),                                 // restore verbatim            -> ()
    // `resolve` answers the immediately-prior `Decision` stop. A `resolve` with no outstanding
    // `Decision` (the prior stop wasn't a `Decision`) is a loud `ControlError::ResolveWithoutDecision`,
    // never silently dropped — absorbing it would desync the `DecisionId` counter ⇒ broken replay.
    Run { until: StopConditions, resolve: Option<Answer> },
    Hash { scope: HashScope },                      // -> [u8; 32]
}

// ---- run control ----
pub struct StopConditions { pub deadline: Option<VTime>, pub on: StopMask }
/// Which non-terminal decision/exit CLASSES surface (vs. auto-service). Crash / assertion /
/// quiescence ALWAYS stop. The class bits mirror `environment::DecisionClass` (defined locally
/// per conventions rule 2). **Pinned mapping (integrator):** `class_bit` IS the `DecisionClass`
/// discriminant and `StopMask` bit N = `1 << DecisionClass`-discriminant — discriminants frozen
/// at task 24's `DecisionClass` enum (1..=6). Both crates encode the identical bit so the
/// armed-class set can never diverge (divergence ⇒ different decisions surface ⇒ broken replay).
pub struct StopMask(pub u32);
impl StopMask { pub const NONE: Self; pub fn arm(self, class_bit: u16) -> Self; pub fn armed(&self, class_bit: u16) -> bool; }

pub enum HashScope { Whole, Disk, Region { base: u64, len: u64 } }

// ---- run outcomes (guest-observable; this is the explorer's reaction surface) ----
pub enum StopReason {
    // always-present substrate
    Deadline      { vtime: VTime },
    Quiescent     { vtime: VTime },                 // HLT + empty timer queue = test ended
    Crash         { vtime: VTime, info: CrashInfo },
    // enrichment (present with a cooperating guest / SDK)
    Decision      { vtime: VTime, id: DecisionId, ctx: Vec<u8> },  // answer via next Run{resolve}
    SnapshotPoint { vtime: VTime },                 // SDK lifecycle "ready"
    Assertion     { vtime: VTime, ev: EventRef },   // SDK Always-violated / Sometimes-hit
    // No `Host` variant: an in-band hypercall is serviced by the frontier's consonance plane and the
    // run continued — it never surfaces as an out-of-band stop (anything R2 must react to arrives as
    // `Decision`/`SnapshotPoint`/`Assertion`). This keeps `StopReason` representable by task 12's
    // explorer surface (which also has no `Host`), preserving the two-result-category rule.
}
pub struct CrashInfo { /* kind: panic/triple-fault/shutdown + detail */ }
pub struct EventRef  { pub id: u32, pub data: Vec<u8> }

// ---- transport/backend failures (NOT guest outcomes) ----
// A frame can decode cleanly yet carry a bad *payload*: `MalformedEnvironment` = a `Branch` env
// blob that fails task 24's `EnvSpec::decode`; `MalformedAnswer` = a `Run{resolve}` answer that is
// malformed or wrong-class for the outstanding decision. Both are loud + distinct from `Protocol`
// (wire framing) and `BadEnvVersion` (version) — the backend never misclassifies them or passes
// untrusted bytes into service code (fail-loud, conventions rule 4).
pub enum ControlError {
    UnknownSnapshot(SnapId), RestoreFailed, SnapshotWhileArmed, NotQuiescent,
    BadEnvVersion(u16), MalformedEnvironment, ResolveWithoutDecision, MalformedAnswer,
    Protocol(ProtocolError),
}
pub enum ProtocolError { ShortFrame, BadMagic, BadVersion, BadLength, /* thiserror */ }

// ---- negotiation ----
pub struct Caps {
    pub protocol_version: u16,
    pub env_version_min: u16, pub env_version_max: u16,
    pub coverage: CoverageGeometry,                 // where/shape of the shmem map; bytes never on the socket
    pub flags: CapFlags,                            // e.g. guest_has_sdk, coverage_producer kind
}
pub struct CoverageGeometry { pub map_bytes: u32, pub producer: u8 }
pub struct CapFlags(pub u32);

// ---- codec ----
pub const PROTO_VERSION: u16 = 1;
/// Max on-wire frame body. Generous for Environment blobs / hashes but bounded so untrusted
/// transport can't force unbounded buffering: `decode_*` returns `ProtocolError::BadLength` the
/// moment a header's length field exceeds this — BEFORE buffering the body.
pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;   // 16 MiB
/// Encode a request/reply into a length-delimited frame (magic + version + seq + len + body).
/// Host std; bodies (Environment blobs, hashes) allowed up to `MAX_FRAME_LEN`. **Fallible:** a body
/// that would exceed `MAX_FRAME_LEN` returns `Err(ProtocolError::BadLength)` — never panics,
/// truncates, or emits a frame the `decode_*` cap would reject; on `Err`, `buf` is left unchanged.
pub fn encode_request(seq: u32, req: &Request, buf: &mut Vec<u8>) -> Result<(), ProtocolError>;
pub fn encode_reply(seq: u32, reply: &Result<Reply, ControlError>, buf: &mut Vec<u8>) -> Result<(), ProtocolError>;
/// Decode exactly one frame from the front of `buf`; returns (seq, value, bytes_consumed).
/// MUST never panic on any input. Partial frame ⇒ Ok(None)-style "need more".
pub fn decode_request(buf: &[u8]) -> Result<Option<(u32, Request, usize)>, ProtocolError>;
pub fn decode_reply(buf: &[u8]) -> Result<Option<(u32, Result<Reply, ControlError>, usize)>, ProtocolError>;

pub enum Reply { Hello(Caps), SnapId(SnapId), Unit, Stop(StopReason), Hash([u8; 32]) }
```

## Acceptance gates

Beyond the standard gates in conventions:

1. **Golden bytes.** Hand-written expected frames for every `Request` variant and every `Reply`
   / `ControlError` variant (asserts exact `[u8]`, pinning the wire format).
2. **Round-trip property test.** Arbitrary in-bounds `Request`/`Reply`/`ControlError` encode→decode
   to identical values; `seq` echoes; ≥256 cases. A body exceeding `MAX_FRAME_LEN` makes `encode_*`
   return `BadLength` (asserted) — never a panic, truncation, or an undecodable frame.
3. **Adversarial decode (Tier-1 fuzz target).** `decode_*` on arbitrary byte strings, on valid
   frames with single-byte mutations, and on truncations of every length never panics, never
   reads out of bounds, and reports `ProtocolError` cleanly. A header advertising a body length
   `> MAX_FRAME_LEN` is rejected with `BadLength` immediately — asserted to happen *before* the
   body is buffered (no unbounded allocation from untrusted length fields). Provide a `cargo-fuzz`
   target (`fuzz/` is exempt from the dep whitelist).
4. **Version negotiation.** A `Hello` with an out-of-range `protocol_version`/`env_version`
   range is detectable from `Caps` alone; an off-version `Environment.blob_version` decodes to a
   `Request` carrying it (so the backend can answer `BadEnvVersion`), not a decode error.
5. **Streaming framing.** Feeding the byte stream one byte at a time through `decode_*` yields
   the same sequence of frames as feeding it whole (partial-frame handling is correct).
6. **Loopback.** An in-process server (a `Reply`-returning stub) driven by an in-process client
   over a `Vec<u8>` pipe exercises every verb; two identical sessions produce byte-identical
   transcripts.

## Non-goals

The unix socket itself, the verb→`Backend`/`snapshot-store`/`Dispatcher` binding, the
stage-and-re-enter run suspension (all **frontier**, vmm-core); the internal structure of
`Environment`/`Answer` (task 24); the coverage map bytes (shmem, never serialized here).
