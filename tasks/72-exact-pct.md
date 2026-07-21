# Task 72 — `dissonance/tactics-pct`: exact two-pass PCT + the tactic portfolio as bandit arms (Phase G2/G3)

> **FRONTIER · the schedule-entropy capability.** Probabilistic Concurrency Testing, made *exact*
> by determinism: pass 1 replays and **counts** the `k` scheduling `Moment`s; pass 2 places the
> `d−1` change points **exactly** (uniform over the counted `k`) and realizes them as
> `InjectInterrupt @ Moment`. All crate logic is portable and Mac-gated; only the proof gates are
> box-only. Depends on **task 59** (`InjectInterrupt` enforcement — the PCT lever) and **task 64**
> (the `Tactic` spine); the box gate additionally needs **task 58** (the live `Machine`) and
> **task 69**'s seeded-bug benchmark (69's non-goals defer bugs (iv)/(v) here — **this task builds
> bug (v) into 69's benchmark manifest**; coordinate on its harness). **Task 70** owns the
> Selector/bandit machinery and defines the `Arm` interface in `dissonance/selector-bandit`; this
> crate depends on it and implements the arms; the foreman sequences.

Read first: `tasks/00-CONVENTIONS.md`, `docs/EXPLORATION.md` ("The Proposal seam: Tactic +
EnvCodec" — the **PCT via determinism** paragraph is this task's heart — plus the Phase G roadmap
row), `docs/DISSONANCE.md` ("The host control plane", the `scheduler` boundary case under "The
guest fault model", and "Ruling: single-vCPU is the v1 contract"),
`tasks/59-host-plane-enforcement.md` (the `perturb`/staged-fault path you drive),
`tasks/47-deterministic-preemption-timer.md` + `tasks/55-deterministic-force-exit-preemption.md`
(the exact-count arrival machinery that makes placement instruction-exact),
`tasks/64-explorer-spine-refactor.md` + `dissonance/explorer/src/spine.rs` (the `Tactic`
contract), `tasks/69-*.md` (the benchmark fixture + trial methodology), `tasks/70-*.md` (the
Selector/bandit reward machinery), `dissonance/environment/src/host.rs` (`HostFault`, `Moment`).

## Environment

Surface list (frontier waiver of hard rule 1): `dissonance/tactics-pct/` (one new crate, branch
`task/tactics-pct`; may depend on `dissonance/explorer`, `dissonance/environment`,
`dissonance/tactics-regime`, and `dissonance/selector-bandit` per the task-64 plugin pattern)
and `harmony-linux/` (the planted-bug payloads this task owns — bug (v), depth-2 ordering, and bug (iv),
partition-duration, gate 6 — plus init/manifest wiring per task 69's benchmark conventions). The
census fold, placement sampler, guarantee arithmetic, and arm implementations are pure logic,
macOS + Linux, mock-testable. The proof gates are **box-only** (patched KVM; tasks 58 + 59
landed; task 69's benchmark image): run from an `#[ignore]`d integration test or bin inside the
crate that drives the task-58 server over `control-proto` — read-only use of everything outside
the crate/guest surface, no production changes elsewhere. Pin per `docs/BOX-PINNING.md`; KVM
discipline per the Box-safety block below.

## Context

PCT (ASPLOS 2010): assign random priorities to `n` runnable tasks, run the highest-priority
enabled one, and lower a priority at `d−1` change points chosen among the run's `k` scheduling
steps; a depth-`d` bug is then found with probability **≥ 1/(n·k^(d−1))** — and empirically
`d ≤ 3` covers most concurrency bugs. On a nondeterministic system, `k` is unknown until the run
ends, so implementations place change points online (a reservoir approximation) and the realized
schedule cannot be exactly re-run. Determinism dissolves both defects: pass 1 counts `k` exactly,
pass 2 places exactly, and the pair `(env, change points)` is a complete reproducer. Placement is
*instruction-exact* because the task-47/55 machinery lands a run at an exact retired-instruction
count and task 59 applies `InjectInterrupt` right there. **No nondeterministic fuzzer has this;
Antithesis is single-core-pinned and does not ship it either** [beyond].

**The SMP caveat (restate loudly, invariant c):** under the single-online-vCPU v1 contract
(task 62), PCT here perturbs the **guest scheduler's** interleaving of tasks on the one online
vCPU — a forced interrupt causes a reschedule among in-guest threads — not true parallel SMP
interleavings. That is exactly the interleaving space where real races in a one-online-vCPU guest
live; true SMP is out of scope until re-ruled.

## What to build

### 1. Pass 1 — the counting replay (the census)

Replay the base `Environment` unchanged and enumerate the `k` candidate preemption `Moment`s with
a deterministic **census rule** — v1: the run's timer-interrupt delivery `Moment`s plus any
surfaced `Scheduler`-class decision `Moment`s, as recorded in the `RunTrace`/recorded env. The
census is a pure fold over a recorded run (portable, mock-testable); a finer census is a config
knob (`k` trades against the bound). Same env ⇒ same `k`, by determinism — gated on the box.

### 2. Pass 2 — exact priority assignment + change-point placement

Draw the `d−1` change points uniformly **without replacement** over the counted `k` (a seeded
Fisher–Yates prefix — exactly uniform by construction, unlike the online reservoir); map each to
its census `Moment`; stage each as `perturb(InjectInterrupt { vector }, moment)` via task 59 (v1
vector: the guest's LAPIC-timer/reschedule vector — document the choice). Where a workload
surfaces `Scheduler`-class decisions (SDK-cooperative), a `PctTactic` (a spine `Tactic`) answers
them in PCT priority order, priorities drawn per-task from the seed; black-box workloads get the
interrupt lever alone. `n` is a per-workload config knob in v1. Expose
`guarantee(n, k, d) -> Rational` computing `1/(n·k^(d−1))` in exact integer arithmetic,
overflow-checked (rejected, never wrapped).

### 3. The portfolio as bandit arms (G3)

The `Arm` trait is **defined in `dissonance/selector-bandit`** (task 70; hard rule 2 — the
selection policy is the consumer of arms). This crate depends on selector-bandit and
**implements** the arms; it never defines the interface.

Register five arms (Coyote's lesson: a diversified portfolio dominates any single strategy):
**quiet** (all-nominal — the determinism canary + baseline histories), **fault-regime** (wraps
task 71's `RegimeTactic`), **pct(d)** (this task, `d ∈ {2,3}`; `prepare` runs the two passes),
**value-fuzz** (supply-class value perturbation — v1-thin), **swizzle** (clustered rolling
`Process` restarts — v1-thin). Arm-level reward is **campaign-root-routed** per 70's ruling —
distinct from the spine's exemplar-keyed `Selector::reward`, no spine change — this crate ships
arms, never selection policy.

## Invariants (restate in the crate docs; each is gated)

- **(a) OPEN-LOOP Modulation.** `PctTactic` never reads Sensor/Archive output mid-run; identical
  `(state, point, rng)` ⇒ identical answer. Both passes are between-runs work — nothing here
  steers a run from live feedback.
- **(b) Determinism discipline.** Seeded PRNG only; no floats — the guarantee, census, and
  placement are exact integer/rational arithmetic.
- **(c) Single-online-vCPU v1 contract** (task 62), as stated in Context: guest-scheduler
  interleaving, not SMP.

## Acceptance gates

1. **Standard suite** green on `dissonance/tactics-pct` (build / nextest / clippy `-D warnings` /
   fmt / deny), all-features, macOS + Linux.
2. **Portable proptests (≥256 each):** census determinism over mock traces (same trace ⇒ same
   `k`); placement determinism (same `(k, d, seed)` ⇒ same change points) and exact uniformity
   (exhaustive count over small `k` — every position equally likely); open-loop proptest for
   `PctTactic` (the task-64 pattern); `guarantee` arithmetic exact, overflow rejected; no
   `f32`/`f64` in the crate.
3. **Box gate — the proof (Klees-style trial discipline; cite task 69's methodology):** on
   task 69's planted **depth-2 ordering bug**, at an equal branch budget per arm, `pct(2)` finds
   the bug and the IID fault tactic does not — report per-trial found-rate/time-to-bug tables over
   the trial count 69 prescribes; no single-run anecdotes.
4. **Box gate — pass-1 count stability:** the counting replay of the same env, run twice, yields
   the same `k` twice (the determinism check on the census rule itself).
5. **Arms registered:** the five arms load into a toy-`Machine` campaign; the `quiet` arm
   reproduces the nominal baseline bit-identically (the determinism canary lives). Live campaign
   integration rides task 70's gates.
6. **Box gate — bug (iv) / fault-regime arm (the Phase-G roadmap gate: "finds a
   partition-duration bug the IID version misses"); GATED ON TASK 61 LANDING:** build bug (iv)
   (partition-duration) into 69's manifest; at an equal budget the **fault-regime arm** finds it
   and the IID fault tactic does not. Requires task 61's standing net faults — DISSONANCE.md's
   standing-fault sequencing guard (the Moment→VTime map / eligibility hook) is 61's to own.
   Gates 1–5 do not block on 61; this gate alone waits for it.

## Box-safety (CRITICAL)

Stock KVM = **1396736**; the patched module is larger. ALWAYS leave the box on stock + verified
after every run: `pkill -9 -f pct_box_gate` (and any `live_*`) FIRST → wait
`lsmod | grep '^kvm_intel'` users=0 → `rmmod kvm_intel kvm; modprobe kvm; modprobe kvm_intel` →
verify size 1396736 on a FRESH ssh connection. SSH drops (exit 255) on pkill/rmmod are normal —
reconnect + verify. Pin builds/tests to `taskset -c 2` (`docs/BOX-PINNING.md`). Run gates in the
foreground and READ results before reporting; no detached pollers + idle.

## Prior art

- **PCT** (Burckhardt et al., ASPLOS 2010) [beyond] — the priority scheduler + bug-depth concept
  (`d ≤ 3` covers most concurrency bugs); made exact here by determinism.
- **PCTCP** (OOPSLA 2018) [beyond] — the message-passing variant (chain partitioning over
  deliveries); named follow-on once task 61 makes message deliveries schedulable.
- **RFF** (ASPLOS 2024) [beyond] + **Krace** (S&P 2020) [beyond] — schedule-space coverage
  metrics; the deferred feedback channel — a timing-dimension signal code coverage cannot see.
- **Coyote** (TACAS 2023) [eng] — the portfolio lesson: diversified strategies as arms of a
  bandit; realized here against task 70's Selector.

## Non-goals

- PCTCP message-passing chain partitioning — named follow-on, gated on task 61's net vertical
  making message deliveries schedulable.
- RFF/Krace schedule-space coverage as a feedback channel — named follow-on; no Sensor/CellFn
  work here.
- SMP / multi-vCPU anything — task 62's ruling stands; SMP enters later, if ever, as a `Machine`
  capability plus a vCPU-switch decision class.
- Bandit/selection policy, reward math, or the Selector itself — task 70 owns them; this crate
  ships arms.
- Enforcement changes — task 59's `perturb` path is consumed as-is; no `consonance/vmm-core`
  diffs.
