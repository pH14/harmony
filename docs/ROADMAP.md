# Roadmap — what's left, and who builds it

Wave 1 (tasks 01–05) is merged: the five components with **frozen, precisely-specifiable
interfaces** that could be built in parallel with no `/dev/kvm` and no cross-task
dependency. This document sequences everything after that and classifies each piece by
**who can build it**:

- **delegable-now** — self-contained, gate-first, interface already pinned by a spec or
  INTEGRATION.md; a worker (or a box spike) can do it today.
- **design-gated** — would be delegable, but its interface depends on a vmm-core decision
  not yet made. Named below with the exact ruling that unblocks it. Do **not** spec these
  until the ruling exists, or you encode the wrong interface.
- **frontier** — KVM bring-up and integration; stays with the integrator, designed with
  the user, per PLAN.md.

The honest shape: wave 2 is **thinner** than wave 1, because the remaining components'
interfaces (vm_state contents, the fault model, the explorer's control API) are downstream
of vmm-core's design. The leverage right now is in the **pre-vmm-core spikes and the
contract** (06/07/08) — they produce the numbers and the frozen surfaces that the frontier
work and the next delegable batch both need.

## The three rulings that unblock wave 2

Each is a vmm-core design decision (made with the user, likely during Phase 0 bring-up).
Naming them here so the dependency is explicit:

- **R1 — device-emulation model.** ✅ **Resolved — see `docs/R1-DEVICE-MODEL.md`.** No
  in-kernel irqchip (`KVM_IRQCHIP_NONE`); userspace **xAPIC** LAPIC with a V-time-driven
  timer; no IOAPIC; PIC/PIT as deterministic boot stubs. Settled ahead of bring-up by KVM
  source verification (the in-kernel LAPIC timer is host-time with no escape). Concretizes
  INTEGRATION.md §4's `vm_state` checklist into an exact field set — **unblocks 09**, and spins
  out a new delegable pure-logic `lapic` crate (task 13).
- **R2 + R3 — the dissonance design.** ✅ **Consolidated into `docs/DISSONANCE.md`** (the bug
  finder built on consonance; subsumes the former separate R2 control-API and R3 fault-model
  rulings, which turned out to be one model). The guest runs against an opaque, versioned
  **`Environment`**; a fault is just an environment answering a service **non-nominally** at one
  seam, `decide(point) -> Answer` (mechanism → the services, policy → the explorer). Control
  transport = out-of-band verbs `snapshot`/`branch`/`replay`/`run`/`perturb`/`hash` (no bare
  `restore`); faults split into a **host control plane** (machine-level) and layerable **guest
  control planes**. The two loops are the **Variation** (one run) and the **Theme** (search across
  runs). **Network
  locus = host-side `pv-net`** (no in-guest `tc`): a host L2 switch with V-time-scheduled
  delivery, every fault an op on that schedule. Spins out the four `dissonance/*` crates
  (24/25/26/12; task 24 absorbs the old "fault scheduler" row 11). **Open:** "real TCP replays
  under V-time" is gated on a guest OS — `pv-net` is gate-tested with synthetic frames until then.

## Sequenced backlog

| # | Task | Class | Depends on | Output |
|---|---|---|---|---|
| 06 | CPU/MSR determinism contract | **merged (PR #10)** | task 04 merged | `docs/CPU-MSR-CONTRACT.md` |
| 07 | PMU precise-count spike | **merged (PR #20)** | box; spec on PR #7 | measured `skid_margin=128` + GO |
| 08 | Snapshot/memslot-restore spike | **in review (PR #22)** | box | restore-latency table + chosen Phase 4 mechanism |
| 09 | `vm_state` serialization framework | **specced** (`tasks/09-vm-state.md`); delegable-now | R1 ruling, **task 06 (merged)**, snapshot-store; device section folds in 13 | versioned, round-trip-tested blob codec |
| 10 | Guest VMCALL transport shim | **merged (PR #23)** | task 01 + task 04 (both merged); §1 ABI final | `Transport` impl composing with task 01 `Client` |
| 11 | Re-baseline CPU/MSR contract → det-cfl-v1 | **merged (PR #42)** | box | det-cfl-v1 (the old "fault scheduler" row folded into task 24) |
| 12 | `dissonance/explorer` (Variation/Theme, coverage/corpus) | **specced** (`tasks/12-explorer.md`); delegable-now | dissonance design (`docs/DISSONANCE.md`); contracts of 24/25 | pure-logic exploration engine (policy) |
| 13 | `lapic` crate (userspace xAPIC + V-time timer) | **merged (PR #38)** | R1 ruling, vtime | pure-logic xAPIC register state machine + timer |
| 24 | `dissonance/environment` (decide seam + seeded faults) | **specced** (`tasks/24-environment.md`); delegable-now | dissonance design (`docs/DISSONANCE.md`) | `Environment`/`Answer`/catalog + `SeededEnv` + recorded-replay |
| 25 | `dissonance/control-proto` (control-transport wire types + codec) | **specced** (`tasks/25-control-proto.md`); delegable-now | dissonance design | versioned length-delimited codec; Tier-1 fuzz target |
| 26 | `dissonance/pv-net` (host L2 switch + V-time fault schedule) | **specced** (`tasks/26-pv-net.md`); delegable-now | dissonance design | pure-logic switch + delivery scheduler + fault→schedule |
| 29 | Telemetry console (out-of-band observation tap + std-only web viewer) | **specced** (`tasks/29-telemetry-console.md`); delegable-now | none — observation-only; `Observer` tap + `Event` schema defined locally | leaf `consonance/telemetry` crate + `console` SSE bin; vmm-core `step()` `emit` seam is frontier (INTEGRATION.md §8) |
| — | vmm-core skeleton (Phase 0) | frontier | box | boots a payload, serial console |
| — | Real perf_event backend (`CpuBackend`) | frontier | 07's numbers | consumes `skid_margin` |
| — | Multiboot + bzImage loaders | frontier | task 04 | replicate QEMU `-kernel` entry state |
| — | Dirty-log harvest + memslot restore | frontier | 08's mechanism | KVM ↔ snapshot-store wiring |

## Notes on the borderline items

- **10 (transport shim)** is the closest-to-ready delegable task after 08: the §1 VMCALL
  ABI is frozen (magic `0x31504348` ruled in PR #2), both endpoints it bridges are merged
  (task 01 `Client`/`Transport`, task 04 bare-metal payload environment), and INTEGRATION.md
  §1 already calls it "a small later task ... composes with the task 01 `Client` unchanged."
  The one open question is whether it can be gate-tested without vmm-core (the host side of
  VMCALL is frontier) — likely yes against a host-side loopback harness, which would make it
  fully delegable. Worth speccing right after 08 if you want the queue deep.
- **09 (vm_state)** looks delegable (pure serde + round-trip proptest) but is genuinely
  R1-gated: the blob's field set *is* the device model. Speccing it before R1 would freeze
  the wrong structs. **Resolved — R1 is ruled (`docs/R1-DEVICE-MODEL.md`); the exact field set
  is enumerated there, and a new `lapic` crate (task 13) falls out of it.**
- **12** is a real, valuable, pure-logic crate in the same mold as vtime/snapshot-store; its
  interface was the most downstream, **now unblocked** — R2/R3 are ruled (`docs/DISSONANCE.md`), so
  12/24/25/26 are specced and delegable.
- **29 (telemetry console)** is cleanly delegable-now and depends on no unmade ruling: it is
  pure host-side observation, and its `Observer` trait + `Event` schema are defined locally in
  the leaf `telemetry` crate. The only frontier piece is the one-line `observer.emit(...)`
  wiring inside `Vmm::step()` — built by the integrator like other delegated crates' vmm-core
  seams, not by the worker. Out-of-band by construction: never hashed (distinct from task 28's
  report channel and the M2 serial capture), default-off (`NullObserver`), `contract_hash`
  unchanged. Port/resource access is deliberately **not** in scope — that rides R2/R3.

## Recommended order of operations

1. Land 06 (workflow in flight) and 07 (after PR #7 merges) → vmm-core has its contract and
   its `skid_margin`.
2. Run 08 (this PR adds the spec) → the Phase 4 restore mechanism is chosen.
3. Spec 10 if you want a delegable task running while vmm-core bring-up starts.
4. R1/R2/R3 are ruled (`docs/R1-DEVICE-MODEL.md`, `docs/DISSONANCE.md`) → 09 and the dissonance
   wave (12/24/25/26) are delegable now.
5. Frontier work (vmm-core skeleton → backends → loaders → snapshot wiring) proceeds on the
   box, consuming 06–08's outputs.

## Wave 3 — workloads + branching (current frontier)

Deterministic real-Linux boot **landed** (task 34: two same-seed patched boots → bit-identical serial
through `GUEST_READY` + identical `state_hash`). That closes PLAN.md Phases 0–3. Wave 3 is the
reorientation after it, decided with the user: **running a sophisticated workload is the highest-leverage
next step** ("right now we barely run anything"), and **branching is the dissonance half**. Two streams
run **in parallel right now** — they share nothing and both execute on the box:

| Stream | Tasks (start → end) | Startable now | Output |
|---|---|---|---|
| **consonance — workloads** | **36** kernel rebase → **37** bare Postgres → **38** Postgres-in-Docker | **36** | a real, stateful, containerized DB runs deterministically, streaming stdout/stderr |
| **dissonance — branching** | **39** live snapshot/branch → **40** branching demo | **39** | snapshot a running guest, fork into reproducible seeded futures |

The two are independent until they **join at 40** (branch a running Postgres, needs 39 + 37). Task **39**
can start against the guest we *already* boot — it does not wait on Postgres. So the immediate parallel
pair is **36 (consonance) ‖ 39 (dissonance)**.

Decisions baked into the specs (from the design discussion):
- **Workload = Postgres**, single-node throughout (bare first to isolate the DB determinism surface, then
  Docker for the credibility shot). Container runs `--network none`.
- **Guest kernel:** stop hand-growing `tinyconfig`; rebase onto a **Kata-class container-host config** +
  the determinism overlay (`config-fragment`), built + pinned with our own pipeline. Determinism is
  enforced from *below* (patched KVM + V-time + device models), so the config is capability/probe-surface
  only, not load-bearing — see task 36.
- **Storage = RAM-backed** (brd or loop-over-ext4-image): real ext4 + real `fsync`, contents in the
  already-hashed/snapshotted guest RAM. **Tasks 22 (`BLOCK_WRITE` to real storage) and 23
  (crash-consistency) are struck** as originally scoped.

Deferred, captured so they aren't re-derived (none on the Wave-3 critical path):
- **D1 — host-side, snapshot-store-backed RAM block device.** The reborn 22/23: a *modeled* disk outside
  guest volatile RAM, so "power loss drops the cache, keeps the disk" is expressible and durability faults
  (tear / reorder / drop un-synced writes per seed) inject at the host's block view. The only way to hunt
  **durability/crash-consistency** bugs; until it exists, task 40 hunts the **concurrency/scheduling** bug
  class only.
- **D2 — distributed / multi-node + live `pv-net`** (the 3-node-Raft money-shot). **D3 — modeled
  block-level faults** (rides D1). **D4 — boot/exec performance** (`run_until`-precise stepping; 14 s/boot
  is bounded and fine until exploration scales).

CPU characterization (an AMD-SVM and/or ARM branch-count **feasibility spike**, task-92 registry) is a
"maybe" the user flagged — held off the critical path because it needs different silicon, not because it
lacks value (AMD is the lower-risk portability win; ARM the higher-risk/higher-cool one). See the ARM
section below.

## Cross-cutting: ARM/AArch64 port (post-v1)

`docs/ARM-PORT.md` captures the feasibility analysis for an eventual AArch64 backend
(Cortex-X925/A725 on DGX Spark, or Neoverse V2 on Grace). Key takeaways, recorded so they
aren't re-derived: **DGX Spark is *not* Neoverse V2** (it's Armv9.2-A Cortex-X925/A725, and
unlike V2 it *has* FEAT_ECV — the time-virtualization feature PLAN.md named as the blocker);
Spark gives bare-metal KVM + documented perf access (the vendor-lock worry doesn't
materialize); but the central bet — precise branch-counting for V-time — is **unproven on
any candidate ARM core** (rr has zero tested data on V2 and doesn't even recognize Spark's
cores). The viability gate is **Phase 0.5 spike #1 re-run on real ARM silicon**, not a code
refactor — so the arch abstraction is *not* built pre-emptively; it's adopted as a discipline
while `vmm-core` is written (neutral core vs. `vmm-vmx` backend) so an ARM backend is additive.

## Cross-cutting: determinism & conformance corpus

`docs/DETERMINISM-CORPUS.md` is the plan for *verifying the engine is itself correct* — a
growing corpus (instruction sweep, fuzzed inputs, real workloads like SQLite-with-disk) run
through four oracles (determinism via `unison`, conformance vs the frozen contract,
seed-sensitivity, backend-equivalence). Tasks 17 (`det-corpus` harness) and 18 (instruction
sweep) are specced and delegable-now; 19 (fuzzer) and 20 (SQLite-with-disk) are outlined there.
The device surface is deliberately minimal and thin today — the complete set of hypercall
services is `Console` (out), `Entropy`, `Block` (read-only), `Event`; there is **no network
service and no writable storage** yet (see the "Current device surface" table in
DETERMINISM-CORPUS.md). Wave 3 supplies writable storage **inside the guest** instead of via a
host service: a RAM-backed ext4 (brd / loop), so the contents live in already-hashed guest RAM and
need no new hypercall service — **tasks 22/23 (host `BLOCK_WRITE` to real storage + crash-consistency)
are struck**, and the corpus's "real standard system" entry retargets from SQLite-with-disk to
**Postgres-on-RAM** (tasks 36–38). The durability-fault surface — the one thing RAM-in-guest can't
model, since an instant/no-op `fsync` has no durable-vs-volatile split — is **deferred to D1**, a
host-side snapshot-store-backed RAM-disk model, not a real disk. Network is *not* a host device in
this model — intra-guest networking comes free with a Linux guest; the external-net escape, the
fault-injecting bridge, and the distributed fault model are deferred to **R3 / D2** (+ multi-node),
not a new device-model ruling.

## Cross-cutting: code quality

Independent of the feature backlog above, `docs/CODE-QUALITY.md` is the prioritized plan for
tightening quality tooling (coverage via `cargo-llvm-cov`, mutation testing via
`cargo-mutants`, mechanized determinism lints, fuzzing the wire decoders, Kani proofs on
`vtime` arithmetic, and more). Tier 0 there is adoptable now on the merged pure-logic crates;
later tiers attach to `vmm-core`/`vm_state` as those surfaces appear.
