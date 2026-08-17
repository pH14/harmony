# TESTING — the testing authority doc

This document is the index and the authority for how harmony is tested. It replaces the
scattered per-crate `IMPLEMENTATION.md` prose that used to serve as the list of hardware gates
and their hand-typed invocations. Companion docs: `AGENTS.md` (what "correct" means),
`docs/GLOSSARY.md` (the naming authority), `docs/LAYERS.md` (the capability layering),
`docs/ARCH-BOUNDARY.md` (the ISA boundary), `docs/PROTOCOL.md` (the control-wire authority),
`docs/CODE-QUALITY.md` (the tool floor).

## Why a ladder

harmony is a hypervisor whose correctness bar is *bit-identical reproducibility*: the same seed
must produce the same execution, byte for byte. That bar is not one property, and no single test
can hold it. It decomposes into five separable claims, and each claim wants a different kind of
test, running in a different place:

1. the **chip** behaves the way the determinism machinery assumes it does;
2. every **backend** (the thing that holds a vCPU and surfaces its exits) obeys the same written
   contract;
3. the **engine** built on a backend actually reproduces a run;
4. the **control wire** a remote client drives the engine through keeps its per-plane promises;
5. real **workloads** pass real oracles on the hosts we claim to support.

Those are the five rungs. Each rung is only meaningful if the rung below it holds, and each rung
catches a class of defect the rung above it would only see as an unexplained divergence. A
divergence caught at rung 1 is a sentence ("this chip's `RDTSC` is not trapped"); the same defect
caught at rung 5 is a week of bisection.

Two rules apply to the whole ladder.

- **A green gate is the floor, not the bar.** A test that cannot fail is worse than no test,
  because it reports confidence. Every rung below names how its gates avoid passing vacuously.
- **Hardware gates fail loudly when the host baseline is missing — never skip silently.** A gate
  that needs `/dev/kvm`, a patched module, a pinned core, or a specific CPU must *panic with what
  is missing and where to run it*, never return success because it found nothing to do. The
  `#[ignore]` attribute is how a hardware test stays out of the portable lane; a runtime
  capability probe that turns a missing host into a pass is a defect.

---

## Rung 1 — CPU qualification

**What it proves.** That a specific physical chip is a lawful substrate for the determinism
machinery: every instruction and feature the chip advertises is accounted for, and the accounting
is *exhaustive* rather than a list of the ones somebody remembered.

**What kind of test lives here.** A per-chip sweep that enumerates the chip's advertised
instruction and feature surface and classifies each entry into exactly one of three buckets:

| Bucket | Meaning | Consequence if wrong |
|---|---|---|
| **deterministically pure** | the operation's result is a function of architectural state alone | none — it may execute in the guest untrapped |
| **must-trap** | the result depends on something outside architectural state (`RDTSC` and the timestamp counter, `RDRAND`/`RDSEED` and the hardware entropy source, the CPUID leaves that vary by host) | a same-seed divergence, silently, at an unpredictable point |
| **forbidden** | the operation cannot be made deterministic on this chip at all, and must be unreachable (CPUID-hidden, control-register-owned, or filtered) | an adversarial or merely unlucky guest escapes determinism |

Alongside the classification, two checks that are properties of the chip rather than of any
instruction:

- **exactness** — that a deadline expressed in retired branches lands where the machinery says it
  lands, on this chip's performance-monitoring hardware, including the skid the chip actually
  exhibits rather than the skid the datasheet claims;
- **save/restore fixpoint** — that this chip's full vCPU state (including the extended state
  image, whose size and layout are host-dependent) survives a `save → restore → save` round trip
  unchanged.

**Where it runs.** On the chip. Nowhere else — this is the one rung that is *definitionally*
hardware, because the artifact under test is the silicon.

**Specified here, built in a later PR.** Its first two customers are already queued:

- the **x86 determinism box re-certification** — the `det-cfl-v1` baseline (Coffee Lake-S,
  `docs/CPU-MSR-CONTRACT.md` §2) is currently pinned by a hand-audited contract table plus the
  live gates in rung 3; qualification turns that into a mechanical per-chip verdict that a
  re-certification can re-run rather than re-argue;
