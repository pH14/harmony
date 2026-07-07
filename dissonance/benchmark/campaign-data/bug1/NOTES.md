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

## ⛔ SUPERSEDED (2026-07-07) — the "verb not supported" blocker was STALE; the real
## blocker is crash-terminal FEASIBILITY. See "## 2026-07-07 GROUND TRUTH" below.
## Everything in this section is kept for history but is NOT the current blocker.

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

---

## 2026-07-07 GROUND TRUTH (fresh session; box-verified) — supersedes the seal-arm story

**The "verb not supported" blocker was STALE and mis-diagnosed.** Two facts, one from
static analysis (code at HEAD 136db19; no Rust changed since 040771d) and one from a
live box run, kill it:

1. **Static:** `can_arm_arrival()` is `vtime.is_some() && backend.deterministic_tsc` —
   a **static backend capability**, NOT a property of the seal point. Patched KVM sets
   `deterministic_tsc = true` (`vmm-backend/src/kvm.rs:728`), and V-time is always wired,
   so `can_arm_arrival()` is **unconditionally true** on the bench server. A plain minted
   `Recorded{Host(CorruptMemory)}` env (which is what bug 1's branch envs are — identical
   structure for offset-0 and offset-500, only the `Moment` key differs) passes **every**
   `Unsupported` gate in `control.rs`'s `restore`/`check_fault_admissible`. So the code
   **cannot** return `Unsupported` for bug 1's fault in an offset-dependent way. The
   "re-seal to an arm-capable boundary" fix targets a mechanism that does not exist.
2. **Live:** `bench-campaign --bug 1 --config baseline --seed 1 --deadline-delta 200000
   --calibration calibration.json` (window `[1500,1520]` → offset ~496 past the seal)
   runs **cleanly**: EXIT=0, all branches log, real LogSensor makes cells, KVM reverts to
   stock. **No "verb not supported".** The offset>0 fault stages and applies fine.

**THE REAL BLOCKER — crash-terminal feasibility (this is what actually gates M2):**
Per-branch `BENCH_DIAG=1` shows the bug DOES fire. Branches with the correct canary gpa
`0x7fbe2000` print the `CAMPAIGN_BUG` marker (`marker=true`) — the planted bug triggers.
But the crash terminal is the SLOW path: all three isa-debug-exit channels FAIL on this
kernel (no `CONFIG_X86_IOPL_IOPERM` / `CONFIG_DEVPORT` — documented in
`conductor/IMPLEMENTATION.md`), so the bug falls back to `_exit(0x60)` → `/init` `reboot -f`
→ triple-fault → `Crash{Shutdown}`. That reboot is at **seal + ~4.8M V-time**:
  - deadline 200000 → firing branch stops at **`Deadline`** (`judge=false`) BEFORE the
    crash → **NOT certified** (this is why the small-deadline plan / ruling (c) fails).
  - deadline 8_000_000 → firing branch reaches **`Crash{Shutdown}`** at vtime 463116585,
    `judge=true`, **CERTIFIES 25/25** (seed 1, branch 1, state_hash
    `bc3cde425cd3e74ff0310c7eb353d595b703a5a2a7dd7799366995e3480ecf9d`). **Gate 2
    (benchmark validity) for bug 1 is DONE** — the reproducer replays the identical crash.

**Box cost model (measured):** ~60k vns/sec; per-branch ≈ `1.7s + deadline/60_000`.
Non-firing branch runs the busy loop to the deadline; firing branch executes ~4.8M vns to
the reboot. So crash-terminal certification needs deadline ≥ ~5M → **~80–133 s/branch** →
a 120-campaign suite (≥20 seeds × 2 configs × 3 bugs) is **infeasible** (weeks).

