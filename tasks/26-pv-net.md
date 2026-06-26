# Task 26 — `dissonance/pv-net`: host-side L2 switch + V-time network-fault scheduler

Read `tasks/00-CONVENTIONS.md` first. Touch only `dissonance/pv-net/`.

Design basis: `docs/DISSONANCE.md` (the fault model + the `pv-net` locus + the `decide` seam).
Host-side enforcement; no in-guest `tc`/netfilter.

## Environment

Runs on: macOS and Linux. Requires: Rust (stable). Does **not** require `/dev/kvm`, a guest OS,
QEMU, or real networking. Pure-logic — the switch is exercised with synthetic L2 frames, a fake
`Environment`, and a fake V-time clock. The hypercall TX handler, the RX ring + pv-NIC IRQ, and
guest-memory frame copies are **frontier** (vmm-core), built later against this crate.

## Context

In-guest "nodes" are containers/processes in one deterministic single-vCPU guest, so inter-node
traffic is guest-internal. We route it through a `net_tx` hypercall to a **host L2 switch** (this
crate), which sees every frame and is the single point where network faults apply — host-side, so
enforcement is determinism-clean.

The key idea: **delivery is scheduled in V-time, and every network fault is an operation on that
schedule.** A frame sent at V-time `T` is delivered at `T + L₀`; `drop` = no delivery event,
`delay(d)` = `T + L₀ + d`, `reorder`/`dup`/`corrupt`/`partition` likewise manipulate the
schedule. Latency is in **V-time (branch-count) units**, the only deterministic clock. The switch
consults the `Environment` (`decide`) per send to choose the answer.

This crate is the switch + the schedule; it does **not** own the `Environment` (task 24) — it
takes a decider through a locally-defined trait (conventions rule 2; integrator wires task 24's
`Environment` to it).

## Public API

```rust
/// V-time (branch count). Mirrors the integration type; defined locally.
pub struct VTime(pub u64);
pub struct NodeId(pub u32);
pub struct ConnId(pub u64);

/// Parsed L2/L3/L4 header fields the switch needs for routing + fault targeting.
pub struct FrameHdr {
    pub src_mac: [u8; 6], pub dst_mac: [u8; 6], pub broadcast: bool,
    pub src: NodeId, pub dst: NodeId,          // resolved from address ↔ node map
    pub conn: ConnId, pub len: u32,
}
/// Parse a raw L2 frame. MUST never panic; returns None for unparseable/too-short input.
pub fn parse(frame: &[u8], nodes: &NodeMap) -> Option<FrameHdr>;
pub struct NodeMap { /* MAC/IP ↔ NodeId, set at config */ }

/// The locally-defined decision seam (integrator binds task 24's `Environment`). The switch asks
/// for an answer per send; in seeded mode this is a pure PRNG draw, no host round-trip.
pub struct NetSend { pub src: NodeId, pub dst: NodeId, pub conn: ConnId, pub len: u32 }
// No `Partition` variant by design: a partition is the *standing* `set_partition` topology
// policy (consulted in `on_tx`), not a per-send answer — see task 24's `Fault` note. A
// per-send "partition" would just be `Drop`.
pub enum NetAnswer {
    Deliver,                 // nominal: T + L₀
    Drop,
    Delay(VTime),            // T + L₀ + d (saturating — see the V-time-overflow note below)
    Dup,                     // two events
    // `offset` is reduced modulo the frame length before the XOR (an empty frame ⇒ no-op `Deliver`),
    // so a recorded/mutated out-of-range `offset` is deterministic and never panics (rule 4).
    Corrupt { offset: u32, xor: u8 },
    /// Hold this frame, deliver it after the *next* frame on this link. If no later frame ever
    /// arrives, the held frame is flushed at `due(now)` once `now` passes a bounded reorder
    /// horizon (`T + L₀ + REORDER_MAX`, a fixed V-time constant) — so a last-frame reorder can
    /// never strand or hang a Timeline.
    Reorder,
}
pub trait NetOracle { fn decide_send(&mut self, now: VTime, s: &NetSend) -> NetAnswer; }

/// **V-time arithmetic saturates.** Every scheduled time — `T + L₀ + d`, the reorder horizon
/// `T + L₀ + REORDER_MAX` — uses saturating `u64` add. A mutated/hostile `Delay(u64::MAX)` or a
/// `now` near `u64::MAX` clamps to `VTime(u64::MAX)` (delivered only if a Timeline ever reaches it,
/// i.e. effectively dropped) — never a debug panic, never a release wrap that delivers in the past
/// (conventions rule 4).

/// A scheduled delivery the frontier will enact when V-time reaches `at`.
pub struct NetDeliver { pub dst: NodeId, pub frame: Vec<u8>, pub at: VTime }

pub struct Switch { /* l0, jitter, standing partitions, BTreeMap<(VTime, seq) -> NetDeliver>,
                      held-reorder buffer (per link), next monotonic seq */ }
impl Switch {
    pub fn new(nodes: NodeMap, l0: VTime) -> Self;

    /// Handle one transmit: parse, consult the oracle (or a standing partition), and enqueue
    /// 0..N deliveries. Deterministic given (now, frame, oracle state, switch state). A frame whose
    /// `parse` fails (truncated/malformed guest bytes) is **dropped** — `on_tx` returns an empty
    /// `Vec`, never unwraps or panics (it is the guest-controlled TX entry point; rule 4).
    /// Broadcast frames fan out to all destinations, each its own decision — the destinations
    /// are visited in **sorted `NodeId` order** so the per-destination oracle consultations draw
    /// from the PRNG in a fixed order (`NodeMap` is BTree-/sorted-vec-backed, never `HashMap`:
    /// iteration order must not reach an answer — conventions rule 4).
    pub fn on_tx(&mut self, now: VTime, frame: Vec<u8>, oracle: &mut dyn NetOracle) -> Vec<NetDeliver>;

    /// Pop all deliveries due at or before `now` (the frontier drains these into RX rings).
    /// Ties broken by a deterministic monotonic seq (no wall-clock, no map-order leakage).
    pub fn due(&mut self, now: VTime) -> Vec<NetDeliver>;

    /// Standing connection/node fault for a V-time window (partition / clog). While active, the
    /// matching sends are answered without consulting the oracle. **Provenance/determinism:** a
    /// partition's reproducer is the `EnvSpec::Recorded.standing` schedule (task 24). On **`branch`**
    /// the frontier applies that schedule by calling this (re-arming identically for the same `env`);
    /// on **`replay`** (which carries only a `SnapId`) the standing state is restored verbatim from
    /// `vm_state` (`save_state`/`restore_state`), not from an env. Either way it is never armed
    /// out-of-band in a way that escapes the reproducer.
    pub fn set_partition(&mut self, a: NodeId, b: NodeId, window: (VTime, VTime));
    pub fn set_throttle(&mut self, link: (NodeId, NodeId), max_per: (u32, VTime), window: (VTime, VTime));

    /// Snapshot support (the switch is part of vm_state): byte-deterministic. Serializes the **whole**
    /// switch state — pending deliveries, standing faults/throttles, **the held-`Reorder` buffer**,
    /// and **the next monotonic tie-break `seq`**. If a quiescent snapshot lands while a `Reorder`
    /// frame is held (outside the pending map) or after some deliveries advanced the seq, dropping
    /// either would change RX ordering on restore and break branch/replay determinism.
    pub fn save_state(&self) -> Vec<u8>;
    pub fn restore_state(&mut self, b: &[u8]) -> Result<(), NetError>;
}
pub enum NetError { Malformed, /* thiserror */ }
```

