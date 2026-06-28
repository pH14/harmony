# Task 52 — deterministic HLT-resume: complete the event-driven V-time clock

> **The second of the two container-runtime primitives (task 47 was the first), and a correction to a
> design deviation.** Today the run loop can only advance V-time *by executing* — so when the guest
> goes idle (`HLT`) it freezes, and the VMM treats the first `HLT` as terminal
> (`vmm.rs`: `Exit::Hlt => terminate`). That was never the intended model; it was papered over because
> every prior workload was hand-shaped to never idle. **Real `runc` is the first workload that
> genuinely idles mid-operation** (its create→exec handshake blocks both the parent in `waitpid` and
> the Go container-init on a futex/exec-fifo at the same instant → kernel idle task → `HLT`), so the VM
> dies before the container comes up — proven by the task-48 box gate (`runc_launched=true`,
> `runc_rc=None`, `terminal=Hlt`; see PR #23 + `guest/linux/IMPLEMENTATION.md` task-48 note).
>
> **The principle this task restores:** the run loop is a **discrete-event loop** — always advance
> V-time *to the next scheduled event*, reaching it **by executing** when there is runnable work
> (task 47 single-steps to the exact retired-branch count) or **by jumping** when the guest is idle
> (advance straight to the armed timer deadline, inject, resume). Retired-branches are the *measure of
> execution*, not the clock's only source of motion.

Depends on **task 47 (PR #15, merged)** — `run_until` + the LAPIC-timer deadline machinery this reuses.
Branch from `main`.

## Environment

The portable decision logic (when/where to advance, the discriminator, the clock/counter accounting)
is **SimCpu-testable on macOS + Linux** and MUST carry unit + property + (where it fits) Kani
coverage. The end-to-end proof is **box-only** (patched KVM + the built Docker image, det-cfl-v1 host):
re-running the task-48 `live_runc_postgres` gates. Pin per `docs/BOX-PINNING.md` (core 4 is free —
task 41 was struck); **always revert KVM to stock `1396736` + verify** after any patched run.

Read first: `tasks/00-CONVENTIONS.md`, `tasks/47-deterministic-preemption-timer.md` (the deadline +
`run_until` machinery + the B≡A counter invariant this must preserve), `consonance/vmm-core/src/vmm.rs`
(`step()` exit dispatch ~1336–1358, `preemption_deadline()` ~1834, `on_deadline()`), the `VClock`
(`work_for_vns` / the vns↔work axes), `consonance/lapic/src/lib.rs` (`next_timer_deadline()` — the
deadline `D`), and `tasks/48-runc-postgres.md` + `guest/linux/IMPLEMENTATION.md` (the finding).

## The change

### 1. Discriminate idle-HLT from terminal-HLT (the clean signal)

On `Exit::Hlt`, the guest is either *waiting for an interrupt that will come* or *dead*. Use the same
signal real CPUs use — the guest's **interrupt-enable flag (`RFLAGS.IF`)** plus whether a timer is
armed:

- **`IF == 1` AND a LAPIC timer is armed** (`preemption_deadline()` is `Some`) → **resumable idle**:
  advance + inject + resume (below).
- **`IF == 0`, or no timer armed** → **terminal** halt (the kernel's final `cli; hlt` after
  poweroff / a wait nothing will ever satisfy) → `terminate(TerminalReason::Hlt)`, exactly as today.

The minimal-boot poweroff and every existing terminal path are `IF==0`/no-timer, so they MUST remain
terminal (asserted by the no-regression gate).

### 2. Advance V-time to the next event (`D`) — the jump

`D` is **not new** — it is the currently-armed deadline `preemption_deadline()` already computes (and
task 47 already uses for `run_until` during execution): `work_for_vns(lapic.next_timer_deadline())`.
On idle HLT, reach that same `D` by **jumping** instead of by executing: advance the guest-visible
clock to `D`, inject the LAPIC timer vector, and resume `step()`. Edges already handled by existing
code — reuse them: deadline **already in the past** ⇒ fire immediately (zero jump — task 47's
`TargetInPast`); **no deadline** ⇒ falls into the terminal branch above.

### 3. The load-bearing invariant — do NOT fabricate retired-branches

A jump executes **no instructions**, so the retired-branch counters MUST stay true counts of executed
branches — task 47's `run_until` PMU target and the **B≡A invariant** (Counter B = backend PMU ≡
Counter A = vmm-core WorkSource) stay intact over the *execution* component. Model the guest clock as:

```
guest_vtime  =  execution_derived_vtime(real_retired_branches)  +  accumulated_idle_vtime
```

On an idle HLT, add `D − guest_vtime` to `accumulated_idle_vtime` (so `RDTSC` reads `D` and the timer
fires at the right *guest-perceived* instant) — **without** touching the retired-branch count. The
next timer arming converts its vns deadline back to a real-branch `run_until` target net of the idle
term. **Put this accounting in the portable `vtime` planner**, not the box-only `kvm_sys`/`patched_kvm`
FFI (which excludes itself from coverage/mutation per `docs/CODE-QUALITY.md`), so it is exercised by
SimCpu tests — the kvm layer stays thin FFI that just enters/exits.

## Determinism (the whole point)

Every input to the jump is a pure function of the seed: `D` (from the guest's own timer programming +
the fixed `timer_hz`/`VClock`), the current clock, and `RFLAGS.IF` are all seed-derived; no wall clock
enters. Two same-seed runs idle at the identical point, jump the identical amount, and inject the
identical vector. The idle period becomes a **deterministic constant**, never a nondeterminism source.

## Acceptance gates

1. **Portable, SimCpu-tested (the primary gate — not the box):**
   - Unit + **property** tests (proptest ≥256; stateful where it fits) in the `vtime` planner: an idle
     advance lands the clock at exactly `D`; the timer fires at the right guest-perceived time;
     `guest_vtime` is monotonic; retired-branch counts are **never** incremented by a jump (B≡A holds
     over execution); overdue ⇒ zero-jump-fire; no-timer/`IF==0` ⇒ terminal. Test against an
     **independent** reference model of "elapsed = execution + idle", not a mirror of the impl.
   - Kani candidate: the clock arithmetic saturates (no wrap; a far-future `D` clamps).
2. **No regression (byte-identical):** M1/M2/P6, the det-corpus goldens, and the minimal-boot +
   bare/OCI Postgres `state_hash`es are **byte-unchanged** (the change is strictly additive on the
   idle-HLT path; every existing terminal is `IF==0`/no-timer and stays terminal). Standard gates green
   (build/test/clippy `-D`/fmt/deny/coverage/mutants/public-api); `unsafe` ⇒ Miri.
3. **Box — the end-to-end proof (the money-shot):** the task-48 `live_runc_postgres` gates
   **r1/r2/r3 now pass** unchanged — real `runc` runs the Postgres OCI container, **deterministic-twice**
   (bit-identical serial + `state_hash`), seed-sensitive. Quote the equal digests + a sample UUID/ts.
   Revert KVM to stock `1396736` + verify.
4. **Corollary (confirm, don't assume):** check whether timer-driven waits (`sleep`/`nanosleep`/futex
   timeouts) now make progress on their own (they froze before, forcing the task-37/38 cooperative-poll
   workarounds). If so, note it in `IMPLEMENTATION.md` as a capability unlocked; do **not** refactor
   existing guest scripts in this task.

## Non-goals

Changing behavior on the **execution** path (task 47 owns that — this is purely the idle-HLT addition);
multi-vCPU / other interrupt sources (single-vCPU, LAPIC-timer-only — generalize "next event" later if
needed); MWAIT; any contract/hash-schema change. Do not fall back to the task-38 `unshare` shim — that
is kept for comparison, not a substitute for this gate.

## Box-run (foreman, after merge)

Reuse the task-48 setup: `/root/ht42` checked out `task/hlt-resume` + built image
(`make -C guest fetch && make -C guest/linux docker-image`), then
`/root/run-patched-ht42.sh 5400 cargo test -p vmm-core --test live_runc_postgres -- --ignored
--nocapture --test-threads=1 r2_runc_postgres_deterministic_twice_patched` (then r1/r3). Always reverts
to stock `1396736` via the EXIT trap. Capture the evidence into `guest/linux/IMPLEMENTATION.md`.
