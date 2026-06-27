# Dissonance — the deterministic bug finder

This is the design ruling for **dissonance**. It models the whole permutation surface as one
thing: dissonance permutes a running system through two kinds of **control plane** — the **host
control plane** (the machine, workload-agnostic) and the **guest control planes** (the services
the guest requests, workload-defined and layerable). Every perturbation, from either plane, is
recorded into one reproducer keyed by a deterministic **`Moment`**, so every bug it finds replays
exactly.

> **Supersedes the single-seam framing.** An earlier version of this doc rested on *"a fault is
> just the guest's environment answering a service non-nominally."* That is true for the guest
> control planes but **cannot express** host-level perturbations (memory corruption, clock skew,
> CPU modulation) — there is no service point the guest raises for them. The model below has two
> planes feeding one reproducer; the single seam was the wrong altitude.

## Naming

- **harmony** — the whole project.
- **consonance** — the deterministic hypervisor: a single-vCPU KVM VMM that runs an **opaque**
  guest with bit-identical replay (virtual time from a retired-branch counter, hypercall-only I/O,
  copy-on-write snapshot/branch). The substrate. **It makes no assumption about what software runs
  inside it** — "a real Linux guest" is one instantiation, not part of its contract. Crates in
  `consonance/`.
- **`harmony-<env>`** — a **guest environment**: a self-contained guest world built on consonance
  (`harmony-linux`, `harmony-kubernetes`, `harmony-metal`, …). Each is a deterministic, stable
  layer that *inherits* replay from consonance and supplies the guest-level fault vocabulary,
  monitoring, and output. consonance never learns which one is running, and the guest planes
  **layer** (see below). `harmony-linux` is the first (it is what used to be `guest/`). *The
  `harmony-` prefix signals "pluggable guest world, not core engine"; the core tiers keep single
  musical names.*
- **dissonance** — the bug finder built on consonance (this doc). It permutes a guest through its
  control planes, injecting faults, to make real software misbehave — and because the substrate is
  deterministic, every bug reproduces exactly. Crates in `dissonance/`.
- **unison** — the determinism harness (replay-equivalence / `compare_runs` / `bisect_divergence`).

## What dissonance is

Dissonance treats a running guest as a black box and asks: *under what conditions does it break?*
It supplies the guest's entire environment — entropy, scheduling, payload, and **faults** — and
watches for crashes and violated assertions. The search is either **seed-driven** (one seed → a
whole run, FoundationDB style) or **coverage-guided** (react to feedback, Antithesis style); both
produce the same reproducible artifact.

The whole permutation surface is **two kinds of control plane**, split by one litmus test:

> **Does the guest have to *ask* for this for the fault to exist?**
> **No** → host control plane (you can do it to a guest that is just spinning).
> **Yes** → a guest control plane (it exists only because the guest invoked a service).

## The host control plane

consonance-level perturbations imposed on the **machine** — guest-oblivious, requiring no
cooperation, **identical for every `harmony-<env>`**. There is no service point: dissonance
applies them from outside, between instructions, at a chosen `Moment`.

```rust
enum HostFault {
    SkewTime(VTime),                       // jitter virtual time
    SetClockRate(Ratio),                   // CPU modulation: retired-branches → V-time slope
    CorruptMemory { gpa: u64, mask: BitMask }, // single-event-upset
    InjectInterrupt { vector: u8 },        // delivery-timing perturbation
}
```

This is the surface that "punches straight through to the hypervisor." It is **flat and universal**
— one host plane, no layering, the same knobs whether the guest is Linux, Kubernetes, or bare
metal. **Determinism payoff:** keying a host fault by `Moment` turns "random bit flip" into
"flip GPA `0x4000` bit 3 at instruction 1,234,567" — *reproducible*, because consonance is
deterministic. That reproducibility is the whole reason these route through dissonance rather than
being untracked chaos.

**Transport:** the host plane is driven over the **out-of-band control transport** (the
`control-proto` socket) — dissonance to consonance, the guest never sees the message.

## The guest control planes

Guest-level perturbations at a **service the guest requested**. Each is *defined by* an
`harmony-<env>`, and they **layer**: `harmony-kubernetes` declares `harmony-linux` as its base and
its catalog is the base's classes **⊕** its own. A fault is the environment answering a service
**non-nominally** ("EIO" instead of "ok"; "dropped" instead of "delivered"), at exactly one seam a
service consults *before any side effect*:

```rust
fn handle_block_read(&mut self, req: BlockReq, env: &mut dyn Environment) -> BlockResp {
    let pt = DecisionPoint::BlockIo { op: BlockOp::Read, lba: req.lba, len: req.len };
    match env.decide(&pt) {                                  // -> Outcome (task 24)
        Outcome::Resolved(Answer::Nominal)             => self.read_real(req),        // happy path
        Outcome::Resolved(Answer::Fault(BlockEio))     => BlockResp::Error(EIO),
        Outcome::Resolved(Answer::Fault(BlockTorn(n))) => self.read_partial(req, n),
        Outcome::NeedsHost                             => self.suspend_for_explorer(), // reactive
        /* … */
    }
}
```

**Layering.** Guest control planes form a dependency DAG bottoming at the bare machine. The
`harmony-<env>` crates depend on one another; class names are **namespaced per layer**
(`linux.net.drop`, `kube.net.partition`) and co-exist — a higher layer may *add* or *constrain* a
lower class but never silently reinterpret it. The host plane stays flat beneath the whole stack.
*(All the stacked guest planes composed together is informally "a chord" — a concept parked for
later; nothing depends on naming it yet.)*

**Cooperation gradient.** A guest plane spans how much the software cooperates:

- **Tier 1 — intercepted services** (unmodified software): faults at boundaries consonance already
  mediates (block / net hypercalls).