## Acceptance gates

Beyond the standard gates in conventions:

1. **Parse + `on_tx` never panic (fuzz).** `parse` on arbitrary/truncated/mutated byte strings
   never panics and never reads out of bounds; **and `on_tx` on arbitrary frame bytes drops
   malformed input and never panics** (fuzz `on_tx` itself, not only `parse` — it is the
   guest-controlled entry). Provide a `cargo-fuzz` target.
2. **Schedule determinism.** Identical `(frame sequence, oracle, clock)` ⇒ identical `NetDeliver`
   sequence from `on_tx`/`due`. Property test, ≥256 cases; ties resolved by the monotonic seq,
   never by map iteration order (`BTreeMap`, asserted). Edge V-times (`Delay(u64::MAX)`, `now` near
   `u64::MAX`) saturate to `VTime(u64::MAX)` — asserted to never debug-panic or release-wrap.
3. **Replay.** A recorded `NetAnswer` sequence (oracle replays it) reproduces a byte-identical
   delivery schedule.
4. **Per-verb golden.** For each `NetAnswer`, a hand-written expectation on the resulting
   schedule: `Deliver`→`{at: T+L₀}`; `Drop`→`{}`; `Delay(d)`→`{at: T+L₀+d}`; `Dup`→two events;
   `Corrupt`→delivered bytes differ at exactly `offset % len` (out-of-range/empty-frame `Corrupt`
   is a deterministic no-op, no panic); `Reorder`→delivered after the next on
   the link **AND** a `Reorder` with no later frame on the link is flushed exactly once at the
   bounded horizon (`T+L₀+REORDER_MAX`), never stranded; `partition(window)`→all matching sends
   dropped within the window, delivered outside.
5. **Broadcast.** A broadcast frame produces one decision + delivery per non-source node, and the
   per-destination oracle consultations occur in **sorted `NodeId` order** (assert the draw order
   against a recording oracle — a `HashMap`-backed node set would fail this).
6. **Snapshot round-trip.** `save_state`→`restore_state` preserves the pending schedule, standing
   faults, **the held-`Reorder` buffer, and the next monotonic `seq`** exactly — including a snapshot
   taken while a `Reorder` frame is held (restore must yield identical future RX ordering);
   `save_state` is byte-identical across two equally-driven switches; malformed restore errors
   cleanly.

## Non-goals

The `net_tx` hypercall exit handler, the RX ring, raising the pv-NIC IRQ via the lapic timer,
guest-memory copies (all **frontier**, vmm-core); the pv-NIC guest driver; the `Environment`
itself (task 24); validating that a real Linux TCP stack replays under V-time (gated on a guest
OS — see `docs/DISSONANCE.md` "What is still open"). Keep L2 handling minimal: address↔node
routing + broadcast fan-out; do not implement a full ARP/bridge state machine.
