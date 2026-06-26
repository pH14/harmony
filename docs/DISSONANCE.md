# Dissonance — the deterministic bug finder

This is the design ruling for **dissonance**. It covers the control plane and the fault model as
one model: a fault is just the guest's environment answering a service non-nominally, so the
two are the same machinery.

## Naming

- **harmony** — the whole project.
- **consonance** — the deterministic hypervisor: a single-vCPU KVM VMM that runs a real Linux
  guest with bit-identical replay (virtual time from a retired-branch counter, hypercall-only
  I/O, copy-on-write snapshot/branch). The substrate. Its crates live in `consonance/`.
- **dissonance** — the bug finder built on consonance (this doc). It drives the guest through
  many **environments**, injecting faults, to make real software misbehave — and because the
  substrate is deterministic, every bug it finds reproduces exactly. Its crates live in
  `dissonance/`.

## What dissonance is

Dissonance treats a running guest as a black box and asks: *under what conditions does it break?*
It supplies the guest's entire environment — entropy, scheduling, payload, and **faults**
(dropped packets, failed disk writes, crashes, partitions) — and watches for crashes and
violated assertions. The search is either **seed-driven** (one seed → a whole run, FoundationDB
style) or **coverage-guided** (react to feedback, Antithesis style); both produce the same
reproducible artifact.

## Architecture: two planes and an explorer

| | Out-of-band control plane | In-band SDK plane | The explorer |
|---|---|---|---|
| **Role** | dissonance drives consonance as a black box | the guest cooperates: pulls entropy/payload, pushes assertions/coverage | all of policy |
| **Transport** | unix socket (`control-proto`) | guest hypercalls (`hypercall-proto` + the Antithesis SDK) | — |
| **Initiator** | the explorer | the guest | — |
| **Verbs / API** | `snapshot`/`branch`/`replay`/`run`/`hash` | `entropy`/`event`/assertions/lifecycle | corpus, scoring, mutation |

**A fault is a non-nominal answer at an existing service point.** A guest's only environment
surfaces are these two planes and the substrate, so that is the entire shape a fault can take —
the realization that drives the rest of this document.

## The Environment

The guest runs against an **`Environment`** — the thing that answers every question the guest
cannot answer for itself: entropy, scheduling, payload, **and faults**. A fault is just an
environment that answers a service **non-nominally** ("EIO" instead of "ok"; "dropped" instead
of "delivered").

Faults live in the existing pieces: fault **mechanism** lives in the services (each owns its
non-nominal answer vocabulary — only the block service knows what a torn write is); fault
**policy** lives in the explorer. They meet at exactly one seam, consulted by a service *before
any side effect*:

```rust
fn handle_block_read(&mut self, req: BlockReq, env: &mut dyn Environment) -> BlockResp {
    let pt = DecisionPoint::BlockIo { op: BlockOp::Read, lba: req.lba, len: req.len };
    match env.decide(&pt) {                                  // -> Outcome (task 24)
        Outcome::Resolved(Answer::Nominal)             => self.read_real(req),       // happy path
        Outcome::Resolved(Answer::Fault(BlockEio))     => BlockResp::Error(EIO),
        Outcome::Resolved(Answer::Fault(BlockTorn(n))) => self.read_partial(req, n),
        Outcome::NeedsHost                             => self.suspend_for_explorer(), // reactive
        /* … */
    }
}
```

An `Environment` has two backings, and both replay bit-for-bit:

- **`Seeded(u64)`** — a PRNG answers every decision locally, no host round-trip. Pure
  seed-driven exploration (FoundationDB `BUGGIFY`).
- **`Recorded { seed, overrides }`** — the seed auto-answers the high-frequency decisions; the
  explorer's sparse overrides pin the interesting faults. This is what a coverage-guided session
  records, and it *is* the reproducer.

