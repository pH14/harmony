# Task 47 — Deterministic preemption timer: PMU PMI at the V-time deadline (`run_until` Phase 2)

> **Integrator directive (2026-06-26).** This is the one missing primitive for "run real
> software stacks, deterministically." Today a single-vCPU guest only yields the CPU at a
> *natural* VM-exit. A busy-spinning or compute-bound thread that never exits (the Go runtime's
> `procyield`/`osyield`, runc/containerd/k8s) starves every other thread forever — there is no
> preemption, so the V-time LAPIC timer never gets a chance to fire and the guest scheduler never
> runs. **Make V-time active:** when the next scheduled event (the LAPIC-timer deadline) is at
> V-time `T`, program the retired-branch counter to overflow and force a VM-exit at `T`, inject the
> pending timer interrupt right there, and re-enter. The timer now fires *on time even mid-spin*,
> at a point that is a **pure function of the seed** (`T` retired branches) — so it stays
> bit-identical across two same-seed runs. This is general preemptive-multitasking determinism, not
> a Go hack, and it lives in the patched-KVM backend you already own.
>
> **Environment:** box-only (patched KVM + Intel PMU; `perf_event` is unavailable on the Mac and the
> live PMU/single-step path cannot run there). `ssh hetzner`; pin per `docs/BOX-PINNING.md` (task 41
> owns core 4 while PR #12 is open — use **core 2**). Self-serve box gates via git (rsync is blocked;
> `git archive | ssh tar` or fetch the branch on the box). ALWAYS revert KVM to stock + verify.

Read `tasks/00-CONVENTIONS.md`, `docs/INTEGRATION.md`, and these existing pieces in full before
writing any code — the seam is already designed-in and partly stubbed, your job is to implement
**Phase 2**, not to invent a new abstraction:

- **`consonance/vmm-backend/src/backend.rs`** — the `Backend` trait. `run_until(deadline: Vtime) ->
  Result<Exit>` (the "§2 inversion seam — PMU overflow-early + single-step under the hood"),
  `inject(event: Event)`, `set_pending_irq(Option<u8>)`, `take_accepted_interrupt()`, and the
  `Exit::Deadline` variant (`exit.rs`). On the live backends these are documented as **Phase 2** and
  currently return `Unsupported { what: "run_until" }` / `{ what: "inject" }`.
- **`consonance/vmm-backend/src/kvm_sys.rs`** (`KvmBackend`) and **`patched_kvm.rs`**
  (`PatchedKvmBackend`, which delegates `run_until`/`inject` to its inner `KvmBackend`) — the
  implementation site.
- **`consonance/vtime/src/planner.rs`** — the `CpuBackend` trait (`run_until_overflow(armed_at) ->
  Result<u64>`, `single_step() -> Result<u64>`) and the planner that orchestrates *arm-overflow-early
  → single-step the skid margin to the exact count*. `consonance/vtime/src/sim.rs` is the existing
  in-memory `CpuBackend` impl + its property tests (`overflow_respects_contract`,
  `overflow_at_passed_count_stops_immediately`) — your live impl must satisfy the **same contract**.
- **`consonance/vmm-core/src/work_perf.rs`** (`PerfWorkCounter`) — the real guest-retired-branch
  counter (`PERF_TYPE_RAW 0x1c4`, `exclude_host=1`, count-neutral across exits). Its module doc
  already names the unimplemented "overflow-arm + single-step precise-injection path" — that is this
  task. The PMU overflow is armed by setting the `perf_event` **sample period** + enabling
  `PERF_EVENT_IOC_REFRESH`/an overflow signal (`fcntl` `F_SETOWN`/`F_SETSIG` + `O_ASYNC`, or a
  `poll`/`read` on the fd) so the counter raises at `armed_at` retired branches.

## What to build

1. **A live `vtime::CpuBackend`** bound to the real PMU + KVM single-step. `run_until_overflow(armed_at)`
   arms the retired-branch counter to fire at `armed_at`, runs the vCPU (`KVM_RUN`) until the overflow
   signal forces an exit (or a genuine guest exit happens first), and returns the work count reached.
   `single_step()` does one single-stepped `KVM_RUN` (`KVM_SET_GUEST_DEBUG` with
   `KVM_GUESTDBG_ENABLE | KVM_GUESTDBG_SINGLESTEP`) and returns the new count. Decide where this impl
   lives and how `KvmBackend` reaches a `PerfWorkCounter` + the vCPU fd — `PerfWorkCounter` currently
   sits in `vmm-core` but `run_until` is a `vmm-backend` method, so resolving that layering cleanly
   (move/share the counter, or implement `CpuBackend` on a backend-owned counter) is **the central
   design decision of this task** — call it out in `IMPLEMENTATION.md` and keep `vmm-backend` free of
   a `vmm-core` dependency (no upward deps; rule 3).
2. **`KvmBackend::run_until(deadline)`** — drive the vtime planner: arm the overflow at
   `deadline - skid_margin`, run, then `single_step` the remaining branches to land at **exactly**
   `deadline` retired branches, and return `Exit::Deadline`. A genuine guest exit (IO/MMIO/HLT/…)
   before the deadline returns **that** exit instead, short of `deadline` — never past it. Account for
   **PMU skid** (the PMI lands a few branches late): arm early, single-step to the exact count — the
   same machinery the planner already models in `sim.rs`. Then implement `KvmBackend::inject(event)`
   (the `KVM_NMI` path + the one-shot maskable convenience over `set_pending_irq`).
