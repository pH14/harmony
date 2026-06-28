# Task 53 — deterministic i8254 vPIT: give real Linux a tick (Deterland-style)

> **The missing primitive under the whole frontier.** The box proved (three ways: a live `idle_action`
> trace, task 37's own notes, and Linux's boot log) that real Linux here boots in **virtual-wire APIC
> mode** (`APIC: ACPI MADT or MP tables are not detected` → `Switch to virtual wire mode`) and therefore
> **registers no clock-event/tick device** — only the TSC *clocksource*. With no tick: timer interrupts
> never fire, `sleep`/`nanosleep`/futex-timeouts never wake, **runc's Go runtime deadlocks**, and tasks
> 47 (preempt-at-timer) and 52 (HLT-resume-at-timer) have no armed timer to act on. This is why every
> task-37/38 workload needed cooperative-polling / `unshare` workarounds.
>
> **The fix — the published, proven approach.** Model a **deterministic i8254 PIT** as the guest's
> clock-event. In virtual-wire mode Linux's natural tick source is the legacy **PIT** (always at I/O
> ports `0x40`–`0x43`, needs no ACPI/MADT) — so a vPIT makes Linux register a real periodic tick with no
> firmware tables. This is exactly **Deterland**'s design (Wu & Ford, *Deterministically Deterring Timing
> Attacks in Deterland*, arXiv:1504.07070): it models a **vPIT + vRTC** as "the only fine-grained notion
> of time observable to guest VMs," **disables the LAPIC timer**, and delivers the vPIT's interrupts via
> **performance-counter overflow + single-step-to-exact** — the identical mechanism task 47 already
> built. Deterland ran real **unmodified Ubuntu Linux** this way; Antithesis productionizes the same
> class of design to run real Docker. **Cite Deterland (§4.1.2, §5.1, §5.3) in the PR and
> `IMPLEMENTATION.md`.**
>
> We have independently built the rest of the blueprint already — V-time = instruction/branch count
> (Deterland §5.2); task 47 = arm-early + single-step-to-exact (Deterland §5.1: "pauses the VM 250
> instructions early, then uses Monitor TF to single-step … to the target"); task 52 = on HLT "advance
> the artificial time to … the next vTimer event" (Deterland §4.1.2); IF-gated injection (§5.3). The vPIT
> is the **last missing piece**: a timer *device* the guest actually adopts as its clock-event.

Depends on **task 47 (PR #15, merged)** and **task 52 (PR #27 — carry its HLT-resume forward, re-keyed)**.
**Branch from `task/hlt-resume`** (merge it in) so task 52's reviewed HLT-resume + V-time idle work is the
base; this task **supersedes PR #27** (its mechanism lands here, re-keyed to the vPIT). Read the contract's
PIT row first: `docs/CPU-MSR-CONTRACT.md` already specifies **"the emulated PIT at 1.193182 MHz of V-time"**
in its consistency chain — use that frequency (no contract change).

## Environment

The vPIT model + the re-keyed deadline/idle logic are **pure-logic, SimCpu-testable on macOS + Linux** and
MUST carry unit + property + (where it fits) Kani coverage. The end-to-end proof is **box-only** (patched
KVM + det-cfl-v1 host): the task-48 `live_runc_postgres` gates + a Linux-golden **re-baseline**. Pin per
`docs/BOX-PINNING.md`, **always revert KVM to stock `1396736` + verify**.

Read first: `tasks/00-CONVENTIONS.md`; **Deterland arXiv:1504.07070 §4.1.2 (vTimer), §5.1 (precise
execution / skid), §5.3 (Interrupts)**; `docs/CPU-MSR-CONTRACT.md` (the PIT 1.193182 MHz + CPUID 0x15 rows
+ the consistency chain); `tasks/47-deterministic-preemption-timer.md` + `tasks/52-deterministic-hlt-resume.md`
and their `IMPLEMENTATION.md` notes; `consonance/vmm-core/src/vmm.rs` (`preemption_deadline`, `on_hlt`/
`idle_action`, `resume_idle`); `consonance/lapic/src/device.rs` (the existing timer-device model to mirror);
`guest/linux/IMPLEMENTATION.md` (task 37's "no clock-event device" finding + the runc deadlock).

## The change

### 1. Model a deterministic i8254 PIT (the new device)
A small, well-understood legacy device: I/O ports `0x40`–`0x43` (counters 0–2 + the mode/command register).
Counter **0** drives **IRQ0** — the system timer. Model the standard operating modes Linux uses (at least
**mode 2** rate-generator and **mode 3** square-wave; modes 0/4 as needed), the counter latch/read-back, and
the BCD/binary + access (lobyte/hibyte) bits. The countdown decrements at **1.193182 MHz of V-time** (the
contract's frequency). When counter 0 reaches 0 it **raises IRQ0** and (periodic modes) reloads. Put the
decision logic in the **portable layer** (mirror `lapic/src/device.rs`'s structure), not the box-only FFI,
so coverage/mutation/property/Kani all apply. The vPIT state is a pure function of V-time + the guest's port
writes → deterministic by construction.

### 2. Deliver IRQ0 via the existing PMU-overflow + single-step (task 47), and re-key the deadline source
The vPIT's next IRQ0 is a **deterministic V-time** (current count × period, from the guest's programming).
Make **`preemption_deadline()` source the next-timer deadline from the active clock-event device — the vPIT**
(not the unused LAPIC timer). Then task 47's `run_until` arms the PMU to overflow early and single-steps to
the exact retired-branch count (Deterland's skid solution), and the VMM **injects IRQ0** through the
interrupt-controller path when the guest's `RFLAGS.IF` is set. The **LAPIC timer model stays present but
dormant** (real Linux won't program it in virtual-wire mode — mirror Deterland disabling it).

### 3. Re-key task 52's HLT-resume to the vPIT
On `Exit::Hlt`, task 52's `idle_action`/`resume_idle` must source the wake deadline from the **vPIT** (the
real clock-event), keeping the same skid-free, intercept-aligned anchoring task 52 already got right (do
**not** reintroduce a raw-work-read-at-HLT — that P1 was the central Deterland hazard). The IF=1 +
deliverable-wake discriminator carries over.

## Determinism (the whole point)
Every input is seed-derived: the vPIT countdown (V-time + guest port writes), the IRQ0 deadline, and the
PMU-overflow+single-step delivery are all deterministic; no wall clock enters. Two same-seed runs tick at
identical V-times and deliver IRQ0 at identical retired-branch counts. Cite Deterland §5.1 for the skid
handling (arm early + single-step) — it's the documented state-of-the-art and what task 47 implements.

## Acceptance gates

1. **Portable (primary):** vPIT model unit + **property** (proptest ≥256) + Kani tests vs an **independent**
   reference model — countdown/reload across modes 2/3, latch/read-back, lobyte/hibyte, IRQ0 fires at the
   exact V-time, saturating arithmetic (no wrap on a hostile reload). The re-keyed `preemption_deadline`/
   idle logic tested against SimCpu, **including the skid-modeling test** task 52 added (a skid-perturbed
   counter read must not perturb V-time). Standard gates green (build/test/clippy `-D`/fmt/deny/coverage/
   mutants/public-api); `unsafe` ⇒ Miri.
2. **Box — the empirical proof, RUN FIRST (before the re-baseline):** on a 52+53+48 tree,
   `live_runc_postgres` **r1/r2/r3** — real `runc` reaches **GUEST_READY** and runs the Postgres OCI
   container **deterministic-twice** + seed-sensitive (the thing that's been blocked all along). Also quote
   the **boot log** now showing the clock-event registering + ticks firing (e.g. a "Local APIC/PIT timer"
   clockevent line that was absent before), and confirm `idle_landings`/preemptions are non-zero on real
   Linux (47/52 finally exercised).
3. **Corollary (confirm):** a `sleep`/`nanosleep` in the guest now **returns** (it never did — task 37).
   A tiny test is enough; don't refactor the existing cooperative-polling scripts in this task.
4. **Deliberate re-baseline:** any tick changes the boot, so the **Linux goldens re-base** —
   `live_linux_boot`, `live_postgres`, `live_postgres_docker`, `live_branching_demo`: re-run each
   **deterministic-twice** on the box and record the new `state_hash`/serial goldens (quote the equal
   digests). **M1/M2/P6 + the det-corpus goldens are NON-Linux and MUST stay byte-identical.** Revert KVM
   to stock `1396736` + verify after every patched run.

## Non-goals
No ACPI/MADT/MP-table (we use the vPIT, deliberately — Deterland disables the LAPIC timer; don't chase the
LAPIC-timer-clockevent path); no CPU/MSR-contract frequency change (use the already-specified 1.193182 MHz
PIT + 25 MHz crystal + 2.0 GHz TSC consistency chain); don't delete the LAPIC-timer model (leave it dormant);
no vRTC unless a gate needs it (PIT counter-0 / IRQ0 is the tick — vRTC is a later add if something needs
wall-clock-of-day). HZ tuning (the config is HZ=100; Deterland overhead is ~5% @100ms tick, ~30% @1ms) is a
**secondary** lever — only lower HZ if the box runs are intractably slow, and document it; default to the
existing HZ=100.

## Box-run (foreman, after merge of the worker's Mac-side delivery)
Reuse the task-48 box setup (`/root/ht42`, built image). On a `task/hlt-resume`-based tree merged with
`task/runc-postgres`: `/root/run-patched-ht42.sh <timeout> cargo test -p vmm-core --test live_runc_postgres
-- --ignored --nocapture --test-threads=1 r2_…` (then r1/r3), plus the re-baseline runs of
`live_linux_boot`/`live_postgres`/`live_postgres_docker`. Always reverts to stock `1396736` via the trap.
Capture all evidence (runc GUEST_READY + det-twice digests, the boot-log clockevent line, the sleep-returns
corollary, and the re-baselined golden hashes) into `guest/linux/IMPLEMENTATION.md` + `vmm-core/IMPLEMENTATION.md`,
**citing Deterland**.
