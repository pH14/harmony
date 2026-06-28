# Task 51 — `dissonance/flow`: the central flow-fault proxy engine (`FlowEngine` trait + `ToxiproxyEngine`)

> **NEW CRATE · pure-logic · the in-guest enforcer-brain for task 50's `net_decide` seam.** It turns
> per-flow fault *decisions* into a deterministic, V-time-scheduled stream of concrete connection
> actions. The socket proxy, the transparent redirect, and the `net_decide` hypercall wiring are
> **frontier** (built later against this crate).

Read `tasks/00-CONVENTIONS.md`, `tasks/50-net-fault-boundary.md` (the seam + ruling this enforces),
`tasks/24-environment.md` (the catalog the policy vocabulary mirrors), and `docs/DISSONANCE.md`
("The guest control planes" — host decides / guest enforces) first.

## Environment

Runs on: macOS and Linux. Requires: Rust (stable). Does **not** require `/dev/kvm`, a guest OS,
sockets, root, or real networking. Pure-logic — the engine is exercised with **synthetic connections**
(scripted byte chunks at fake V-times), a **fake clock**, and a **scripted decider**. Fully
gate-testable on a laptop. *(This is the property the design optimizes for: the proxy's fault
behavior is tested directly, without running inside consonance.)*

## Context

Task 50 moved network-fault **enforcement** into the guest: the hypervisor **decides** per flow
(`net_decide`), a guest utility **enforces**. That utility is **one central L4 proxy** inside the
guest that all inter-node traffic routes through — per-pod sidecars would be re-implementing
service-mesh plumbing that belongs to a future `harmony-kubernetes`, out of scope; the central proxy
is a `harmony-linux` concern. It is **toxiproxy-shaped**: a connection arrives, the engine asks "what
should I do with this flow?", and applies the answer by transforming the byte stream (delay / drop /
throttle / reset).

This crate is the proxy's **engine** — the pure-logic core, decoupled from sockets and the hypercall.
Per the integrator's ruling, the engine is **pluggable**: a `FlowEngine` **trait** captures the
contract, and `ToxiproxyEngine` is the implementation we ship, so a different fault model can slot in
later without touching the proxy shell or the seam.

## Public API

```rust
// ---- shared vocabulary (every FlowEngine impl uses these) ----
pub struct VTime(pub u64);   // branch count; the only deterministic clock
pub struct ConnId(pub u64);
pub struct NodeId(pub u32);
pub enum Dir { ClientToServer, ServerToClient }

/// One event on the proxied connection stream. In tests: hand-scripted; in the frontier shell:
/// produced from real `accept`/`read` on the central proxy. Bytes are guest-controlled — handling
/// must never panic (rule 4).
pub enum FlowEvent {
    Open  { conn: ConnId, src: NodeId, dst: NodeId },
    Chunk { conn: ConnId, dir: Dir, at: VTime, bytes: Vec<u8> },
    Close { conn: ConnId, at: VTime },
}

/// What the proxy must physically do, drained by V-time (the frontier shell enacts these on sockets).
pub enum FlowAction {
    Deliver { conn: ConnId, dir: Dir, bytes: Vec<u8>, at: VTime },
    Reset   { conn: ConnId, at: VTime },
}

/// The per-flow policy an engine applies — the decider's answer. Mirrors task 50's `NetFlow` fault
/// vocabulary; defined locally (rule 2); the frontier maps `environment::Answer` → this.
pub enum FlowPolicy {
    Nominal,
    Latency(VTime),                        // delay each chunk's delivery by d V-time
    Loss { seed: u64, num: u16, den: u16 },// drop each chunk w.p. num/den from a SEEDED prng
    Throttle { bps: u32 },                 // pace bytes at bps, in V-time
    Reset,                                 // tear the connection down
}

/// The decision seam (local; the frontier binds it to the `net_decide` hypercall →
/// `environment::decide`, which records the answer into the Moment-keyed Environment). In tests: a
/// scripted/recording fake. The engine decides *when* to consult it (see `FlowEngine`).
pub trait FlowDecider {
    fn decide_flow(&mut self, conn: ConnId, src: NodeId, dst: NodeId) -> FlowPolicy;
}

// ---- the pluggable engine ----
/// A flow-fault engine: `FlowEvent`s + per-flow decisions in → a deterministic, V-time-scheduled
/// stream of concrete `FlowAction`s out. The **contract** is here; the **mechanism** is the impl's.
///
/// Contract (every impl; asserted by the trait-generic gates):
/// - **Deterministic** given (engine state, event sequence, decider answers): identical inputs ⇒
///   identical `FlowAction` sequence — byte-for-byte, including order.
/// - **V-time-drained**: actions surface only through `due(now)`, at/before `now`; ties broken by a
///   deterministic monotonic seq, never by map-iteration order (`BTreeMap`; no `HashMap` into an action).
/// - **Total on guest input**: any `Chunk.bytes`, or an event for an unknown/closed `ConnId`, is
///   handled deterministically (the stray event is ignored — never a panic; the
///   `environment`-override discipline) .
/// - **Saturating V-time**: `at + d` and every scheduled time saturate (a hostile `Latency(u64::MAX)`
///   or `now` near `u64::MAX` clamps, never wraps to deliver in the past — rule 4).
pub trait FlowEngine {
    /// Feed one connection event. An impl consults `decider` when it needs a flow's policy (the
    /// toxiproxy impl: once on `Open`), schedules any resulting deliveries, and returns. Infallible
    /// and deterministic; a stray event for an unknown conn is deterministically ignored.
    fn on_event(&mut self, ev: FlowEvent, decider: &mut dyn FlowDecider);
    /// Pop every action due at or before `now`, in deterministic order.
    fn due(&mut self, now: VTime) -> Vec<FlowAction>;
}

/// The engine we ship — toxiproxy toxic semantics. Consults the decider **once per flow on `Open`**;
/// `Latency` delays each chunk's delivery by d; `Throttle` paces bytes at `bps` in V-time; `Loss`
/// drops each chunk with prob `num/den` from a **per-conn seeded** prng (seeded from `Loss.seed` —
/// never the ambient one, so replay is exact); `Reset` schedules a `Reset` and drops the rest.
pub struct ToxiproxyEngine { /* per-conn policy + BTreeMap<(VTime, seq), FlowAction> + per-conn Prng + next seq */ }
impl ToxiproxyEngine { pub fn new() -> Self; }
impl FlowEngine for ToxiproxyEngine { /* … */ }

/// The trivial reference engine: every flow `Nominal`, every chunk delivered verbatim, decider never
/// consulted. It is the **faults-off baseline** (the recovery / `finally_` case) **and** the proof
/// that `FlowEngine` abstracts over more than toxiproxy (the pluggability the design requires).
pub struct PassthroughEngine { /* per-conn open/closed lifecycle so strays are ignored — no policy, no PRNG */ }
impl PassthroughEngine { pub fn new() -> Self; }   // + `Default`
impl FlowEngine for PassthroughEngine { /* live flow: deliver verbatim at `at`, Reset on Close; an unknown/closed conn is a stray and is ignored, per "Total on guest input" */ }
```

