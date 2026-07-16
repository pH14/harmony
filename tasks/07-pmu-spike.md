# Task 07 — `spikes/pmu-count/`: PMU precise-count feasibility spike

Read `tasks/00-CONVENTIONS.md` first. Touch only `spikes/pmu-count/`. This directory is
**not** part of the cargo workspace (`members = ["consonance/*"]` doesn't match it); it has its
own `Cargo.toml` and its own gates. Spike code is throwaway by design — the deliverable is
**measured numbers**; the rigor lives in the measurements, not the code.

## Environment

- Runs on: **Linux bare-metal x86-64 Intel only** — the determinism box, reached as
  `ssh <det-box>` from your session. Nested virtualization does **not** satisfy this
  (docs/PLAN.md Decision 0: VMX doesn't nest well for PMU work). macOS is your terminal and
  editor only; build and run happen on the box (rsync the spike dir over, or build there
  from a git checkout — document whichever workflow you use in RESULTS.md).
- Requires: `/dev/kvm`, perf_event (`kernel.perf_event_paranoid` ≤ 1 or root — check and
  document which), Rust on the box (`scripts/provision-host.sh` has run).
- **Confound controls** (skid numbers are worthless without them): pin the vCPU thread to
  a fixed core with `sched_setaffinity`; document in RESULTS.md the core ID, whether SMT
  siblings were active, the scaling governor and turbo state, the NMI-watchdog state
  (disable it for the runs or show it doesn't perturb the counts), the host IRQ
  affinity/isolation status for the chosen core, and the C-state/idle policy. Any of
  these you do not control, say so explicitly and fold it into the verdict's confidence
  statement.
- Does not require: QEMU, Docker, the guest Linux image.
- **Fail fast, never skip**: every gate script must detect an unsupported host (no KVM, no
  Intel PMU, paranoid level too high) and fail with a message saying what's missing and
  where to run it — never silently pass or skip.

## Context

docs/PLAN.md Phase 0.5, spike 1 — the experiment the whole vtime design is betting on. V-time
is a pure function of a hardware count of retired conditional branches, and timer
injection requires stopping the vCPU at an **exact** count (arm the PMU overflow early by
a margin, then single-step to the target — the rr technique the merged `vtime` crate's
planner implements against a simulator). Four properties must hold on the real CPU, and if
any fails, design changes cascade, so we prove them before Phases 2/4 invest:

(a) a perf_event counter attached to a KVM vCPU can count **guest-only** retired
conditional branches, unperturbed by VM exits and host work; (b) counter overflow reliably
breaks execution out of `KVM_RUN`; (c) skid is bounded and **late-only**; (d) overflow-early
plus `KVM_GUESTDBG_SINGLESTEP` lands at a stable, repeatable instruction.

The outputs feed directly into: `PlannerConfig::skid_margin` (INTEGRATION.md §6, first
open question) and a go/no-go verdict for the vtime architecture.

## Deliverable

`spikes/pmu-count/` containing:

- A small Rust harness (C shims allowed where ioctls demand it) on `kvm-ioctls`/
  `kvm-bindings` + raw `perf_event_open` (via `libc`/`rustix`). **Dependency whitelist for
  this directory extends to**: `kvm-ioctls`, `kvm-bindings`, `libc`, `rustix`,
  `vm-memory` (optional). **`unsafe` is granted** for KVM/perf FFI and guest-memory
  mapping, each block with a `// SAFETY:` comment; the no-panic and no-float disciplines
  still apply to measurement logic.
- Two tiny deterministic guest workloads (flat binaries or reuse of `guest/payloads/`
  pieces — your choice): one **branch-dense** (≈1 counted event/instruction) and one
  **branch-sparse** (long stretches with few conditional branches), each with a
  **statically known** total conditional-branch count to a defined stop point. At least
  one workload must include a **ring-3 (CPL3) phase** — enter user mode (iret to a flat
  user segment) and retire a known number of conditional branches there — because the
  production guest spends most of its work in userspace and the counter config must count
  CPL3 branches identically. Report the full exclude bitmask of the chosen attr
  (`exclude_user`, `exclude_kernel`, `exclude_hv`, `exclude_host`, `exclude_guest`,
  `exclude_idle`). Each workload's expected count is derived **statically and the
  derivation is checked in**: an annotated disassembly or a generator script that
  computes the expected count from the instruction stream — never inferred from a first
  measurement.
- `RESULTS.md` with every experiment's raw numbers and exact reproduction commands.
- One entry-point script (`run-all.sh`, shellcheck-clean) that runs every experiment
  end-to-end on the box and regenerates the numbers.

## Experiments (normative — RESULTS.md must report each)

1. **Counter configuration**: find the perf_event attr (event/umask for retired
   conditional branches, exclude-bit combinations, attached to the core-pinned vCPU
   thread) whose count for **both** known-count workloads — including the CPL3 phase —
   equals the static expectation **exactly**. The event must be **`pinned = 1`**
   (failure to schedule is a hard error, never silent multiplexing), and every trial must
   verify `time_enabled == time_running` from the perf read format — a multiplexed
   counter invalidates the trial. Report every config tried and its error.
2. **Cross-run stability**: **both** workloads, ≥ 100 runs each: the count at the stop
   point is bit-identical every run. Report any variance and what config eliminated it.
3. **Exit perturbation**: the same workloads with deterministic VM exits injected
   mid-stream. Define the checkpoint mechanism once (e.g. identical checkpoint exits
   present in **both** arms, or truncated-run variants) and use the same mechanism in the
   baseline and perturbed runs so the counts are comparable; state the choice in
   RESULTS.md. The project's hypercalls are **VMCALL** (INTEGRATION.md §1), so use
   VMCALL-induced exits if you can achieve them under KVM — discovering and documenting
   the mechanism (`KVM_EXIT_HYPERCALL` availability vs #UD interception vs another route)
   is itself spike output. If VMCALL exits prove unachievable in the spike's budget,
   port-I/O exits are an acceptable **explicitly-labeled proxy** and RESULTS.md must carry
   a `[question]` flagging that the VMCALL exit path is unmeasured. Either way: counts at
   fixed checkpoints are identical to the no-exit run.
4. **Overflow kick**: arm `sample_period` so overflow fires mid-workload; verify the
   signal interrupts `KVM_RUN` (exit with `EINTR`/immediate-exit path) 100/100 times;
   report the arming→stop latency in counted events.
5. **Skid distribution**: ≥ 1000 trials, with `armed_count` **defined** as
   `count_before_arm + programmed sample_period`; log all three of
   (`count_before_arm`, `sample_period`, `stopped_count`) per trial and record the
   `stopped_count − armed_count` min/median/p99/max. Trials follow a **prescribed
   schedule**: for each workload × armed position ≈{10%, 50%, 90%} of that workload's
   counted length × {no exits, with exits (**experiment 3's** exit mechanism)} — **≥ 100
   trials per cell** (12 cells ⇒ ≥ 1200 total), with at least one armed-position cell of
   the ring-3 workload lying **inside its CPL3 phase** (the headline margin must cover
   userspace execution), and the exact target formula and any seed stated in RESULTS.md
   so the schedule is reproducible. Verify **late-only** (never negative). This histogram
   is the headline output.
6. **Single-step landing** — the full vtime arming pattern. For each trial: pick a
   **target** from the experiment-5 schedule, choose a `candidate_margin` greater than
   experiment 5's observed max skid, arm at `armed_count = target − candidate_margin`,
   run to overflow, **require `stopped_count ≤ target`** (an overshoot is recorded as a
   SkidExceeded-class failure, never silently retried), then single-step
   (`KVM_GUESTDBG_ENABLE | KVM_GUESTDBG_SINGLESTEP`) until the counter reads exactly
   `target`. Coverage is a matrix, not one point: each workload × armed position
   ≈{10%, 50%, 90%} of that workload's counted length, including a cell inside the ring-3
   phase — **≥ 100 repeats per cell**, and within each cell the landing is the same
   instruction every time (identical RIP and identical count). Size the workloads so
   every armed position exceeds `candidate_margin` (≥ 10× the candidate margin in
   counted events is a safe rule): a cell whose `target − candidate_margin` would
   underflow is a workload-sizing bug to fix, never a cell to skip or a wrap to ignore. **Log every per-step
   counter delta**: any delta outside {0, 1} is a no-go finding — this is exactly the
   `CpuBackend::single_step` contract the vtime planner relies on. Report the per-step
   counter-read cost.
7. **Armed overflow across exits** (the INTEGRATION.md §2 assumption — non-timer exits
   while armed must not disturb the plan). Using experiment 6's arming pattern
   (`armed_count = target − margin`), force deterministic VM exits (experiment 3's
   mechanism) at positions both **before `armed_count`** and **between `armed_count` and
   `target`**, resuming after each; verify across ≥ 100 trials that
   `stopped_count − armed_count` is **≥ 0 and ≤ the no-exit observed maximum** (max is
   the criterion — individual values need not have been previously observed) and
   `stopped_count ≤ target` holds exactly as often as in the no-exit baseline.

## Acceptance gates

1. `RESULTS.md` reports all **seven** experiments with raw numbers, the exact attr/config
   used, kernel version, CPU model, the confound-control documentation from the
   Environment section, and one-command reproduction (`./run-all.sh`).
2. Experiment 1 achieves an **exact** match on **both** known-count workloads for a
   go/conditional-go verdict. (A clean, evidenced **no-go** — exact match unachievable,
   residual error characterized — also passes the *task*: the spike's job is the truth,
   not a pass.)
3. Experiment 5's distribution has ≥ 100 trials per schedule cell (≥ 1200 total) and a
   stated max; `RESULTS.md` ends with a **recommended `skid_margin`** (measured max × a
   stated safety factor) and an explicit verdict from the vocabulary
   **go / conditional-go / no-go** against the four properties above. If any property was
   measured on a proxy rather than the real mechanism (e.g. port-I/O instead of VMCALL
   exits), the best available verdict is **conditional-go** with every unmeasured path
   named. A no-go names the failing property with its evidence — a no-go with clean
   evidence is a *successful* spike.
4. Experiments 6 and 7 show 100/100 conforming trials (identical landings; armed-target
   preservation across exits) — or the verdict is no-go.
5. `run-all.sh` is shellcheck-clean and fail-fast per the Environment section.
6. **The experiments actually ran and reproduce**: a fresh `./run-all.sh` on the box
   regenerates RESULTS.md's numbers — exact-count results must match the rerun exactly,
   and the rerun's skid values must not exceed the committed **recommended
   `skid_margin`** (the acceptance bound; the raw observed maximum may legitimately
   differ between runs — that is why the margin carries a safety factor). The verifying
   reviewer will re-run it; a RESULTS.md whose numbers cannot be regenerated fails
   regardless of what it claims.
7. On the box, from the repo root:
   `cargo build --manifest-path spikes/pmu-count/Cargo.toml`,
   `cargo clippy --manifest-path spikes/pmu-count/Cargo.toml -- -D warnings`, and
   `cargo fmt --manifest-path spikes/pmu-count/Cargo.toml -- --check` all pass.
   (Tests are the experiments; no unit-test gate.)

## Non-goals

A production `CpuBackend` (later task — it will *consume* these numbers); integrating with
the `vtime` crate beyond quoting `PlannerConfig::skid_margin`; snapshots/memory work
(spike 2 / task 08); AMD; multi-vCPU; tuning for speed; keeping the code (it may be
deleted after vmm-core lands — write RESULTS.md as if the code were already gone).
