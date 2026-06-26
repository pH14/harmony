# Task 44 — the host control plane: `HostFault` + `perturb` + uniform `Moment` stamping

> **FOLLOW-ON to the `docs/DISSONANCE.md` ruling · DO NOT AUTO-SPAWN until that lands.** This builds
> the half of dissonance's permutation surface the single-seam `Environment` (task 24) never
> covered: substrate-level faults with no guest service point.
>
> **Note:** `dissonance/environment` is **already merged** (task 24, with a `public_api.txt`
> golden + codec/replay tests). Widening its recorded value type from the guest `Answer` to
> `Action = Host | Guest` is a **breaking public-API change** — update the snapshot and the
> codec/replay/mutation tests in the same PR. This is an amendment, not a greenfield crate.

Read `tasks/00-CONVENTIONS.md` and `docs/DISSONANCE.md` ("The host control plane", "The reproducer",
"Theme is agnostic-by-interface") first.

## Why

The guest control planes (task 24) only model faults the guest *asks* for. The **host control
plane** is the workload-agnostic, guest-oblivious surface — memory corruption, clock skew, CPU
modulation, interrupt-timing — that dissonance imposes on the machine from outside. It must record
into the **same** `Environment` as guest faults, on a **single `Moment` axis**, so the Theme stays
plane-agnostic.

## Public API (contract)

```rust
type Moment = u64;                         // retired-instruction count; V-time is a derived view

pub enum HostFault {
    SkewTime(VTime),
    SetClockRate(Ratio),                   // integer/fixed-point only — no float (rule 4)
    CorruptMemory { gpa: u64, mask: BitMask },
    InjectInterrupt { vector: u8 },
}

pub enum Action { Host(HostFault), Guest(/* task-24 Answer */) }

/// Extends the task-24 reproducer: overrides are keyed by Moment, values are either plane.
pub struct Environment { pub seed: u64, pub overrides: BTreeMap<Moment, Action> }
```

Plus the control-transport verb (wire type in `control-proto`, task 25):
`perturb(fault: HostFault, at: Moment) -> Result<(), ControlError>` — stage a host fault at a
`Moment`, recorded into the active `Environment`.

## What to do

1. Define `HostFault` / `Action` / the `Moment`-keyed `Environment` (coordinate with task 24 so the
   guest `Answer` slots into `Action::Guest`; `compose`/`mutate`/`seeded` from the `EnvCodec` operate
   over the merged map).
2. Stamp **every** override with a `Moment` (retired-instruction count): guest decisions with the
   count at which they surface; host faults at the chosen count. This is the load-bearing
   unification — without it the Theme must know an override's plane to order it.
3. The enforcement glue (apply a `HostFault` at its `Moment` during a `run`) lives in
   `consonance/vmm-core` against this crate; this task delivers the **types + codec + stamping**,
   laptop-gate-testable.

## Determinism

`SetClockRate`/`SkewTime` must be integer/fixed-point (no float reaching state — rule 4).
`CorruptMemory` at a `Moment` is a pure function of `(Moment, gpa, mask)`; replaying an
`Environment` re-applies the identical overrides at the identical counts → bit-identical. Property
test: `replay(record(env)) == env`'s run, with host overrides present.

## Acceptance gates

Standard suite on the crate(s); a proptest (≥256 cases) that a mixed host+guest `Environment`
replays bit-identically and that `EnvCodec::compose` re-keys `Moment`s correctly (one-axis
arithmetic — see task 93). No `Theme`/explorer policy change is required to consume `HostFault`
(the D4 invariant — verify by inspection, note it in `IMPLEMENTATION.md`).

## Non-goals

The in-`vmm-core` enforcement of each `HostFault` (frontier glue — separate); new fault *classes*
beyond the four; any explorer-policy change.