Provide a documented seeded PRNG (reuse the `hypercall-proto` xorshift64\* algorithm, defined locally
— rule 2) for `Loss`. No `rand`, no wall-clock, no float (rule 4).

## Determinism

- The engine state lives in **guest RAM** (the proxy is a guest process), so consonance snapshots /
  branches it **for free** — there is **no `save_state`/`restore_state`** (the win over the retired
  `pv-net`, whose host-side state needed explicit serialization). Replay determinism is proven by
  re-running the event sequence, not by serializing state.
- `Loss` rolls from a per-conn PRNG seeded by the (recorded) decision — same decision ⇒ same drops.
- Multiplexed connections are serviced in a deterministic order (`(VTime, seq)` keys), never by
  incidental map/iteration order — the lesson carried over from `pv-net`'s delivery scheduler.

## Acceptance gates

Beyond the standard suite:

1. **Trait-generic determinism (run against EVERY impl — `ToxiproxyEngine` + `PassthroughEngine`).**
   Identical `(event sequence, decider)` ⇒ identical `FlowAction` sequence from `on_event`/`due`.
   Property test, ≥256 cases, parameterized over the engine — the proof the trait's contract holds for
   more than one implementation.
2. **No panic on guest input (fuzz).** `on_event` on arbitrary/truncated/mutated `FlowEvent` (incl.
   arbitrary `Chunk.bytes`, an unknown/closed `ConnId` on `Chunk`/`Close`) never panics and is
   deterministically ignored when stray; provide a `cargo-fuzz` target. Edge V-times
   (`Latency(u64::MAX)`, `now` near `u64::MAX`) saturate — asserted to never debug-panic or release-wrap.
3. **Per-policy golden (`ToxiproxyEngine`).** Hand-written expected action schedule for each
   `FlowPolicy`: `Nominal`→deliver at `at`; `Latency(d)`→`at+d`; `Throttle{bps}`→the paced delivery
   times; `Loss{seed,n,d}`→the exact kept/dropped chunk set for a known seed; `Reset`→a `Reset`
   action then no further deliveries. Pins the toxic semantics + the PRNG.
4. **`PassthroughEngine` is nominal.** Every chunk delivered verbatim at `at`; the decider is **never**
   consulted (assert against a recording decider) — the faults-off baseline.
5. **Decider-driven (`ToxiproxyEngine`).** The decider is consulted **exactly once per flow, on
   `Open`**, and in deterministic order across multiplexed flows (assert the draw order against a
   recording decider — a `HashMap`-backed conn set would fail this).
6. **No order leakage.** A test asserts no `HashMap`/`HashSet` iteration reaches a `FlowAction` or its
   order (`BTreeMap`/sorted, asserted).

## Non-goals

- **The proxy shell (frontier):** the real `accept`/`splice` TCP proxy, the **transparent redirect**
  that routes inter-node traffic through the one central proxy (iptables REDIRECT / a CNI hook), the
  `FlowDecider` impl that issues the `net_decide` hypercall and maps `environment::Answer` →
  `FlowPolicy`, and the enacting of `FlowAction`s on real sockets. Built later against this crate.
- **Per-pod sidecars** (that is `harmony-kubernetes`); **per-message / L7 faults** (slicer, byte
  corruption — the SDK/L7 tier, a later task); the `net_decide` wire types (task 50 / `environment`).
- Any `docs/DISSONANCE.md` edit — task 50 owns the network-section rewrite; this crate is referenced
  from there (a `dissonance/flow` Crates-table row) when that rewrite lands.