3. **Wire the VMM loop** (`consonance/vmm-core/src/vmm.rs`) to call `run_until(next_timer_deadline)`
   instead of (or alongside) `run()` when a LAPIC timer is armed, so a guest that would otherwise spin
   forever is preempted at the V-time deadline: on `Exit::Deadline`, deliver the LAPIC timer
   (`set_pending_irq` re-arbitrated per entry) and re-enter. Keep the existing HLT→V-time-warp fast
   path for the *quiescent* case (a HLTed guest still warps to the next deadline; preemption is only
   needed when the guest does NOT exit on its own).

## Determinism (the whole point)

- The preemption instant is `T` **retired branches** = a pure function of the seed. Same seed ⇒ same
  branch count ⇒ same preemption instant ⇒ bit-identical execution **even with preemption**. It is
  **not** wall-clock. The single-step-to-exact is what makes it precise and reproducible despite PMU
  skid — without it the skid would make the preemption point seed-dependent-plus-noise (a leak).
- `Exit::Deadline` must land at exactly `deadline` (assert work count == deadline, not ≈). A run that
  lands at `deadline ± skid` is a determinism bug — report it, don't widen a tolerance to paper it.

## Acceptance gates

1. **Contract (box):** the live `CpuBackend` satisfies the **same** property contract as `sim.rs`
   (overflow stops at-or-before the armed count, single-step advances by the real retired-branch
   delta). `run_until(deadline)` lands at **exactly** `deadline` retired branches and returns
   `Exit::Deadline`; an injected guest exit before the deadline returns that exit short of `deadline`.
   Prove with a property/stateful test against the independent `sim` model (≥256 cases), plus a live
   box run quoting "armed at D−margin, single-stepped k branches, landed at D".
2. **Preemption (box), deterministic-twice:** a deliberately **busy-spinning** guest (a tight loop
   with no natural VM-exit) is preempted at the V-time LAPIC-timer deadline → the timer vector is
   injected → the guest observes it and makes progress (e.g. a second thread/handler runs and streams
   a marker to `ttyS0`). Run twice at the same seed: **bit-identical** serial + identical `state_hash`.
   Run at a different seed: the preemption instant (and thus the interleaving) **differs** — quote the
   differing branch counts, proving it is genuinely seed-driven.
3. **Headline (box) — the unlock:** **`runc` actually runs the Postgres OCI container** (the real
   `runc`/Go-runtime path, **no** `unshare`/`chroot`/`setpriv` workaround from task 38), the Postgres
   UUID/time workload (task 42) streams to `ttyS0`, `GUEST_READY`, clean shutdown — and it is
   **deterministic-twice** (serial incl. UUIDs/timestamps + `state_hash`). Quote the equal digests and
   a sample UUID/timestamp. **If the Go runtime surfaces a genuinely NEW blocker beyond preemption**
   (something preemption alone does not resolve), implement what you can, prove gates 1–2 + as much of
   3 as the primitive unlocks, and **document the precise next blocker** as the frontier — do not fake
   the gate or relax it.
4. **No regression:** M1/M2/P6 + det-corpus + unison goldens **byte-identical** (the `run_until`
   path is additive — the existing `run()` + HLT-warp path for quiescent guests is unchanged);
   standard gates green (`cargo build`/`test`/`clippy -D warnings`/`fmt`); any new `unsafe` (perf
   overflow signal, `KVM_SET_GUEST_DEBUG`) carries a `// SAFETY:` and runs clean under Miri behind a
   seam, or documents why the privileged path can only be exercised on the box; revert KVM to stock
   `1396736` + verify.

## Public API / integration

You are **implementing** existing trait surface, not adding new public API: `Backend::run_until`,
`Backend::inject`, `Exit::Deadline`, and a live `vtime::CpuBackend` impl. Do not change the
`Backend`/`CpuBackend`/`Exit` signatures or the `Vtime`/`Event` types — other crates compile against
them. If you genuinely need a new shared item (e.g. a backend-owned perf-counter type), keep it inside
`vmm-backend`, document it, and note it in `IMPLEMENTATION.md`. No CPU/MSR contract or hash-schema
change. `unsafe` is granted **only** for the `perf_event` overflow-signal wiring and
`KVM_SET_GUEST_DEBUG` single-step ioctls (the named purpose); every block needs a justifying
`// SAFETY:`.

## Non-goals

The cooperative-yield shim (intercepting Go's `procyield`/`osyield` → doorbell) — PMU-preemption is
the real answer; the shim is the fragile, guest-touching fallback and is explicitly **out of scope**.
Multi-vCPU. Changing the determinization mechanism for RDTSC/RDRAND (unchanged). k8s/k3s itself
(runc + Postgres is the proof; the broader stack rides this primitive but is a later milestone).
This is **ROADMAP D4**'s machinery — boot/exec performance (faster `run_until` stepping) may *also*
ride it, but is not required here.
