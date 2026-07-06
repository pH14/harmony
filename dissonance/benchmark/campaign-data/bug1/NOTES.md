# Bug 1 (fault-timing) box campaign — data, recipe, and the resume state

## Status (2026-07-06): infra + logging validated live; calibration blocked on a
## seal/arm interaction. GO/NO-GO #2 still PENDING. This is the checkpointed
## resume point — everything below is on `origin/task/signal-bug-correlation`.

## What is proven live (real patched KVM)
- Socket console capture works; the **real** LogSensor/CellFnV1 produces cells
  (0 → 3) once the guest logs realistically (`campaign-super` now logs bug-agnostic
  operational lines: lifecycle phase / backpressure / checkpoint).
- The campaign image was rebuilt with the logged `campaign-super`
  (`initramfs-campaign.cpio.gz`, 2026-07-06 15:04).
- Box hygiene: worktree `~/harmony-t69m2`; `source ~/.cargo/env`; `taskset -c $CORE`;
  `/root/box-window.sh acquire/release`. **Runs MUST be foreground with the release
  inline** — a background/timed-out ssh orphans the guest and holds patched KVM (seen
  twice); recover by `kill -9 <pid>` (exact PIDs, not the CI runner) then
  `rmmod kvm_intel kvm; modprobe kvm; modprobe kvm_intel` and verify `1396736` on a
  fresh ssh.

## THE BLOCKER (calibration) — a fault offset > 0 is rejected "verb not supported"
- `calibration.json` sets bug 1's gpa to the real ledger **canary** gpa on the
  logged image (`0x7fbe2000` = 2143166464; printed as `CAMPAIGN_LEDGER_GPA` at boot,
  deterministic).
- A fault with window `[1500,1520]` (offset ~500 past the seal) fails at **branch 0**
  (before any fire) with `control error: verb not supported by this backend`
  (= `ControlError::Unsupported`, `vmm-core/src/control.rs` `check_fault_admissible`).
- A fault with window `[1003,1004]` (offset ~0, `at ≈ floor`) does **not** fail — the
  run progresses. So the **real gpa is fine; the fault OFFSET is the issue** (my
  earlier "gpa-specific" guess was wrong).
- Diagnosis: `at == floor` applies immediately (no arming); `at > floor` needs the
  exact-count arrival seam (`Vmm::arm_arrival` / `can_arm_arrival()`), which returns
  Unsupported here even though task-59 implemented arm_arrival and its box gate passed.
  Likely cause: **`seal_base`'s snapshot-retry lands the base at a point that is
  quiescent-for-snapshot but NOT arm-capable (synchronized)**, unlike task-60's seal.
  Confirm by comparing `can_arm_arrival()` at the bench seal vs the task-60 seal.
### FOREMAN RULINGS (2026-07-06) + the confirmed fix design
- **Ruling: option (a)** — make `seal_base` land at an arm-capable synchronized
  boundary (keep a REAL window search). Option (b) (offset-0) is REJECTED as primary:
  it thins exactly the timing-window search this benchmark discriminates on.
- **First CONFIRM the diagnosis**: compare `can_arm_arrival()` at the bench seal vs
  the task-60 seal. Practical confirm without server instrumentation: run task-60's
  `conductor campaign box` (its own `--gpa-base`/`--window-*`) against the **new logged
  image** with an offset>0 fault. If it ALSO fails "verb not supported", the arm-
  incapable seal is a property of the image/seal, not bench-specific (the realistic
  logging moved the seal point off a synchronized boundary); if task-60 works, the
  bench harness differs and that difference is the bug.
- **Scope**: fix INSIDE this task's surface if the bench harness can do it against
  EXISTING vmm-core APIs (a seal-retry *condition*). Only if it needs a vmm-core/control
  change: keep it minimal, call it out in the PR as a task-59 seam amendment, do NOT
  split into a separate task (M2 blocks 70/72/76). If that change is non-trivial
  (semantics, not a retry condition) → checkpoint + escalate, don't build it.

