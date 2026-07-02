# Task 73 — Phase H: the harmony guest SDK + link-tier decode

> **FRONTIER · Phase H of `docs/EXPLORATION.md`.** The link tier — Tier 2 of `docs/DISSONANCE.md`'s
> cooperation gradient — is vocabulary only today: task 64 pins `RunTrace.events` "empty until
> task 73", `StopReason::Assertion` has never been produced, and no guest has ever emitted an SDK
> event. This task builds both halves: the **guest SDK** (assertions, catalog-at-init, IJON state
> registers, buggify, lifecycle — hooks + transport ONLY, per the thin-SDK ruling below) and the
> **host-side link-tier decode** that populates `RunTrace.events: Vec<(Moment, GuestEvent)>`.
>
> Depends on **task 58** (server + socket `Machine`) and **task 64** (the spine vocab this
> populates). Coordinate with **65** (the recorder channel `events` rides off the box) and **66**
> (the shared catalog/never-fired report format); independent of 59/60. **Shared seam with 61:**
> vmm-core does not yet service the hypercall doorbell — whichever of 61/73 lands first wires
> doorbell→`Dispatcher`; the foreman sequences.

Read first: `tasks/00-CONVENTIONS.md`, `docs/EXPLORATION.md` ("The three signal tiers" — the link
tier — and the Phase H roadmap row), `docs/DISSONANCE.md` ("The guest control planes" — the Tier-2
SDK — and "The control transport"), `tasks/64-explorer-spine-refactor.md` (`RunTrace`, `Feature`,
the `Archive`'s checkpoint-candidate admission), `tasks/01-hypercall-proto.md` +
`consonance/hypercall-proto/src/lib.rs` (`ServiceId::Event = 4`, `event_emit`, `EventSink`),
`tasks/10-vmcall-transport.md` + `tasks/20-io-doorbell.md` + `consonance/vmcall-transport/src/lib.rs`
(the guest doorbell, `DOORBELL_PORT = 0x0CA1`, the `LoopbackHost` test seam),
`tasks/61-net-vertical.md` (the in-guest agent precedent; `ServiceId::Net = 5` is reserved by it),
`dissonance/environment/src/` (`catalog.rs` — `DecisionClass`, supply vs fault classes;
`seeded.rs` — the domain-separated supply/fault PRNG streams; `policy.rs` — `FaultPolicy`),
`guest/payloads/` (the no_std guest workspace conventions).

## Environment

Portable-logic surface (macOS + Linux, laptop-gated): the SDK crate itself (no_std, loopback-tested
against `hypercall_proto::Dispatcher` exactly as `vmcall-transport` is), the `dissonance/link`
decode/catalog/sensor crate, the additive `dissonance/environment` and `hypercall-proto` changes —
all mock-testable with no `/dev/kvm`. **Box-only:** baking the SDK-instrumented demo guest into an
image and the three live gates (patched KVM). Pin per `docs/BOX-PINNING.md`; always revert KVM to
stock **1396736** and verify after any patched run.

Surface list (frontier waiver of hard rule 1):

- **`guest/sdk/`** (new; `harmony-linux/sdk/` if task 43 has landed) — the SDK crate; plus a small
  SDK-instrumented demo payload under `guest/payloads/` and its image/Makefile wiring.
- **`dissonance/link`** (new) — the link-tier plugin: event decode, the assertion catalog +
  never-fired report, the link `Sensor`, the `AlwaysViolation` oracle.
- **`dissonance/explorer`** — only filling the task-64 `GuestEvent` vocab stub (coordinate with 64).
- **`dissonance/environment`** — additive: `DecisionClass::Buggify = 7`, `Fault::BuggifyFire`,
  per-point `FaultPolicy` biasing (codec version bumps per the crate's rules).
- **`consonance/hypercall-proto`** — additive: `ServiceId::Sdk = 6` (`buggify_decide`; 5 is 61's).
- **`consonance/vmm-core`** — **read-only except three named seams**: (1) doorbell→`Dispatcher`
  servicing if task 61 has not landed it (on main the doorbell is unserviced — vmm.rs's VMCALL
  handler is explicitly "a later phase"); (2) a `Moment`-stamped `EventSink` capture in the task-58
  server + flipping the `GUEST_HAS_SDK` cap flag; (3) run-loop stop surfacing: always-violation /
  unreachable-reached → `StopReason::Assertion`, `setup_complete` → `StopReason::SnapshotPoint`,
  and the `Sdk` service answering `buggify_decide` via `Environment::decide`, recorded at its
  `Moment`. Nothing else in vmm-core moves.

## Context — the thin-SDK ruling (load-bearing)

**THE RULING (Paul, 2026-07-01):** the SDK provides **hooks + transport only**. No checkers, no
policy in the guest; Elle/history checkers live at the evaluator layer (task 75). What the guest
contributes is *identity and observation* — named points, their firings, numeric state — and the
host owns every interpretation. Consequences: the link tier is tunable in **interpretation only** —
what's emitted is fixed at guest build (`docs/EXPLORATION.md`'s instrument-tier asymmetry) — and
the scrape-first acquisition order stands: this is Tier 2, a channel for code you own, not a
displacement of the 65/66/67 scrape channel.

**Form: a Rust `no_std` crate**, generic over `hypercall_proto::Transport`. Justification: the
guest tier is already Rust (`guest/payloads` is a no_std workspace; `vmcall-transport` is the
purpose-built no_std guest shim, so `Client<VmcallTransport>` composes with zero new transport
code), and the first code-we-own consumers (the demo payload here; task 69's seeded-bug workload)
are Rust. A C-ABI header shim for foreign workloads is deferred (non-goal); Linux-userspace page
mapping follows the task-61 flow-agent convention when it lands.

## What to build

1. **The SDK verbs** (`guest/sdk`): `init(transport, catalog)` registers the **declared point set**
   at startup (one Emit; each point = stable id + name + kind); `assert_always(cond, point)` /
   `assert_sometimes(cond, point)` / `assert_reachable(point)` / `assert_unreachable(point)`;
   IJON-style numeric registers `state_max(reg, v)` / `state_set(reg, v)`; lifecycle
   `setup_complete()`. All emissions ride the **existing Event service** (`ServiceId::Event = 4`,
   op 1) under a versioned, byte-deterministic payload convention documented in the crate — task 74
   (OTel) rides these same transport conventions. The SDK owns a registry of event-id namespace
   ranges (assertions / state registers / buggify / reserved-for-plugins, e.g. 74's otel chunks) so
   channel plugins allocate ids without collision. Always emits only on violation; sometimes emits
   on **every** hit (features are a timestamped stream, task 64). The host stamps each emission at
   the `Moment` it surfaces; the guest never timestamps. **No `random()` is built**: the Entropy
   hypercall (`Client::entropy_fill`, host `SeededEntropy` — the seeded stream RDRAND draws) is
   already the guest-random primitive; cite it, re-export at most.
2. **Buggify = a `DecisionClass` on the FAULT stream** (`dissonance/environment` + `ServiceId::Sdk`):
   `buggify(point) -> bool` asks the host (`DecisionPoint::Buggify { point }`); the host answers
   `Nominal` (don't fire) or `Fault(BuggifyFire)`, recorded at its `Moment` like every guest-plane
   decision. Draws come from `SeededEnv`'s **fault** PRNG stream (the `FAULT_DOMAIN`-separated one,
   `seeded.rs`) — never entangled with the workload's entropy **supply** stream. Point identity is
   the design: **catalog-registered points, per-point host-side biasing** (a `FaultPolicy`
   extension keyed by point id — the guest never sees probabilities), and **never-fired detection**
   in the catalog report — the deliberate improvement over FoundationDB's anonymous `get_random`
   (a buggify site is a named, steerable, auditable coordinate, not an anonymous coin flip).
3. **Host-side link-tier decode** (`dissonance/link` — the decode lives here, a plugin crate beside
   65's recorder, depending on `explorer` for the spine vocab per task 64's rule-2 layout): raw
   `(Moment, event_id, bytes)` tuples → typed `(Moment, GuestEvent)` into `RunTrace.events`
   (capture rides the same recorder channel task 65 gives `records`; if 65 is unlanded, a minimal
   run-end artifact addition is in-surface — name it in the PR). Plus: the **catalog fold**
   (declared-at-init set + fired counts → the never-fired report, format **unified with task 66's
   config-declared catalog** — the declared signal set *is* the catalog, one report across link and
   scrape); the **link `Sensor`** (an `assert_sometimes` hit or a state-register change ⇒
   `(Moment, Feature)` into the feature stream; `Archive` admission still requires a novel
   `(cell, Moment)` — task 64 semantics — so per-hit checkpoint candidacy requires the
   campaign's `CellFn` config to include the sometimes channel);
   the **`AlwaysViolation` `Oracle`** (`StopReason::Assertion` terminal ⇒ `Bug` with
   genesis-complete env and stable fingerprint). Decode is total and panic-free on arbitrary bytes.
4. **Run-loop surfacing** (vmm-core, the named seams only): always-violation and
   unreachable-reached stop the vCPU as `StopReason::Assertion`; sometimes hits do **not** stop by
   default (`StopMask`-gated — the replay plane consumes them); `setup_complete` surfaces
   `StopReason::SnapshotPoint`.

## Prior art

- **IJON (S&P 2020) [eng]** — the annotation interface (`state_max`/`state_set`); the literature is
  unanimous that a few state annotations beat any amount of blind coverage.
- **FoundationDB / BUGGIFY (Strange Loop 2014) [eng]** — the buggify design, minus the anonymity:
  our catalog identity (registration, per-point biasing, never-fired audit) is the improvement.
- **AFLGo (CCS 2017) [eng]** — a declared-but-unhit sometimes-assertion is a directed-search
  target; that mode is a task-70 follow-on, not built here.

## Acceptance gates

1. **Standard suite** green on every touched crate (macOS + Linux); the SDK crate builds for
   `x86_64-unknown-none` (the no_std proof, per the vmcall-transport gate).
2. **Portable decode + catalog proptests (≥256)** over synthetic event streams: decode never
   panics; the catalog fold books declared/fired correctly; a sometimes hit yields the right
   `(Moment, Feature)` and is admitted as a checkpoint candidate by the spine `Archive` on the toy;
   a planted always-violation makes `AlwaysViolation` mint a `Bug` with a stable fingerprint. The
   never-fired report round-trips in task 66's format (or the minimal shared type, noted for 66).
3. **Portable stream-separation golden:** enabling buggify sampling leaves the supply stream
   byte-identical to buggify-off, and per-point biasing reproduces a golden draw sequence.
4. **Box gate A (determinism):** the SDK demo guest, same seed twice ⇒ **byte-identical decoded
   `(Moment, GuestEvent)` stream** and equal `state_hash`.
5. **Box gate B (the Bug path):** a planted, buggify-gated always-violation surfaces
   `StopReason::Assertion` ⇒ `Bug`; `branch(genesis, bug.env)` replays the violation **N/N, N ≥ 8**.
6. **Box gate C (never-fired):** two declared sometimes points, one wired to fire ⇒ the report
   flags the other as never-fired; the fired one appears as a `(Moment, Feature)`.

## Box-safety (CRITICAL)

Stock KVM = **1396736**; the patched module is larger. ALWAYS leave the box on stock + verified after
every run: `pkill -9 -f sdk_` (and any `live_*`) FIRST → wait `lsmod | grep '^kvm_intel'`
users=0 → `rmmod kvm_intel kvm; modprobe kvm; modprobe kvm_intel` → verify size 1396736 on a FRESH
ssh connection. SSH drops (exit 255) on pkill/rmmod are normal — reconnect + verify. Pin builds/tests
to `taskset -c 2` (`docs/BOX-PINNING.md`). Run gates in the foreground and READ results before
reporting; no detached pollers + idle.

## Non-goals

- **Elle / history checkers in the guest** — evaluator layer, task 75 (the thin-SDK line). The SDK
  may *emit* an operation history over the Event service; it never checks one.
- The in-guest OTel bridge — task 74 (it reuses this task's Event-service transport conventions).
- **AFLGo-style directed search toward unhit assertions** — a task-70 follow-on (the never-fired
  report is its input); name it, don't build it.
- The instrument/coverage tier (SGFuzz-style) and any coverage-shmem wiring.
- `SkewTime`-style host faults (task 59's plane) and any new guest-plane fault beyond `BuggifyFire`.
- A C-ABI header shim for foreign-language workloads — follow-on; foreign software is the scrape
  tier's job first.