- the **incoming ARM box** — the first question qualification must answer there is whether
  **FEAT_ECV** is present, because the whole paravirtualized-clock design
  (`docs/PARAVIRT-CLOCK.md`) exists to survive its absence: on silicon without it the guest's
  counter read cannot be trapped, so the clock must be work-derived rather than counter-derived.
  "Is ECV present" is a qualification output, not an assumption.

**Non-vacuity.** A qualification sweep that classifies zero instructions, or that treats "the
chip does not advertise this leaf" as a pass without recording that the leaf was absent, has
proved nothing. The sweep reports counts per bucket; an empty bucket is a result that must be
explained, not a silence.

---

## Rung 2 — the backend contract suite

**What it proves.** That every implementor of the `Backend` trait
(`consonance/vmm-backend/src/backend.rs`) obeys the same written contract — so the deterministic
VMM above it can be written against the trait alone and never branch on which substrate is in
use.

**What kind of test lives here.** **Contract tests**: one shared exam, written once, generic over
the trait, run against every implementor. The trait's doc comments already *state* the contract;
this rung makes them executable. The exam lives in `vmm_backend::contract` behind the non-default
`contract-tests` feature, and is driven through a `BackendFixture` — the implementor supplies a
fresh backend armed for a named `Scenario`, and the exam supplies the questions.

The obligations fall into three categories. **These three are the whole taxonomy**; there is no
fourth. In particular, "capability honesty" is *not* a category — a capability flag does not
create an obligation of its own, it **selects which exactness exams apply**.

| Category | The obligation | Representative exams |
|---|---|---|
| **ordering** | operations happen in the contract's order, and an out-of-order one fails closed rather than silently mis-servicing the guest | `run` before `set_policy` → `NotConfigured`; resuming with an unserviced read-style exit → `PendingCompletion`; every (pending exit kind × wrong completion method) cell → `BadCompletion` (or, where the trait pins it so, `NoPendingRead`) |
| **exactness** | quantities the engine treats as exact really are exact | `run_until` lands exactly on the deadline and never stops early (the late-only-stop contract); a guest exit before the deadline returns early; zero-length and already-past deadlines; monotonicity — `run_until(a)` then `run_until(b)` is equivalent to `run_until(b)`; same start + same deadline twice → identical exit and identical state; the dirty-page log is sorted, deduplicated, reset on drain, and may over-report but never under-report |
| **fixpoint** | round trips are round trips | `save → restore → save` yields an identical `VcpuState`; a malformed blob yields the `InvalidState` error, never a panic |

Two contracts get their own named exams because they are the ones most easily got subtly wrong:

- **the interrupt delivery contract** — `set_pending_irq` is **one overwritable slot, never a
  queue** (the VMM owns the userspace interrupt fabric and re-arbitrates at every entry, so a
  second set must overwrite rather than enqueue), and `take_accepted_interrupt` reports only
  interrupts *actually issued into the guest*, not merely staged ones;
- **capability-keyed exactness** — a backend that advertises a deterministic clock is required to
  surface the guest's clock reads as exits resolvable to V-time; a backend that does not
  advertise it is required to *say so honestly* rather than quietly behave as if it had the
  capability. The flag chooses the exam; it is not itself the thing under test.

**Where it runs.**

- **portable** — the full exam over `MockBackend`, in the ordinary `cargo nextest` lane, on macOS
  and Linux (`consonance/vmm-backend/tests/contract_mock.rs`);