### Refined root cause + the in-surface fix (retry-condition, existing APIs)
The snapshot-retry in `seal_base` retries only on `NotQuiescent`, so it lands at the
first **snapshottable** point — which is NOT necessarily **synchronized / arm-capable**
(a deadline/quiescent stop can be off a V-time intercept). A fault at offset>0 then
can't arm. Fix `seal_base` to also require arm-capability before committing the base:
after `snapshot()` succeeds, **probe** it — `machine.branch(base, <minimal CorruptMemory
at relative offset 1>)`; the branch-time fault validation is side-effect-free on
rejection (`control.rs` doc), so `Ok` ⇒ arm-capable (the staged probe fault is
discarded by the campaign's first real branch), a `verb-not-supported` reject ⇒ NOT
arm-capable → `drop_snap` + nudge (`run(deadline vt+step)`) + re-snapshot, looping to
`snapshot_max_attempts`. Detection caveat: `ControlError::Unsupported` currently maps to
`MachineError::Transport("control error: verb not supported by this backend")` — either
string-match that message, or (cleaner) add a distinct `MachineError::Unsupported`
variant in `explorer::adapter::control_error_to_machine` and match it. **This is
untested — implement + validate on the box (rebuild, one calibration run fires +
certifies 25/25 at a small deadline_delta) before the ≥20-seed runs.**

### Fairness ruling (bug 2 answer): the 3-cell vocabulary IS a fair test
The guardrail was against silently faking the keyer, not against small vocabularies.
The real sensor making 3 cells from bug-agnostic operational logging is the honest
condition. **Do NOT enrich the logging to help the signal.** Record the cell count
prominently in `CORRELATION-REPORT.md` alongside the zero-cell scope statement, and
rule GO/NO-GO on the data — an honest NO-GO is a real result.

## Wall-time finding (feasibility) — use a SMALL deadline_delta
- With `--deadline-delta 2000000`, a non-triggering branch runs the full 2M V-time:
  ~8 branches took **>400 s** (≈35–50 s/branch incl. ~120 s boot). A ≥20-seed ×
  2-config × ~512-branch campaign at this rate is many hours.
- Drop `--deadline-delta` to ~**50k–200k** V-time (enough for the fault to land + the
  guard to fire, so a non-triggering branch stops quickly). Re-verify a find still
  fires + certifies 25/25 at the smaller bound.

## Remaining recipe (resumable — foreman or fresh session)
1. **Unblock calibration** (above): bug 1 fires + certifies 25/25 at a small
   deadline_delta, real gpa, arm-capable seal (or offset-0 fault).
2. **Bug 1 campaign**: `conductor bench-campaign --bug 1 --config signal|baseline
   --seed S --max-branches ~512 --deadline-delta <small> --calibration calibration.json
   --initramfs initramfs-campaign.cpio.gz --ready-marker CAMPAIGN_READY --out
   campaign-data/bug1/1-<config>-<S>.json`, ≥20 distinct seeds × both configs,
   3-wide (foreground, release inline). Collect JSONs + `FIND … state_hash` lines.
   **Determinism spot-check**: re-run ~3 seeds `--exclusive` (solo) and diff the JSON
   + state_hash vs the co-tenant run — a mismatch is a P0 leak → STOP + escalate.
   Commit + push (checkpoint 1).
3. **order/uuid**: add the same realistic bug-agnostic logging to `order-super.c` /
   `uuid-super.c`; write `build-order-image.sh`/`build-uuid-image.sh` +
   `order-init.sh`/`uuid-init.sh` (model on `build-campaign-image.sh`/`campaign-init.sh`,
   markers `ORDER_READY`/`UUID_READY`); build; calibrate each trigger; run + commit+push
   per `(bug × config)`.
4. **Report**: concat all `CampaignLog`s → `benchmark-report --logs all.json --out
   dissonance/benchmark/CORRELATION-REPORT.md`. **Record the zero-cell scope statement**
   (the log-template signal is inert on silent workloads; selectors must fall back to
   baseline on zero cells). Rule GO/NO-GO honestly — an honest NO-GO is a real result.