- **Tier 2 — SDK-cooperative** (the app links an SDK, Antithesis-style: `assert_always` /
  `assert_sometimes` / `assert_reachable`, `random()`, lifecycle): the **app itself** contributes
  fault points, assertions, and coverage. `assert_sometimes` hands the explorer part of its
  *objective* (drive the run until it fires); `assert_always` hands it a *bug oracle*. App logic
  enters through the same opaque seams the explorer already consumes (see "Theme is
  agnostic-by-interface"), so it enriches the vocabulary without growing the search policy. SDK
  surfaces **stack per layer** along with the catalog.

**Plane = decision *and* enforcement locus.** Every guest-plane fault is **decided by the host**
(the hypervisor answers a service the guest asked about, recorded by `Moment`) and **enforced by the
guest** (it acts on the answer — on the intra-guest CNI, the block layer, a process). The hypervisor
is never on the data path; it never *performs* a fault. *(An earlier model carried a
"Plane ≠ enforcement locus" exception — a guest-plane network fault enforced host-side on the
`pv-net` switch — but it existed **only** to justify `pv-net`, which task 50 retired; networking is
now a per-flow guest-plane decision enforced in-guest. See "Networking" below.)*

**Transport:** guest planes are surfaced **in-band** — the guest hits a service (a hypercall, or an
SDK call via `hypercall-proto`), parks, and dissonance answers nominally or not.

## The reproducer: one `Environment`, `Moment`-keyed

Both planes record into **one** artifact — the portable, genesis-complete reproducer:

```rust
type Moment = InsnCount;                   // single monotonic axis; V-time is a derived view
enum Action { Host(HostFault), Guest(Answer) }
struct Environment { seed: u64, overrides: BTreeMap<Moment, Action> }
```

A single `Moment` axis (retired-instruction count) is **load-bearing**: it puts host- and
guest-plane overrides on one ordered timeline, so the Theme can manipulate them uniformly
(`(Moment, opaque Action)`) without knowing which plane an override belongs to. Guest decisions are
stamped with the instruction count at which they surface; host perturbations are placed at a chosen
count.

An `Environment` has two backings, both replaying bit-for-bit:

- **`Seeded(u64)`** — a PRNG answers every decision locally, no host round-trip (FoundationDB
  `BUGGIFY`).
- **`Recorded { seed, overrides }`** — the seed auto-answers the high-frequency decisions; the
  explorer's sparse `overrides` pin the interesting faults (host *and* guest). This is what a
  coverage-guided session records, and it *is* the reproducer.

The control transport carries an `Environment` as an **opaque, versioned blob** — it never parses
the structure (that is the `environment` crate's contract with the services and the explorer),
which lets the transport be fixed independently of the fault catalog.

## The two loops: Variation and Theme

| | **Variation** (inner) | **Theme** (outer) |
|---|---|---|
| **Unit** | one *decision/perturbation* | one *run* (an `Environment`) |
| **Owns** | the *vocabulary* — `Action` (host ∪ guest planes) | the *search* over opaque `Environment`s |
| **Verbs** | `run` ⇄ `run(resolve)` ⇄ `perturb(HostFault @ moment)` | `branch`/`snapshot`/`replay`/`hash`/`drop` |
| **Produces** | a finished run + its recorded `Environment` | corpus growth; the next environment |
| **Grows when a fault is added?** | yes (+ catalog + codec) | **never** |

A **Variation** drives one run to a terminal stop, answering each surfaced guest decision and
applying any host perturbation at its `Moment`; the actions accumulate into the `Environment` that
reproduces it. The **Theme** picks or mutates an environment, branches, runs one Variation, scores
coverage novelty and assertions, and chooses what to try next. **One Theme step = one Variation.**
In seeded mode the Variation has zero stops (the seed answers everything), so a pure seed-driven
campaign is the Theme alone.

`snapshot` / `branch` are **Theme navigation, not perturbations** — they are not recorded into a
run's `Environment`. A `snapshot` at a **quiescent** point (snapshots are quiescent-only) becomes a
base the Theme forks two ways — `branch(s, env_drop)` + `branch(s, env_deliver)`, two
`Environment`s that answer the interesting decision differently; each replays from the base to that
`Moment` and diverges there. This is the one place the loops interlock, growing a tree of
variations from a single moment — without ever snapshotting while a decision is armed.

**The invariant (the boundary's litmus test):** *adding a fault type — a new `HostFault`, a new
guest decision class, a whole new `harmony-<env>` layer — grows **Variation + catalog + codec** and
touches **Theme** never.* If it forces a Theme change, the abstraction has leaked.

## Theme is agnostic-by-interface

The Theme is generic across exactly three opaque seams — it is structurally blind to fault
semantics but depends on these channels:

- **Navigation** — the opaque `Environment` blob + `SnapId` (`branch`/`replay`/`drop`).
- **Scoring** — an opaque coverage vector + oracle/`StopReason` events; `hello(caps)` negotiates
  coverage geometry. The Theme maximizes novelty over bits whose *meaning* is guest-defined.
- **Proposal** — delegated to the vocabulary-aware codec (`EnvCodec::seeded`/`mutate`/`compose`) +
  the published catalog; the Theme cannot *invent* a legal `HostFault`/`Answer`, so it asks the
  codec. Theme *policy* (select / score / branch-vs-restart / frontier GC) stays generic.

This is the precise sense of "agnostic": the search engine hardcodes no fault types; vocabulary
knowledge lives in the seams it calls. Composition (new layers) and the SDK (app-specific logic)
both enter through these seams — which is why they never touch the Theme.

## The control transport (verbs)

A small, explicit verb set over a versioned, length-delimited request/response socket — the
out-of-band channel the Theme uses to drive consonance (the host plane rides here; guest decisions
surface in-band and are *answered* via `run(resolve)`):

| Verb | Returns | Meaning |
|---|---|---|
| `hello(caps)` | `Caps` | negotiate protocol/blob versions + coverage geometry |
| `snapshot` | `SnapId` | capture state at a quiescent point (pool-wide handle) |
| `drop(snap)` | `()` | release a snapshot (corpus GC) |
| `branch(snap, env)` | `()` | restore + reseed from `env` — explore a new future |
| `replay(snap)` | `()` | restore verbatim — reproduce / determinism gate |
| `run(until, resolve?)` | `StopReason` | advance; `resolve` answers the prior `Decision` |
| `perturb(fault, moment)` | `()` | stage a host-plane `HostFault` at `moment` (recorded) |
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

## The guest fault model

A guest control plane's catalog is a small, versioned, **namespaced** enumeration of **decision
classes** (network-flow, block-io, entropy, scheduler, payload, process) and the **faults** eligible
per class; layers add classes (D7). The vocabulary is convergent across the field (FoundationDB,
Antithesis). The one hard problem was *locus* — where a fault is physically applied — and the rule
that resolves it is uniform: the **host decides, the guest enforces** (task 50). No guest-plane fault
is performed by the hypervisor.

`scheduler` is the telling boundary case the two-plane split resolves: black-box scheduling
perturbation is a **host**-plane interrupt-timing fault (`InjectInterrupt @ moment`); an
SDK-cooperative "which runnable thread next?" is a **guest**-plane decision. Same concept, placed by
cooperation level.

**Networking: a per-flow guest-plane decision, host-decided and guest-enforced (task 50).** Because
single-vCPU determinism rules out one-VM-per-node, the "nodes" of a distributed system are
containers/pods in **one** guest, and inter-node traffic transits the **intra-guest CNI** (bridge +
veth + netns) — it never leaves the guest (tasks 38/48/49). That traffic is *already deterministic*:
consonance determinizes the only two things that could make a guest network vary — the clock (guest
TSC/LAPIC = V-time) and entropy (`/dev/urandom` fed by the entropy hypercall). So the host neither
needs to *see* the traffic (it is intra-guest) nor to *enforce* determinism on it (the substrate
already does).

A network fault is therefore a **per-flow** decision, exactly like block I/O and entropy: a
harmony-linux guest utility asks the hypervisor *"what should I do with this flow?"* (`net_decide` —
a `NetFlow { src, dst, conn, event }` decision point), the hypervisor **answers** a flow-level policy
(recorded into the `Moment`-keyed `Environment` so it replays), and the utility **enforces** the
answer on the intra-guest CNI using Linux's own mechanisms. **One decision per flow/connection, not
per frame** — the host is in the *control* path (low-frequency, recorded), never the *data* path.

| Answer | Flow-level policy the guest enforces |
|---|---|
| `Nominal` | deliver normally |
| `NetLatency(d)` | add `d` of guest-time (V-time) delay — `netem` |
| `NetLoss { num, den }` | drop a `num/den` fraction, sampled from a seeded PRNG (`1/1` = full drop) |
| `NetThrottle { bps }` | cap bandwidth at `bps` — `tbf` |
| `NetReset` | refuse / reset the connection (a `RST`) |
| partition(a↔b, window) | **standing** link policy (drop all on the link in the V-time window), carried in `EnvSpec::Recorded.standing` and enforced guest-side (e.g. an nftables rule) |

Per-**message** faults (reorder / duplicate / corrupt a *specific* message) need message boundaries
the network layer cannot see; together with L2 byte-corruption they move to the **SDK / L7 tier** (a
later task) — deferred, not dropped.

This is determinism-clean by the **enforcement-determinism discipline**: because the enforcer runs
*in* the guest, it inherits the substrate's determinism **iff** it takes every input from a
determinized source — delays measured in **guest V-time**, random drops/loss from a **seeded** PRNG
(or the entropy hypercall), never a host wall-clock or unseeded host RNG. It *cannot* reach a
non-determinized source: consonance denies them (the CPU/MSR contract gives a deterministic
TSC/LAPIC/PIT/CMOS surface and no PV clock). Task 49 is the empirical proof: a full k8s network stack
runs intra-guest, deterministic-twice. The block and process faults follow the same host-decides /
guest-enforces shape (block I/O is a host-answered hypercall the guest acts on; crash/restart is
snapshot/branch at a `Moment`).

## What is still open

- **"Real TCP replays under V-time"** — now **validated end-to-end in the guest** (no host schedule
  to compose). Tasks 38/48/49 run real Linux TCP stacks (Postgres; a k3s cluster, pod-to-pod over the
  CNI) intra-guest and replay **deterministic-twice**, because the guest's timers ride the
  V-time-backed TSC/LAPIC/PIT/CMOS surface (the contract denies a PV clock) and entropy is seeded. The
  open frontier is now the **guest flow utility** itself (task 50 non-goal): *what* enforces a
  `NetFlow` policy on the CNI (tc-netem / nftables / a userspace L4 proxy) and the `net_decide`
  hypercall-service wiring.
- **The decision-class taxonomy** is the one contract shared between the control transport (which
  names classes in `StopMask`) and the guest fault catalog (which defines them). Keep them in sync.
- **Layer-conflict semantics** (D7): the exact rules for how a higher `harmony-<env>` layer adds vs.
  constrains a lower layer's classes are sketched (namespacing) but not pinned.
- **Host control-plane realization**: `HostFault` + `perturb` + uniform `Moment` stamping across
  both planes is specified here but not yet built (task 45). The existing `Environment` (task 24)
  covers the guest planes only.

## Crates and tasks

| Crate | Builds | Task |
|---|---|---|
| `dissonance/environment` | the **guest control-plane** `decide` seam, the catalog (incl. the per-flow `NetFlow` network seam), `SeededEnv`, the recorded-replay format | `tasks/24-environment.md`, `tasks/50-net-fault-boundary.md` |
| `dissonance/control-proto` | the control-transport wire types + versioned codec | `tasks/25-control-proto.md` |
| `dissonance/explorer` | the Variation/Theme engine, corpus, scoring, strategy | `tasks/12-explorer.md` |
| *(host plane)* | `HostFault` + `perturb` + uniform `Moment` stamping in consonance | `tasks/45-host-control-plane.md` |

> The host-side L2 switch crate `dissonance/pv-net` (task 26) was **retired** by task 50: it modeled a
> host-routed multi-VM topology the project does not use. Networking is now a per-flow guest-plane
> decision (`NetFlow`, owned by `environment`), enforced in-guest.

`environment`, `control-proto`, `explorer` are pure-logic and laptop-gate-testable. The
frontier glue — the socket server, the reactive-suspension run loop, the `net_decide`
hypercall-service + the guest flow utility that enforces a `NetFlow` answer on the CNI, and the
host-plane `perturb` enforcement — lives in `consonance/vmm-core` and is built against these crates.