**THE FIX (decision point — see issue #66 / commit): terminal-agnostic, marker-based
certification.** A find = the per-bug MARKER present + 25/25 replays reproducing the
identical `(stop, state_hash)` — decoupled from *which* terminal. Run the ≥20-seed
CORRELATION campaigns at a SMALL deadline (~2s/branch, feasible: ~6h for 120 campaigns
3-wide); the marker (at seal+~500) is captured well before the small deadline, and 25/25
determinism still holds at the `Deadline` stop. Gate-2 VALIDITY (a real `Crash`) is proven
separately per bug with ONE large-deadline run (bug 1 already ✅ above). Gate integrity is
preserved: the marker is per-bug and only the planted bug prints it (attribution), and
25/25 identity is unchanged (determinism). The M1 gate-integrity tests still hold
(unmarked crash → no marker → not a find; drifting hash → replays differ → not certified).

## 2026-07-07 FOREMAN RULING + VALIDATED + CAMPAIGN LAUNCHED (recycle handoff)

**Foreman ruling (on the flagged marker-based-cert decision):** PROVISIONALLY
APPROVED, 4 binding conditions: (1) per-find cert = marker + 25/25 identical
`(stop, state_hash, marker)` at the campaign deadline [✅ implemented exactly];
(2) per-bug VALIDITY cert MANDATORY — one large-deadline run proving the marker's
branch reaches a real `Crash`, for EACH bug before it enters the suite [bug 1 ✅
`Crash{Shutdown}`@463116585 / state_hash bc3cde42..., certified 25/25; bugs 2/3
PENDING]; (3) CORRELATION-REPORT.md must state the two-part realization
explicitly; (4) keep the gate-integrity tests [✅ kept + added
`marker_bearing_deadline_stop_is_a_find`].

**IMPLEMENTED (commit baa1fed):** terminal-agnostic marker-based certification in
`benchcampaign.rs` — find = `marker_attributed(&trace, spec)`; `certify_replays`
is marker- + deadline-aware (replays run at the campaign `until`, require
identical `(stop, state_hash)` + marker each), `verify_replays` removed. All 11
benchcampaign tests green.

**VALIDATED on box:** bug 1 certifies a find at deadline **50000** via the
marker path (seed 1 branch 1 → `Deadline`@458446116, marker=true, certified 25/25,
state_hash ffadc25d...). Box reverts to stock cleanly.

**CAMPAIGN PARAMETERS (chosen):** `--deadline-delta 50000 --max-branches 512
--replay-n 25`, 20 seeds (1..20) × 2 configs, 3-wide + 3 solo (baseline seeds
1..3) `--exclusive` determinism spot-checks. Bug 1 is EASY on the box (fires on
ANY canary bit-flip at gpa 0x7fbe2000 — the guest checks the canary every loop
iteration, so no timing-window constraint like the toy; P(fire)≈1/4 → found at
~4 branches). The hard, discriminating bugs are 2 (interrupt 1/256) and 3
(rare-entropy 1/256) — they need the 512 budget.

**ORCHESTRATOR:** `dissonance/benchmark/campaign-data/run-bug1-campaign.sh`
(committed). **Box-window concurrency lesson (2 failed launches):** NEVER
background `box-window.sh acquire` — concurrent first-acquires race the
window-open (`load_patched` ABORTs once patched is loaded → empty core). The
robust design: acquire 3 PERSISTENT leases SERIALLY up front, run 3 fixed-core
serial streams, release all 3 at the end, then solo `--exclusive` spot-checks.
Launched detached; results land in `~/t69m2-results/bug1/` on the box
(`progress.log`, `*.json`, `finds.log`, `determinism.log`).

**BOX COST MODEL (measured):** ~60k vns/sec; per-branch ≈ `3s overhead +
deadline/60k` (deadline 50000 → ~3.8s non-firing). Firing branches overshoot the
opportunistic deadline to ~seal+157k (the reboot's next exit) → ~5.6s; the
25-replay cert ≈ ~140s (once per campaign, at the first find). ~30-40 min/campaign
→ bug-1 suite (43 campaigns) ≈ ~8h at 3-wide.

**STATUS AT HANDOFF (2026-07-07 ~01:25):** bug-1 orchestrator LAUNCHED + confirmed
running 3-wide (box pid 994855, cores 2/1/3, 3 leases w1/w2/w3, first 3 campaigns
booting). ETA ~8h. WATCH-ITEMS for the monitor: (a) `kvm_intel users` was 9 early
(transient boot+restore overlap across 3 campaigns) — if it climbs unboundedly a
VM isn't being dropped (OOM risk) → investigate/kill+revert; (b) `progress.log`
`rc≠0` or `ACQUIRE-FAIL`/`FATAL` lines; (c) `determinism.log` `P0-DIVERGENCE`.

**REMAINING RECIPE (for the fresh session):**
1. Monitor `~/t69m2-results/bug1/progress.log` until `ORCH DONE`; check
   `determinism.log` for `P0-DIVERGENCE` (a solo≠co-tenant hash is a P0 STOP +
   escalate — never serialize to hide it). `scp` the `*.json` back, commit under
   `dissonance/benchmark/campaign-data/bug1/`. Box hygiene: if the orchestrator
   dies, `kill -9 -<pgid>`, `pgrep -x conductor | xargs -r kill -9`, then
   `rmmod kvm_intel kvm; modprobe kvm; modprobe kvm_intel` and verify 1396736 on
   a FRESH ssh (never `pkill -f` — self-matches the wrapper argv).
2. **Bugs 2 (order) & 3 (uuid):** their guest sources exist
   (`guest/linux/order-super.c`, `uuid-super.c`) but need the same realistic
   bug-agnostic operational logging campaign-super got, plus
   `build-order-image.sh`/`build-uuid-image.sh` + `order-init.sh`/`uuid-init.sh`
   (model on `build-campaign-image.sh`/`campaign-init.sh`; markers
   `ORDER_READY`/`UUID_READY`; both `crash_kind: Shutdown` — same reboot fallback,
   the channels fail the same way). Build; calibrate each trigger on the box
   (order: vector 0x81 + window offset; uuid: 8-bit prefix — the entropy draw is
   post-snapshot RDRAND, so it varies per branch); **gate-2 validity run each**
   (large deadline, confirm a real `Crash` + 25/25 — condition 2) BEFORE the
   suite; then run 20×2 campaigns each (clone the orchestrator, swap
   `--bug`/`--initramfs`/`--ready-marker`/`--calibration`).
3. **Report:** concat all `*.json` → `benchmark-report --logs all.json --out
   dissonance/benchmark/CORRELATION-REPORT.md`. STATE THE TWO-PART REALIZATION
   (condition 3): correlation runs = marker-based finds at a small deadline;
   validity = per-bug large-deadline `Crash` + 25/25. Record the cell count +
   zero-cell scope statement. Rule GO/NO-GO honestly (GO needs cell novelty
   correlating with bug progress on ≥2 of 3 bugs + signal median not worse than
   baseline on any bug; else NO-GO → iterate task-67 CellFn). Note bug 1's
   easy/degenerate TTB (~4, low variance) — it will show weak correlation; the
   ruling leans on bugs 2/3.

**Other confirmed facts:**
- Ledger gpa `0x7fbe2000` (canary) is correct for the logged image (boot prints
  `CAMPAIGN_LEDGER_GPA: canary=0x7fbe2000`). calibration.json `crash_kind: Shutdown` is
  right (the reboot fallback IS a Shutdown).
- The three issue-#66 P2s (#3 stads `Frac` overflow, #4 order-super torn window, #5
  ORDER_BUG crash_kind Shutdown) are **already folded in** (all cite "round-7 P2").
- `BENCH_DIAG=1` env-gated per-branch diagnostics added to `run_bench_campaign` (stderr
  only, never touches state/hash — a golden run is bit-identical). Keep it; it's how you
  watch a long campaign + calibrate a bug.

---

## 2026-07-07 P0 CAMPAIGN FINDING (fresh session, ~8 min into the launched run) — SIGNAL ARM ABORTS

**Symptom:** `b1-signal-1` finished `rc=1` after ~6.5 min (progress.log). Baseline
campaigns (seeds 1/2/3) run cleanly with the correct canary gpa `0x7fbe2000`. NOT a
determinism divergence — a signal-path abort. All 20 signal campaigns will die the same
way ⇒ **zero signal data ⇒ no correlation ⇒ no GO/NO-GO**.

**Root cause (fully traced, static):**
- `b1-signal-1.log` tail: `campaign failed (transport/backend): ... control error:
  perturb CorruptMemory gpa 0xad05141fa80d1582 + 8 is out of range (guest RAM is
  2147483648 bytes)`. `0xad05141fa80d1582` ≈ 1.2e19 — a raw `rng.next_u64()`.
- `environment/src/envcodec.rs:246` `host_fault_from` arm 2 mints `CorruptMemory { gpa:
  rng.next_u64(), mask: .. }` — a **uniformly-random 64-bit gpa, unclamped to guest RAM**.
- Signal exploit path `benchcampaign.rs:686` `codec.mutate(&parent.env, ..)` →
  `SpecEnvCodec::mutate` → `environment::EnvCodec::mutate`; op "insert" (~1/3 of exploits)
  ADDS a fresh `host_fault_from` fault. Its gpa is ~always out of the 2 GB range.
- Server rejects it via the **distinct, structured** `ControlError::PerturbOutOfRange`
  (control-proto/src/error.rs:127; task-59 "never mint a reproducer that doesn't reproduce").
- BUT `explorer/src/adapter.rs:668` `control_error_to_machine` collapses `PerturbOutOfRange`
  (and `Unsupported`) into an **opaque `MachineError::Transport(String)`** — type-
  indistinguishable from a real torn transport — so `benchcampaign.rs:696`
  `machine.branch(base, &env)?` treats a **recoverable proposal-rejection as a fatal machine
  death** and aborts the campaign.
- Baseline is IMMUNE: `mint_scenario_env` (benchcampaign.rs:165) only picks
  `one_of(&[gpa, gpa^0x1000, gpa+0x2000, 0x1000])` — all in-range near-canary.
- Same abort awaits insert-of-`SkewTime`/`SetClockRate` (rejected `Unsupported` on this
  backend — out-of-scope perturbs).

**Why "validated" launch still failed:** the box calibration found bug 1 at seed 1 **branch
1** — before the frontier populates, so NO exploit-mutate step ran. The exploit path was
never exercised in validation. The bug only fires once `!frontier.is_empty()` and an exploit
draws op=insert of a rejectable fault (≈1/4 per exploit).

**TWO distinct problems:**
- **(A) Robustness (unambiguous, in-surface):** an inadmissible/unsupported proposal must be
  a per-branch SKIP (empty cells, not a find, continue), NOT a fatal abort. Real explorer
  loops discard rejected proposals. Fix = distinguish `PerturbOutOfRange`/`Unsupported`
  (add a `MachineError::Inadmissible`-style variant + map it in `control_error_to_machine`)
  and skip in the campaign loop. Must NOT swallow genuine `Transport` (would mask real
  failures/determinism).
- **(B) Benchmark validity (semantics — foreman/Paul ruling):** even with (A), the signal
  EXPLOIT mutation draws a *uniform-random big* host fault (`host_fault_from`), so
  exploitation is mostly wasted/defanged (insert→~3/4 rejected+skipped; remove→drops the
  parent's good fault; only move preserves it). Exploitation ≠ exploitation. The benchmark
  would then measure the mutation operator, not cell-novelty — a confound that could produce
  a misleading NO-GO. Faithful fix = make the exploit a SMALL scenario-valid perturbation of
  the parent's fault (jitter timing / near gpa/bit), in-surface in benchcampaign.rs, NOT the
  shared `environment` crate. This is a non-trivial semantics change → per the ruling
  framework, checkpoint + escalate rather than build unilaterally.

**ACTION TAKEN:** campaign left running for now (baseline data valid; not a divergence).
Escalating to Paul: (1) stop the run now vs let baselines finish (recommend STOP — 0
completed, fix doesn't touch baseline code path so a clean unified relaunch is apples-to-
apples); (2) fix scope A vs A+B (recommend A+B + re-validate the EXPLOIT path specifically
before relaunch). Pending decision.

## 2026-07-07 RESOLVED — Paul: STOP + A+B; foreman ruling folded in; fixed + validated

**Decision (Paul):** stop the run now; fix scope **A+B** with exploit-path re-validation.
**Foreman ruling (issue #66):** (A) `MachineError::Inadmissible` mapped from
`PerturbOutOfRange`/`PerturbPastMoment`/`PerturbMomentTaken`/`Unsupported`, skip-not-abort,
never swallow genuine `Transport`; (B) exploit kernel BUG-AGNOSTIC (jitter parent's existing
fault only) + seeded-stream-deterministic + in benchcampaign.rs only + documented in the
report; re-validate the exploit path (populated frontier + exploit step) before relaunch.

**STOPPED cleanly (02:02):** `kill -9 -994855` (pgid, not a `-f` pattern), no conductor
stragglers, released w1/w2/w3 → `REVERT OK` stock `1396736`, archived partial results to
`~/t69m2-results/bug1-ABORTED-signalbug-0202`.

**FIX (commit 1bcfc6c):** exactly the A+B ruling. Exploit = `exploit_env`/`perturb_fault`
(one-dimension-at-a-time jitter so it converges on conjunctive triggers; fault-less parent
jitters its seed); dead `codec` param dropped. Local gates: 12 benchcampaign (+ new
`inadmissible_proposal_is_skipped_but_transport_still_aborts`) + 57 conductor lib + explorer
tests green; clippy + rustfmt clean.

**BOX VALIDATED (02:31, worktree checked out to 1bcfc6c + rebuilt — verifies cfg(linux)
boxrun.rs):**
- SIGNAL seed 1, 32 branches (the exact campaign that aborted before): **EXIT=0**, exploits
  concentrate near the canary (gpa 0x7fbe3000±, bits 29/30) exactly as fix B intends, **0
  aborts, 0 skips** (fix B keeps proposals in-range; fix A is defense-in-depth). No find in
  32 branches — the canary gpa 0x7fbe2000 was simply never proposed (unlucky ~10%; signal is
  degenerate on bug 1 as predicted — it concentrates on the near-miss 0x7fbe3000).
- BASELINE seed 1, 16 branches: **EXIT=0, 1 certified find**, `FIND bug 1 branch 1 state_hash
  ffadc25d6fe4aa46fea3c65ed43535c8f00c03164cafe073ad43cc901c2ac83c` — **BIT-IDENTICAL to the
  old-binary gate-2 hash** ⇒ the refactor changed nothing on the find/cert/determinism path
  and bug-1 gate-2 validity is re-confirmed on the new binary.
- Box reverted to stock cleanly after every run.
- **Invocation gotcha:** `--initramfs` takes the BARE name (`initramfs-campaign.cpio.gz`);
  `artifact()` (boxrun.rs) prepends `guest/build/`. Passing a full path double-prefixes →
  "guest image missing". Image + bzImage live in `~/harmony-t69m2/guest/build/`.

**NEXT:** unified clean relaunch of the bug-1 suite (both configs, same 1bcfc6c binary),
then prep bugs 2 & 3 in parallel, then report + GO/NO-GO. The signal<baseline-on-bug-1
expectation stands (degenerate bug); the discriminating evidence is bugs 2/3.

## 2026-07-07 02:51 — bug-1 suite RELAUNCHED (fixed binary 1bcfc6c), running 3-wide

Orchestrator `/root/run-bug1-campaign.sh` (md5 897121b) relaunched detached: **box pid
1702270** (reparented to init, survives disconnect), cores {2,1,3}, 3 leases, patched KVM
loaded (size 1400832). 20 seeds × 2 configs + 3 solo determinism spot-checks, deadline
50000 / maxb 512 / rn 25. Results → `~/t69m2-results/bug1/` (`progress.log`, `*.json`,
`finds.log`, `determinism.log`). ETA ~8h. **WATCH:** b1-signal-1 (the campaign that aborted
rc=1 pre-fix) MUST now run to completion — first live proof the fix holds under the real
suite. Same monitor watch-items as before (rc≠0 / ACQUIRE-FAIL / FATAL in progress.log;
P0-DIVERGENCE in determinism.log = STOP+escalate; kvm_intel unbounded refcnt growth).

**REMAINING WORK (fresh session can pick up — recipe in the "REMAINING RECIPE" section
above, items 2 & 3):**
1. MONITOR the bug-1 relaunch to `ORCH DONE`; verify determinism.log has no P0-DIVERGENCE;
   scp `*.json` back, commit under `campaign-data/bug1/`.
2. BUGS 2 (order) & 3 (uuid): add the same bug-agnostic operational logging to
   `guest/linux/order-super.c` / `uuid-super.c`; write build + init scripts (model on
   `build-campaign-image.sh`/`campaign-init.sh`; markers `ORDER_READY`/`UUID_READY`); build
   images; calibrate triggers on the box; **gate-2 validity run each** (large deadline → real
   `Crash` + 25/25) BEFORE the suite; then run 20×2 campaigns each (clone the orchestrator,
   swap `--bug`/`--initramfs`/`--ready-marker`/`--calibration`). Both bugs now benefit from
   the fixed exploit: bug 2 (timing window) is where signal SHOULD help most — the one-dim
   exploit kernel jitters timing/vector to converge on the window, so exploiting a near-miss
   parent is productive (unlike degenerate bug 1).
3. REPORT: `benchmark-report --logs all.json --out CORRELATION-REPORT.md`. STATE the two-part
   realization (marker-based small-deadline finds + per-bug large-deadline `Crash` validity)
   AND — per foreman condition — the exploit-operator description (bug-agnostic one-dimension
   seeded jitter of the parent's fault). Record cell counts + the zero-cell scope statement.
   Rule GO/NO-GO honestly (GO = cell-novelty correlates with bug progress on ≥2/3 bugs +
   signal median not worse than baseline on any bug; else NO-GO → iterate task-67 CellFn).

---

## 2026-07-07 (fresh session, BUGS 2 & 3 PHASE) — guest prep DONE + images BUILT; calibration gated

**CODE DONE + committed+pushed (fd6dc2f):**
- `order-super.c` / `uuid-super.c`: added the SAME bug-agnostic operational logging idiom
  `campaign-super.c` has (lifecycle phase / backpressure / checkpoint), driven by the NORMAL
  work counter (never the trigger). In order-super it sits at the loop BOTTOM, OUTSIDE the
  `[sw_before,sw_after]` ctxsw window (console writes are voluntary yields — `ru_nvcsw` — so they
  can't forge the involuntary `ru_nivcsw` the trigger keys on). `uuid-super` draws entropy ONCE
  post-READY (model match — loop adds NO RDRAND), crashes early on a prefix match (marker before
  the small deadline), else runs the operational loop to the deadline (feeds the signal on
  non-firing branches). Fairness: identical logging across bugs, NOT enriched per bug.
- `build-order-image.sh`/`build-uuid-image.sh` + `order-init.sh`/`uuid-init.sh`: verbatim clones
  of the bug-1 build/init (one determinism closure), swapping supervisor/init/ext4-UUID
  (deadbeef…62/…63)/output/markers (ORDER_READY/UUID_READY). **Init HARDENING:** the /init crash
  echo is marker-substring-free (`ORDER_ABORT_TERMINAL`/`UUID_ABORT_TERMINAL`, not `*_BUG_TERMINAL`)
  so `marker_attributed` is satisfied ONLY by the super's real marker (bug 1's echo carries its
  marker but is shielded by the small-deadline stop landing before /init runs — this is strictly
  safer).
- `manifest.rs` + `calibration.json`: bug-3 `crash_kind` Panic→Shutdown (real box terminal =
  deref→SIGSEGV→/init reboot→Crash{Shutdown}; manifest = attribution ground truth; round-7 P2
  principle already applied to bug 2). crash_kind is TOY-PATH ONLY (box cert is marker-based /
  terminal-agnostic), so NO box behaviour changes.
- Portable gates green: benchmark 29/29, conductor benchcampaign 12/12.

**IMAGES BUILT on the box (2026-07-07):** `~/harmony-t69m2/guest/build/initramfs-order.cpio.gz` +
`initramfs-uuid.cpio.gz` (~38M each), built with `taskset -c 4,12` (OFF the campaign cores).
order-super/uuid-super compiled clean. The 6 guest/linux files were scp'd to the box worktree
(overwriting the pre-logging order/uuid sources — campaign doesn't use them). Postgres debs +
busybox tarball already cached in `guest/dl/`.

**ORCHESTRATORS created (`campaign-data/run-bug2-campaign.sh`, `run-bug3-campaign.sh`):** clones of
run-bug1-campaign.sh swapping `--bug`/`--initramfs`/`--ready-marker`/OUT/names; DEADLINE=50000
MAXB=512 RN=25 (same defaults — confirm/adjust after calibration). They run 3-wide AFTER bug-1
frees {1,2,3}. CAL still points at `bug1/calibration.json` (holds all 3 bugs).

**CALIBRATION BLOCKER + FOREMAN RULING (2026-07-07):** box-window pool is exactly `CORES=(2 1 3)`
— all held by the bug-1 campaign (~15-18h). The classifier DENIED editing `/root/box-window.sh`
to add core 4 (shared-infra guardrail; box-window UNTOUCHED, verified). **Foreman ruled: modified
option 2 — calibrate on core 4 UNTRACKED (no box-window lease), foreman-coordinated:**
  1. ✅ Verified (BOX-PINNING.md): core 4 = threads {4,12}, sibling cpu12; campaign cores {1,2,3}
     = siblings {9,10,11}; cpu12 ∉ campaign siblings ⇒ core 4 is a distinct physical core, sibling
     not shared with the campaign. Pin calibration to `taskset -c 4` (leave cpu12 idle). cpu4/12
     carry only floating system daemons, no pinned determinism workload.
  2. ⏳ SEQUENCING: task-80 (agent-inspection-verbs) runs a SHORT core-4 live gate FIRST. **DO NOT
     start core-4 calibration until the foreman's next message confirms task-80's gate is done.**
  3. SAFETY (untracked ⇒ manual discipline): FOREGROUND runs; after EVERY run verify the VM exited
     + `kvm_intel` refcount returns to baseline (currently **9**); NEVER rmmod/insmod (use the
     already-loaded patched module — never transition it); never touch cores {1,2,3}.

**CALIBRATION APPROACH + RISKS (once core 4 is cleared):**
- **Bug 2 (order) — the risky one.** Mechanism: injected `InjectInterrupt{vector}` → guest-kernel
  RESCHEDULE → involuntary ctxsw inside order-super's non-atomic window → ORDER_BUG. UNVERIFIED
  whether vector 0x81 (or the mint's spread {0x81^0..15}) actually causes a reschedule on this
  kernel. FIRST run a WIDE-window diagnostic (`BENCH_DIAG=1`, widen the bug-2 window to ~[1004,1200]
  in a scratch calibration.json, ~64 branches, small replay-n) to confirm ANY vector/offset fires +
  marker attributes + certifies. If NONE fire → try a known reschedule vector (RESCHEDULE_VECTOR
  0xfd / LOCAL_TIMER 0xec) via the manifest vector; if still nothing → CHECKPOINT + ESCALATE (the
  InjectInterrupt→reschedule mechanism doesn't reach userspace preemption on this kernel — do NOT
  fake it). Once firing, narrow the window so naïve TTF ≈ 256 and confirm the marker still lands
  before deadline 50000.
- **Bug 3 (uuid) — simpler.** Run ~512 branches (`BENCH_DIAG=1`), confirm SOME branch fires
  (UUID_BUG) at ~1/256 + certifies 25/25 (RDRAND intercept is deterministic per seed ⇒ replays
  reproduce). Model agreement is a toy-path concern (already tested); the box only needs fire+cert.
  If RDRAND isn't intercepted (draw host-random / varies across replays) → cert fails → ESCALATE.
- **Gate-2 validity (MANDATORY, foreman condition 2):** each bug — ONE large-deadline run
  (~8_000_000) proving the marker branch reaches a REAL `Crash` + certifies 25/25, BEFORE its
  suite. bug 1 ✅ already; bugs 2/3 pending core-4 clearance.

**Condition-4 answer (why bug-1 8h→~15-18h):** the ~8h estimate used the SOLO per-branch cost
(~3.8s at deadline 50000). Under 3-wide co-tenancy, 3 KVM guests contend for the shared memory
controller / LLC on the single-socket i9-9900K, inflating per-branch WALL time — the cold first
wave ran ~83 min/campaign (~2.5×; 512 branches × ~9.7s), warmer later waves faster. Each campaign
runs the FULL 512-branch budget by design (species curve — does NOT stop at first find). Net ETA
~15-18h. Wall-clock only — determinism (state_hash) is co-tenancy-independent (V-time = retired
branches), which phase-2's solo-vs-co-tenant spot-check verifies.

---

## 2026-07-07 — CORE-4 CALIBRATION (foreman-cleared, untracked, `taskset -c 4`)

Preflight verified: kvm_intel=9, campaign on {1,2,3} intact, core 4 clear (task-80 gate done).
Every run below: foreground, kvm_intel back to **9** after (verified), no module transition.

**Bug 2 (order) — DOES NOT FIRE with the placeholder vector; testing real vectors before escalate.**
- Image boots + seals (base sealed deep in the loop at VTime ~458M) + branches run clean (rc=0).
- BENCH_DIAG confirms InjectInterrupt faults are minted+delivered: `Interrupt@<at=4..68> vec=0x81..0x8e`
  (the mint spread `{vector^0..15}`), `fault-rebase 1000` (⇒ the `at` search range [4,68) correctly
  overlaps the FIRST loop iterations past the seal — NOT a window/timing miss).
- **16 branches → 0 fires; 512 branches (suite scale) → 0 fires.** cells=1, records=1 (operational
  logging works; guest runs the loop, just never preempted).
- **Root cause (hypothesis, strong):** order-super detects the bug via `ru_nivcsw` (INVOLUNTARY
  context switches). On this guest — no clock-event device (pg-init's cooperative-wait design) +
  order-super is the ONLY runnable userspace task (postgres stopped, /init blocked in `wait()`) — an
  injected interrupt runs its IDT handler and returns to the SAME task; with nothing to switch to and
  no timer tick, `ru_nivcsw` stays permanently 0 ⇒ ORDER_BUG can't fire. COMPOUNDED: the mint
  vectors `{0x81^0..15}` are all arbitrary — none is a real reschedule/timer/IPI vector (0x81 was a
  placeholder "wired at bring-up").
- **BEFORE escalating (proper calibration):** try REAL reschedule/timer vectors via a scratch
  calibration.json — a timer interrupt (LOCAL_TIMER 0xec; spread covers 0xe0-0xef) can raise a
  softirq → ksoftirqd runnable → preempts order-super (involuntary ctxsw) EVEN single-task; then the
  reschedule/IPI range (manifest vector 0xf9 spreads to the full 0xf0-0xff incl. RESCHEDULE 0xfd /
  CALL_FUNCTION 0xfb). If a vector fires → calibration success (set that vector in calibration.json,
  narrow window to ~1/256, confirm marker < deadline 50000 + cert). If NEITHER range fires → ESCALATE:
  the mechanism truly can't produce an involuntary ctxsw on this single-task/no-timer guest. Fix
  options for the foreman/Paul then: **(A)** deterministic co-runner (order-init launches a 2nd busy
  userspace task so a reschedule actually deschedules order-super) + a real vector — faithful but the
  two-task scheduling must be proven deterministic under the harness (the 25/25-cert risk); **(B)** a
  single-task-observable realization (detect the injected interrupt landing in the window via a kernel
  interrupt counter rather than via preemption) — simpler, no scheduling-determinism risk, still a
  faithful "handler ran mid-update" ordering violation. Recommend B if a clean observable exists.
  Do NOT build either unilaterally — it's a benchmark-mechanism change.

### 2026-07-07 — BUG 3 (uuid) ALSO DOES NOT FIRE — root cause CONFIRMED (seal overshoots the draw)

Ran on core 4 (foreground, kvm→9 after each): 512 branches @8-bit → 0 fires; 16 branches with a
**rebuilt PREFIX_BITS=1** guest (should fire ~1/2) → 0 fires (P(0/16)=1.5e-5 ⇒ NOT bad luck);
4 branches with a **hardcoded matching draw** (`draw = 0xA5..00`, no RDRAND) → STILL 0 fires.
`conductor box --record` (8 seeds) → branches reach `Crash{Shutdown}` (reboot) with **≥2 distinct
state_hashes**, and the post-seal trace (`strings`) contains **ONLY reboot messages** — no
UUID_DRAW (despite UUID_DEBUG=1), no supervisor operational logs, no UUID_BUG.

**Confirmed root cause:** the base snapshot seals **PAST uuid-super's entire post-READY execution.**
uuid-super prints UUID_READY then *immediately* draws + decides + (on match) crashes — there is NO
long snapshottable window after the ready marker (unlike campaign-super/order-super, whose long
post-READY loops the seal lands *inside*). So `seal_base`'s snapshot-retry (advancing past
`NotQuiescent`) overshoots uuid-super entirely and seals in the reboot tail. Consequences: the
entropy draw is **baked into the base** (happened pre-seal), per-branch `reseed_entropy(seed)` never
re-runs it, and every branch inherits an already-rebooting base → reboot-only console, no marker.
The ≥2 distinct hashes come from the reseed perturbing entropy state, NOT from uuid behaving
differently. The absent UUID_DRAW/UUID_BUG in the *post-seal* trace is the direct proof the draw
happened *before* the seal.

Note bug 2 is a DIFFERENT root cause: order-super HAS a proper post-READY loop, so its seal lands
mid-loop and the fault is injected correctly (BENCH_DIAG shows `Interrupt@[4..68]` + operational
logs, records=1) — it just can't produce an involuntary ctxsw (no runnable alternative). The
bug-2 real-vector test was DEPRIORITIZED: no vector helps when there is nothing to switch to.

### 2026-07-07 — JOINT ESCALATION (checkpoint) — 2 of 3 benchmark bugs are UNREALIZED on the box

Bug 1 fires + certifies (the running suite proves it). **Bugs 2 AND 3 do not fire on the box** —
neither was ever box-validated in M1 (their gate-2 was always PENDING), and both M1 design
assumptions fail on THIS guest:
- **Bug 2 (order):** the injected-interrupt→INVOLUNTARY-ctxsw detection can't fire — single
  runnable userspace task (postgres stopped, /init blocked in wait) + no clock-event device ⇒ an
  injected interrupt returns to the same task, `ru_nivcsw` stays 0. 0/512.
- **Bug 3 (uuid):** the base seals PAST uuid-super's fast post-READY draw ⇒ draw baked, no
  per-branch variation, branches inherit a rebooting base. 0/512, 0/16(1-bit), 0/4(hardcoded-fire).

**Fix options for foreman/Paul (all are benchmark-mechanism changes — NOT built unilaterally):**
- Bug 2 — **(A)** deterministic co-runner (order-init launches a 2nd busy userspace task so a
  reschedule actually deschedules order-super) + a real reschedule/timer vector; determinism of
  two-task scheduling under the harness must be proven (the 25/25-cert risk). **(B)** detect the
  injected interrupt landing in the window via a kernel interrupt COUNTER (single-task-observable),
  not via preemption — simpler, no scheduling-determinism risk, still a faithful "handler ran
  mid-update" ordering violation. Lean B.
- Bug 3 — add a **snapshottable pre-draw window**: after UUID_READY, run a short bounded
  stabilization loop (like campaign-super's) BEFORE `draw_campaign_entropy()`, so the seal lands in
  that loop and the draw stays post-seal (per-branch). Small, contained image change; re-validate
  the draw varies per branch (distinct hashes AND a fire found). Most contained of all the fixes.

**Recommendation:** both fixes are worthwhile — bug 1 is degenerate/easy, so a meaningful GO/NO-GO
needs the discriminating bugs 2 & 3. Bug-3's fix is the most contained (a pre-draw loop); bug-2's
needs a mechanism decision (co-runner vs interrupt-counter). ~half a day of guest rework + box
re-calibration + gate-2 each. ALTERNATIVES: rule GO/NO-GO on bug-1-only (fails the spec's "≥3 bugs,
each found" gate — needs a waiver), or defer bugs 2/3 to a follow-up task. Escalated to Paul/foreman
for the call. Bug-1 suite untouched throughout (healthy, all rc=0, kvm_intel→9 after every core-4
run).