The control plane carries an `Environment` as an **opaque, versioned blob** — it never parses
the structure (that is the `environment` crate's contract with the services and the explorer).
This is what lets the control plane be fixed independently of the fault catalog.

## The two loops: Timeline and Multiverse

| | **Timeline** (inner) | **Multiverse** (outer) |
|---|---|---|
| **Unit** | one *decision* | one *run* (an `Environment`) |
| **Scope** | forward through one deterministic execution | coverage-guided search across runs |
| **Verbs** | `run` ⇄ `run(resolve)` | `branch`/`snapshot`/`replay`/`hash`/`drop` |
| **Produces** | a finished run + its recorded `Environment` | corpus growth; the next environment |

A **Timeline** drives one run to a terminal stop, answering each surfaced decision; the answers
accumulate into the `Environment` that reproduces it. The **Multiverse** picks or mutates an
environment, branches, runs one Timeline, scores coverage novelty and assertions, and chooses
what to try next. **One Multiverse step = one Timeline.** In seeded mode the Timeline has zero
stops (the seed answers everything), so a pure seed-driven campaign is the Multiverse alone.

A `snapshot` taken at a **quiescent** point (snapshots are quiescent-only) becomes a base the
Multiverse forks two ways — `branch(s, env_drop)` + `branch(s, env_deliver)`, two `Environment`s
that answer the interesting decision differently; each replays from the base to that decision and
diverges there. This is the one place the loops interlock, growing a tree of timelines from a
single moment — without ever snapshotting while a decision is armed.

## The control plane

A small, explicit verb set over a versioned, length-delimited request/response socket:

| Verb | Returns | Meaning |
|---|---|---|
| `hello(caps)` | `Caps` | negotiate protocol/blob versions + coverage geometry |
| `snapshot` | `SnapId` | capture state at a quiescent point (pool-wide handle) |
| `drop(snap)` | `()` | release a snapshot (corpus GC) |
| `branch(snap, env)` | `()` | restore + reseed from `env` — explore a new future |
| `replay(snap)` | `()` | restore verbatim — reproduce / determinism gate |
| `run(until, resolve?)` | `StopReason` | advance; `resolve` answers the prior `Decision` |
| `hash(scope)` | `[u8;32]` | canonical state digest (the determinism primitive) |

Two rules carry the safety properties:

- **No bare `restore`.** Every restore is `replay` (verbatim — the repro/gate path) or `branch`
  (reseed — the explore path), so the reproduce-vs-diverge choice is explicit at every call site.
- **Two result categories, fail-loud.** A guest-observable outcome is a `StopReason`
  (`Crash`/`Quiescent`/`Deadline` always present; `Decision`/`Assertion`/`SnapshotPoint` present
  with a cooperating SDK). A VM/transport failure is a `ControlError`. Never report one as the
  other.

Single-vCPU determinism makes the reactive path trivial: the lone vCPU parks on a decision, so
**exactly one decision is ever outstanding** — `run` surfaces it, `run(resolve)` answers it and
continues (the suspended hypercall is re-entered with the staged answer). A `StopMask` carried in
each `run`'s `StopConditions` (task 25) decides which decision *classes* surface; everything else
the seed answers locally.

## The fault model

The catalog is a small, versioned enumeration of **decision classes** (network-send, block-io,
entropy, scheduler, payload, process) and the **faults** eligible per class. The vocabulary is
convergent across the field (FoundationDB, Antithesis); the only hard problem was *locus* —
where a fault is physically applied.

**Network locus: host-side `pv-net`.** Because single-vCPU determinism rules out one-VM-per-node,
the "nodes" of a distributed system are containers in one guest, and inter-node traffic is
guest-internal. We route it through a `net_tx` hypercall to a **host L2 switch**, so the host
sees every frame and applies faults host-side. The switch schedules
delivery in **V-time**, and **every network fault is an operation on that schedule:**

| Fault | Effect on the delivery schedule |
|---|---|
| deliver (nominal) | one RX event at `T + L₀` |
| drop | no event |
| delay(d) | one event at `T + L₀ + d` |
| reorder / duplicate / corrupt | reassigned / doubled / byte-flipped events |
| partition(a↔b, window) | standing policy: drop on that link in the window |

This is determinism-clean because decide, enforce, and schedule are all host-side in V-time, and
the guest's own TCP timers ride the existing V-time-backed time surface — the contract's deterministic
TSC / LAPIC-timer / PIT / CMOS — **not** a PV clock, whose leaves/MSRs the CPU/MSR contract denies to
close host-time leakage. The block and process faults are likewise host-natural (block I/O is already
a host-serviced hypercall; crash/restart is snapshot/branch at a V-time).

## What is still open

- **"Real TCP replays under V-time"** is the load-bearing assumption behind `pv-net`. It needs a
  guest OS whose timers ride the V-time-backed TSC/LAPIC/PIT/CMOS surface (the contract denies a PV
  clock) to validate (same frames at same V-times → identical schedule → identical guest state).
  Until then `pv-net` is gate-tested with synthetic frames.
- **The decision-class taxonomy** is the one contract shared between the control plane (which
  names classes in `StopMask`) and the fault catalog (which defines them). Keep them in sync.

## Crates and tasks

| Crate | Builds | Task |
|---|---|---|
| `dissonance/environment` | the `decide` seam, the catalog, `SeededEnv`, the recorded-replay format | `tasks/24-environment.md` |
| `dissonance/control-proto` | the control-plane wire types + versioned codec | `tasks/25-control-proto.md` |
| `dissonance/pv-net` | the host L2 switch + V-time delivery scheduler + fault→schedule | `tasks/26-pv-net.md` |
| `dissonance/explorer` | the Timeline/Multiverse engine, corpus, scoring, strategy | `tasks/12-explorer.md` |

All four are pure-logic and laptop-gate-testable. The frontier glue — the socket server, the
reactive-suspension run loop, the `net_tx`/RX-IRQ wiring — lives in `consonance/vmm-core` and is
built against these crates.