- **hardware** — the *identical* exam over `KvmBackend` and `PatchedKvmBackend`, Linux/x86_64,
  `#[ignore]`d (`consonance/vmm-backend/tests/contract_kvm.rs`). CI compiles it; the hardware lane
  (rung 5's runner, below) executes it.

The trait is **designed, not frozen** (`backend.rs` says so, and the ARM freeze decision is still
open — ARM's overflow-to-exit path may yet pressure `run_until`). This suite is the tripwire for
that discussion: a freeze proposal is credible exactly to the degree that this exam passes
unchanged on a second vendor.

**Non-vacuity.** The exam reports what it *declined* to run. A fixture that cannot produce a
given scenario (stock KVM, for instance, never surfaces a hypercall exit — the kernel services it
in-kernel) returns `None` from `spawn`, and the scenario lands in `ContractReport::declined`,
which the caller asserts on. Declines are data. A silently smaller exam is not.

---

## Rung 3 — engine identity tests

**What it proves.** That the deterministic VMM reproduces a run: two executions of the same
address produce bit-identical full state hashes.

**Vocabulary.** The test that runs the same address twice and compares full state hashes is an
**identity test**. (Not "deterministic-twice" — that phrase named the mechanism by its shape and
never said what it established.)

**What kind of test lives here.** The existing determinism suites: the `unison` divergence
bisector and its oracles, the `acceptance-suite` O1 oracle, and — the bulk of the coverage — the
`#[ignore]`d live gates that boot a real guest on real KVM.

**Where it runs.** Portable halves in the ordinary lane; live halves on the determinism box.

### Inventory — every hardware-gated and out-of-lane test file, its rung, and its invocation

This table is the index. It is normative for *where a gate lives*; each file's own header remains
normative for that gate's environment (image hashes, environment-variable knobs, revert steps).
Every invocation below is transcribed from those headers.

Common shape: `cargo test -p <crate> --test <file> -- --ignored --nocapture --test-threads=1`,
prefixed by `taskset -c <core>` for core pinning and often `timeout <s>`. Where a header names a
specific core, it is reproduced; `<core>` means the header leaves it to the run window's lease.

| Test file | Rung | Needs | Invocation |
|---|---|---|---|
| `consonance/vmm-backend/tests/contract_kvm.rs` | 2 contract | `/dev/kvm`, patched module for the patched leg | `taskset -c 1 cargo test -p vmm-backend --test contract_kvm -- --ignored --nocapture --test-threads=1` |
| `consonance/vmm-backend/tests/kvm_smoke.rs` | 2 contract | `/dev/kvm`, VMX | `taskset -c 1 cargo test -p vmm-backend --test kvm_smoke -- --ignored --test-threads=1` |
| `consonance/vmm-backend/tests/live_preemption.rs` | 2 contract (exactness) | patched KVM, perf | `taskset -c 2 timeout 120 cargo test -p vmm-backend --test live_preemption -- --ignored --nocapture --test-threads=1` |
| `consonance/vmm-core/tests/live_linux_boot.rs` | 3 identity | `/dev/kvm`, bzImage + initramfs | `taskset -c 1 timeout 180 cargo test -p vmm-core --test live_linux_boot -- --ignored --nocapture --test-threads=1` |
| `consonance/vmm-core/tests/live_m1_m2.rs` | 3 identity | `/dev/kvm`, perf | `taskset -c 1 cargo test -p vmm-core --test live_m1_m2 -- --ignored --test-threads=1` |
| `consonance/vmm-core/tests/live_determinism.rs` | 3 identity | patched KVM | `taskset -c 2 cargo test -p vmm-core --test live_determinism -- --ignored --test-threads=1` |
| `consonance/vmm-core/tests/live_preemption.rs` | 3 identity | patched KVM, perf | `taskset -c 2 timeout 150 cargo test -p vmm-core --test live_preemption -- --ignored --nocapture --test-threads=1` |
| `consonance/vmm-core/tests/live_snapshot_branch.rs` | 3 identity | patched KVM | `taskset -c 4 cargo test -p vmm-core --test live_snapshot_branch -- --ignored --test-threads=1` |
| `consonance/vmm-core/tests/live_nonquiescent_snapshot.rs` | 3 identity | patched KVM, Postgres image | `taskset -c 4 timeout 3600 cargo test -p vmm-core --test live_nonquiescent_snapshot -- --ignored --nocapture --test-threads=1` |
| `consonance/vmm-core/tests/live_branching_demo.rs` | 3 identity | patched KVM, Postgres image | `taskset -c 4 timeout 3600 cargo test -p vmm-core --test live_branching_demo -- --ignored --nocapture --test-threads=1` |
| `consonance/vmm-core/tests/live_dirty_remap.rs` | 3 identity | patched KVM, dirty log, Postgres image | `taskset -c 2 timeout 7200 cargo test -p vmm-core --release --test live_dirty_remap -- --ignored --nocapture --test-threads=1` |
| `consonance/vmm-core/tests/live_pvclock.rs` | 3 identity | patched KVM, pvclock kernel build | `taskset -c 2 cargo test -p vmm-core --release --test live_pvclock -- --ignored --test-threads=1` (**g0 smoke first** — see ordering below) |
| `consonance/vmm-core/tests/live_host_plane.rs` | 3 identity | patched KVM | `taskset -c <core> cargo test -p vmm-core --release --test live_host_plane -- --ignored --nocapture` |
| `consonance/vmm-core/tests/live_moment_address.rs` | 3 identity | patched KVM | `taskset -c <core> cargo test -p vmm-core --release --test live_moment_address -- --ignored --nocapture` |
| `consonance/vmm-core/tests/live_sdk.rs` | 3 identity | patched KVM, SDK guest image | `cargo test -p vmm-core --release --test live_sdk -- --ignored --nocapture` |
| `consonance/vmm-core/tests/live_exec_improvisation.rs` | 4 protocol (live) | patched KVM, exec initramfs | `INITRAMFS=initramfs-exec.cpio.gz taskset -c <core> cargo test -p vmm-core --release --test live_exec_improvisation -- --ignored --nocapture` |
| `consonance/vmm-core/tests/seal_rate_sweep.rs` | 3 identity (rate) | patched KVM, Postgres image | `taskset -c 2 timeout 7200 cargo test -p vmm-core --test seal_rate_sweep -- --ignored --nocapture --test-threads=1` |
| `consonance/vmm-core/tests/live_postgres.rs` | 5 acceptance | patched KVM, Postgres image | `taskset -c 2 timeout 1500 cargo test -p vmm-core --test live_postgres -- --ignored --nocapture --test-threads=1 p2_postgres_deterministic_twice_patched` |
| `consonance/vmm-core/tests/live_postgres_docker.rs` | 5 acceptance | patched KVM, Docker image | `taskset -c 2 timeout 3000 cargo test -p vmm-core --test live_postgres_docker -- --ignored --nocapture --test-threads=1 p2_docker_postgres_deterministic_twice_patched` |
| `consonance/vmm-core/tests/live_runc_postgres.rs` | 5 acceptance | patched KVM, runc image | `taskset -c 4 timeout 4200 cargo test -p vmm-core --test live_runc_postgres -- --ignored --nocapture --test-threads=1 r2_runc_postgres_deterministic_twice_patched` |
| `consonance/vmm-core/tests/live_k3s_postgres.rs` | 5 acceptance | patched KVM, k3s image | `taskset -c 2 timeout 14400 cargo test -p vmm-core --test live_k3s_postgres -- --ignored --nocapture --test-threads=1 k2_k3s_postgres_deterministic_twice_patched` |
| `consonance/vmm-core/tests/box_corpus.rs` | 5 acceptance | patched KVM, corpus payloads | `taskset -c 2 cargo test -p vmm-core --test box_corpus -- --ignored --nocapture` (bless goldens: prefix `DETCORPUS_BLESS=1`) |
| `dissonance/campaign-runner/tests/live_harmony_bridge.rs` | 5 acceptance | patched KVM, guest image | `taskset -c <core> cargo test --release -p campaign-runner --test live_harmony_bridge -- --ignored --nocapture --test-threads=1` |
| `dissonance/campaign-runner/tests/live_materialization.rs` | 5 acceptance | patched KVM, Postgres image | `taskset -c 2 timeout 7200 cargo test -p campaign-runner --test live_materialization -- --ignored --nocapture --test-threads=1` |
| `dissonance/campaign-runner/tests/live_film.rs` | 5 acceptance | patched KVM, game workload + pinned core | `taskset -c <core> timeout 7200 cargo test -p campaign-runner --test live_film -- --ignored --nocapture --test-threads=1` |
| `dissonance/campaign-runner/tests/live_draw_probe_pair.rs` | 5 acceptance | patched KVM, game workload | `taskset -c <core> cargo test --release -p campaign-runner --test live_draw_probe_pair -- --ignored --nocapture --test-threads=1` |
| `dissonance/campaign-runner/tests/live_draw_probe_diagnosis.rs` | 5 acceptance (diagnostic) | patched KVM, game workload | `taskset -c <core> cargo test --release -p campaign-runner --test live_draw_probe_diagnosis -- --ignored --nocapture --test-threads=1` |

Out-of-lane but **not** hardware — listed so the inventory is complete and nobody hunts for a box
to run them on:

| Test file | What it is | Invocation |
|---|---|---|
| `consonance/vmm-backend/tests/n2_nested_hammer.rs` | exploratory apparatus from the nested-x86 program, not production surface | run through that program's own harness |
| `consonance/vmm-core/tests/n3_repeat_gate.rs` | exploratory apparatus from the nested-x86 program | run through that program's own harness |
| `consonance/vmm-core/tests/arm64_tcg_smoke.rs` | needs `clang` + `llvm-objcopy` + `qemu-system-aarch64`, no KVM | `cargo test -p vmm-core --test arm64_tcg_smoke -- --ignored` |
| `consonance/snapshot-store/tests/bench.rs` | informational timing, not pass/fail | `cargo test -p snapshot-store --release --test bench -- --ignored --nocapture` |
| `consonance/snapshot-store/tests/bench_production_shape.rs` | informational timing at production shape | `cargo test -p snapshot-store --release --test bench_production_shape -- --ignored --nocapture` |
| `consonance/vm-state/tests/golden.rs` | the golden regenerator behind `--ignored`; the golden assertion itself runs in the ordinary lane | `cargo test -p vm-state --test golden -- --ignored --nocapture` |
| every crate's `tests/public_api.rs` | frozen-surface snapshots; need the pinned nightly + `cargo-public-api` | the `public-api` job in `.github/workflows/quality.yml` |

**Gate ordering on hardware.** Two orderings are load-bearing. They were encoded in
`scripts/box-gates.sh`, the packaged x86 suite retired with the det-cfl-v1 box in 2026-08
(git history has the script); any future hardware-suite runner must re-encode both rather
than leave them to memory:

- **`live_pvclock` runs its `g0` smoke first.** `g0_smoke_boot_registers_and_reads_sane_time` is
  the minutes-long probe of the riskiest live assumptions (does the kernel build, does the guest
  register the clock page, does it read sane time, does it reach readiness). Spending the G1/perf
  budget before g0 passes wastes hours on a wedged image.
- **The patched-module gates leave the host on stock KVM.** Every gate that loads the patched
  modules must revert them afterwards and *verify* the revert on a fresh connection. The revert
  is a checked step in the script, not a comment in a header.

---

## Rung 4 — protocol tests

**What it proves.** That the control wire — the verbs a remote client drives the engine through
(`dissonance/control-proto`, served by `vmm_core::control::ControlServer`) — keeps the obligation
attached to each of its five planes.

**What kind of test lives here.** Per-plane obligation tests. The planes and their obligations are
ruled in `docs/PROTOCOL.md`; this rung is where each obligation becomes a test:

| Plane | Obligation | Test |
|---|---|---|
| session | the handshake is first and version-comparable | negotiation tests in `dissonance/control-proto/tests/negotiation.rs` |
| state algebra | replay identity — restoring and re-running reproduces the state | `consonance/vmm-core/tests/protocol.rs` (`Drop` lifecycle) plus the rung-3 live identity gates |
| observation | hash neutrality — observing must not change the machine | `consonance/vmm-core/tests/protocol.rs`, interleaving every observation verb into a run and requiring the final `state_hash` to be unchanged |
| intervention | `Perturb` is recorded (a fault is an input, so it is environment amendment); `Exec` is off the record and taints its timeline | the taint tests in `control.rs` and the live improvisation gate |
| provenance | golden-stable encoding — reproducers are persisted evidence | `dissonance/control-proto/tests/golden.rs`, one golden per `Request`/`Reply` variant |

**Where it runs.** Entirely portable: `MockBackend` under `ControlServer::handle`, in the ordinary
lane. The wire is the one part of the system that needs no hardware to be pinned exactly.

**Non-vacuity.** The golden test asserts *exact bytes*, not round-trip equality — a codec that
round-trips its own drift still fails. The hash-neutrality test asserts against a control run
with no observations at all, so a change that made observation a no-op would fail the run
comparison rather than pass the neutrality check.

---

## Rung 5 — the acceptance matrix

**What it proves.** That real workloads pass real oracles on the hosts and virtualization levels
we claim to support.

**What kind of test lives here.** Not test *functions* — **data**. A cell of the matrix is a row
in the corpus manifest (`docs/corpus-manifest.toml`), and the `acceptance-suite` binary executes
cells. The matrix axes:

| Axis | Values | Manifest field |
|---|---|---|
| **workload** | the payload or application under test | `name` + `source` + `kind` |
| **oracle** | **O1 identity** (same address twice, bit-identical), **O2 conformance-to-spec** (observable digest equals a committed golden), **O3 seed-sensitivity** (different seeds must diverge the *observable* output while the work count holds) | `oracles` |
| **host** | `portable` (any dev machine, toy subject), `det-cfl-v1` (the x86 determinism box), `msr1` (the ARM box) | `hosts` |
| **virt level** | `l1` (guest on bare-metal host) or `l2` (guest inside a virtualized host) | `virt` |

**A hardware cell differs from a portable cell only in the host row.** That is the point of
making cells data: the same workload, the same oracles, the same runner — a different host
requirement. Nothing about the oracle changes when the substrate does.

**Where it runs.** The `acceptance-suite` binary. It runs every portable cell anywhere (there is a
CI smoke test that does exactly this, so the entry point cannot rot); on a qualifying host it
additionally runs the cells that host satisfies, selected with `--host <id>`. The real-VMM
registry is wired behind a Linux composition root in the binary, mirroring the pattern in
`dissonance/campaign-runner/src/boxrun.rs`: the library stays substrate-free, the binary names the
concrete pair.

**Strictly additive, for now.** The existing `live_*` workload tests are **not** migrated into
manifest rows by this rung, and not one of them is deleted, rewritten, or weakened. Migration
happens only after a hardware parity run proves the new entry point reproduces the old gates
exactly. Until then the manifest is the *shape* of the acceptance matrix and the `live_*` files
are its *content*.

**Non-vacuity.** The manifest validator already rejects an empty corpus and any item that declares
zero oracles (both would report all-pass while testing nothing), and the runner refuses an item
filter that matches nothing. A host requirement that no runner satisfies must surface as an
unrun cell in the report, never as a pass.

---

## The runner-label scheme for hardware lanes

Hardware CI is a `workflow_dispatch`-only workflow, `.github/workflows/box.yml`, with one lane
per hardware class, keyed on **self-hosted runner labels**. Every lane in the workflow must have
a registered runner: a job whose label set matches nothing does not skip — GitHub leaves it
queued, and the workflow's one-run-at-a-time concurrency group wedges behind it. A lane is added
alongside its box and removed with it.

| Lane | Labels | What it runs |
|---|---|---|
| ARM box | `[self-hosted, kvm, arm64, msr1]` | the CPU-qualification lane (rung 1). A placeholder step plus the portable arm64 checks until the qualification suite exists |

(The x86 determinism-box lane — `[self-hosted, kvm, x86_64, det-cfl-v1]`, running the packaged
`scripts/box-gates.sh` suite — was retired with its machine in 2026-08; both live in git history
for the day a successor box exists.)

Label discipline: the first three labels are *capabilities* (self-hosted, has KVM, this
architecture) and the fourth is the **chip baseline identity** (`msr1`; the retired x86 box was
`det-cfl-v1`). A gate that depends on a specific chip names that chip in its label set; a gate
that merely needs KVM does not. This is what keeps a determinism gate from scheduling onto the
wrong silicon and reporting a green that means nothing.

---

## Where each rung's evidence lives

| Rung | Portable evidence | Hardware evidence |
|---|---|---|
| 1 CPU qualification | — (nothing about a chip is portable) | the qualification report, per chip |
| 2 backend contract | `contract_mock.rs` in the ordinary lane | `contract_kvm.rs` + `kvm_smoke.rs`, box-only `#[ignore]` gates (their packaged runner retired with the det-cfl-v1 box) |
| 3 engine identity | `unison` + `acceptance-suite` O1 over toy subjects | the `live_*` identity gates |
| 4 protocol | `control-proto` goldens + `vmm-core/tests/protocol.rs` | the live improvisation/taint gate |
| 5 acceptance matrix | portable cells via the `acceptance-suite` binary | hardware cells, plus the `live_*` workload gates until migration |
