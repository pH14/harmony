# CORRELATION-REPORT — GO/NO-GO #2 (the Phase-F gate)

**Verdict: NO-GO** (amended directional Gate 4, integrator ruling `fa9d323`). The Phase-D
signal configuration does **not** beat the blind baseline on the sole real discriminator
(bug 3): it finds the planted bug **less often** and, censored at budget, **slower**. Phase F
(task 70) does **not** dispatch on this evidence; the fix is the cell function / selector
(iterate task 67), re-keyed offline against the retained traces (`docs/SCORING.md` E-fails
playbook). **The search is still not the fix.**

> Signal seeds: **20** · Baseline seeds: **20** · Measure-1 budget: **256 branches** ·
> Effect-size floor: **ρ ≤ −3/10** · Stopping ε: **1/1000** · Deadline: 50 000 V-time ·
> Replay bar: 25/25 · Cells: real task-67 `LogSensor` + `CellFnV1`.

## Scope — the M2 amendment (Paul, `fa9d323`)

This gate rules on **bugs 1 and 3** (≥20 seeds/config each). **Bug 2 (ordering-interrupt) is
documented below as found-but-degenerate/deferred** — its interrupt-counter observable is
structurally uncalibratable on this box (three confirmed findings), so perfecting a second
*synthetic* discriminator was ruled not worth the instrument cost; the ecologically valid
second discriminator is the held-out real workload (task 86). Gate 4 is **directional**, not
the binary "≥2 of 3": bug-3 clearly positive (right direction, meaningful effect) **and no bug
inverted** → provisional GO; bug-3 flat or inverted → NO-GO.

## Per-bug measures

| Bug | Found (sig / base of 20) | 1: novelty↔progress ρ (n) | 2: trajectory (on-path vs chance) | 4: median TTB — finders only (sig / base) | 4: median TTB — **censored @512** (sig / base) |
|---|---|---|---|---|---|
| **1** fault-timing-crash | 20 / 20 | — (degenerate) | 8/27 vs 21/10240 → above | 5.0 (IQR 8.5) / 2.0 (IQR 2.0) | 5.0 / 2.0 |
| **2** ordering-interrupt | — / — (deferred) | — | — | — | — |
| **3** rare-entropy-prefix | **11 / 18** | **−0.671** (n=11, < Klees floor 20) | 15/15 vs 31/10240 → above | 159.0 (IQR 219.0) / 190.0 (IQR 197.0) | **299.0 / 225.0** |

### Bug 3 — the discriminator, read honestly

The signal's **finders-only** median (159) looks better than baseline's (190), but that
compares *different, non-comparable seed subsets*: the signal only found the **11 easiest**
seeds; baseline additionally found 8 harder ones. Corrected for that:

- **Find rate:** signal **11/20**, baseline **18/20**. At the designed ~1/256 rate over 512
  branches P(find) ≈ 0.86, so baseline (0.90) is on-model; the signal (0.55) is far below it.
- **Censored median TTB (non-finds = the 512 budget):** signal **299**, baseline **225** —
  **the signal is worse.**
- **Common seeds (both found, n=10):** a **5–5 wash** (signal faster on 6, 8, 15, 16, 17;
  baseline faster on 1, 2, 9, 10, 18) — no signal advantage where both succeed.
- **Seeds baseline found but the signal missed:** 4, 5, 7, 12, 13, 14, 19, 20 (**8**). Seeds
  the signal found but baseline missed: 11 (**1**).

**Why the signal is worse here (mechanism):** bug 3 is a *single-dimension rare-value* trigger
(a seeded-entropy prefix — task-42 pattern) with **no locality**: jittering a near-miss draw
does not move it toward the target prefix. The signal config exploits a frontier exemplar on
~¾ of branches (`explore_period = 4`), and for bug 3 those exploits are a bug-agnostic one-
dimension seed jitter (see "The exploit kernel"), which is **unproductive** — so the signal
effectively explores only ~1/4 of its budget of fresh draws and finds the rare draw far less
often. Baseline explores a fresh draw every branch and covers more of the seed space.

The within-signal correlation **ρ = −0.671** (more cells discovered → shorter TTB, the *right*
direction) is a genuine positive nuance — the log-template novelty *does* track this bug's
progress among the runs that find it — but it is (a) computed on only **11 finders (< the Klees
≥20-trial floor)**, so underpowered; and (b) **degenerate**: `cells@256` takes only *two* values
(3 or 4), and the negative ρ is produced entirely by the two 3-cell seeds (10, 15) happening to
be the two slowest, not by a graded novelty↔progress relationship. It is reproducible from the
committed per-seed JSONs — derivation + series + a reproduce script in
`campaign-data/bug3/results/measure1-signal-derivation.md` (ρ = −0.6708). It does **not** make
the signal *beat* baseline.

### Bug 1 — degenerate, not a discriminator (as designed)

Bug 1 (task-60's fault-timing crash, reused verbatim) fires on **any** canary bit-flip at the
ledger gpa — the guest checks the canary every loop iteration, so there is no timing window:
naïve TTB ≈ 2–5. Both configs find it near-instantly (20/20 each). The signal median (5.0) is
slightly **worse** than baseline (2.0) — an artifact of `explore_period = 4` delaying the
signal's first fresh draw on a bug that baseline hits at branch ~2. This is a degenerate-bug
artifact, not a meaningful inversion, but it is not evidence *for* the signal either.

## Measure 3 — STADS species instrumentation

Species = distinct cells; samples = branches; pooled over all 20 seeds per config (20 480
branches each).

| | Signal | Baseline |
|---|---|---|
| Observed species S_obs | **4** | **4** |
| Singletons f1 / doubletons f2 | 0 / 0 | 0 / 0 |
| Good–Turing discovery prob. | 0.00000 | 0.00000 |
| Chao1 richness (est. remaining) | 4 (≈0) | 4 (≈0) |
| Stopping rule (discovery < 1/1000) reached | sample **2** | sample **2** |

Both configurations exhaust discovery almost immediately (by the 2nd branch): the bug-agnostic
operational logging emits a **tiny, fixed template vocabulary** (lifecycle warmup/steady/drain,
backpressure, checkpoint → ~4 cells), so the species-accumulation curve is flat after the first
handful of branches and the Chao1 richness is fully observed. This is the crux of the NO-GO: a
signal whose species pool saturates at 4 cells within 2 branches **carries almost no search
information** to guide the remaining ~510 branches.

## The three-part M2 realization (how the numbers were produced)

1. **Terminal-agnostic, marker-based certification.** On this container kernel the three
   isa-debug-exit crash channels all fail, so each bug's real `Crash{Shutdown}` is a `reboot -f`
   → triple-fault **millions of V-time past the seal** (bug 1 ~5M, bug 3 ~9M). Requiring that
   terminal per find would make the ≥20-seed suite take weeks. A **find** is therefore the
   per-bug serial **marker** present (only the planted bug prints it — attribution) **+** the
   reproducer replaying the identical `(stop, state_hash, marker)` **25/25** (determinism),
   decoupled from *which* terminal, run at a small 50 000-V-time deadline (~fast branches).
2. **Per-bug large-deadline validity (Gate 2).** Separately, one large-deadline run per bug
   proves the marker branch reaches a **real** `Crash{Shutdown}`: bug 1 `Crash@463116585`
   (25/25); bug 3 `Crash@473510449` — "backend shutdown exit (triple fault …)" — at seal+~9M,
   certified 25/25 (state_hash `c62b0f69…`). So the marker-based finds correspond to real
   crashes.
3. **The exploit kernel.** Exploitation is a **bug-agnostic, seeded-deterministic, one-
   dimension-at-a-time** jitter of the parent's existing fault (in-surface in `benchcampaign.rs`,
   never the shared `environment` crate): a fault-bearing parent nudges timing / gpa / bit /
   vector by a small step (converges on *conjunctive* triggers by fixing one dimension while
   holding the others); a **fault-less** parent (bug 3's RareEntropy mints no fault) jitters its
   **seed**. This one-dim design is exactly why the signal underperforms on bug 3: a seed jitter
   is non-convergent on a locality-free rare-value trigger (see above). It would help a
   *conjunctive* bug — which is what bug-2's deferred successor would test.

## Bug 2 — found-but-degenerate / deferred (three box findings)

Bug 2's interrupt-counter observable (Paul's 2026-07-07 option-B) is **structurally
uncalibratable** on this box; details in `campaign-data/bug1/NOTES.md`, escalation `c71038a`:

1. **Injection latency → duty ≈ 1.** The injected interrupt is serviced a few thousand V-time
   after its armed Moment, so the detection window must exceed that latency to catch it between
   the two counter samples (WINDOW_SPIN=4096 fires 48/48; 256 fires 0/many). A window that large
   fills the whole loop iteration.
2. **Rarity is unreachable feasibly.** With duty ≈ 1, diluting P(fire) needs either faults
   scheduled *past* the deadline — rejected `run overshot staged Moment … schedule unsatisfiable`
   — or a huge per-iteration filler that starves the very operational logging the signal reads.
3. **The order-image build is not seal-reproducible:** identical source seals the base at
   458M/463M/473M across rebuilds and firing flips 48/48 → 0/48.

Per the amendment, bug 2 does not count toward Gate 2/3; its **rare-value-gate rework is a
conditional follow-up**, bought only if the bugs-1&3 evidence is ambiguous and a tiebreaker is
decision-relevant.

## Deliverables & determinism

- **Retained traces (SCORING R1/R2 substrate):** every branch's `RunTrace` is retained per
  campaign (`campaign-data/bug3/results/traces.tar.gz` → `b3-<config>-<seed>.traces.json`, each
  an ordered `(branch, RunTrace)` array; 26 MB raw → 936 KB) — the offline evaluation set a future
  `CellFn` candidate replays through its pure fold to re-key this campaign without re-running it.
  A first-class deliverable **regardless of the verdict**, and precisely the E-fails evaluation
  set. **Contents: 43 files** — the **40** campaign traces (`b3-{baseline,signal}-{1..20}`) plus
  **3 solo determinism-check traces** (`b3-baseline-{1,2,3}-solo`). An offline re-key **must
  exclude the 3 `-solo` files** — they are duplicate baseline runs for the co-tenant-vs-solo
  check, not additional seeds.
- **Determinism (co-tenant vs solo):** seed 1 `5281f249…` and seed 2 `38a6540c…` match exactly
  (co-tenant == solo). Seed 3's raw `determinism.log` line reads `P0-DIVERGENCE co=[] solo=[]`,
  but this is a **false positive of the label** — baseline seed 3 found nothing in *both* the
  co-tenant and the solo run (empty == empty is *agreement*, a non-event), consistent, not a leak.
  No real divergence. *(Orchestrator fixed post-hoc: `run-bug{1,2,3}-campaign.sh` phase 3 now
  prints `AGREE (…non-event…)` for empty==empty; `P0-DIVERGENCE` fires only on a genuine hash
  mismatch. The committed `determinism.log` predates the fix.)*
- **Cell counts (fairness):** ~4 cells for both bugs 1 and 3 under both configs — the small,
  fixed template vocabulary is the honest condition (the logging is bug-agnostic **by design**;
  it was *not* enriched to help the signal). **Zero-cell scope statement:** the log-template
  signal is inert on silent workloads; a selector must fall back to baseline on zero cells.
- **Known gap:** bug 1's campaign ran *before* the retention amendment, so its traces were not
  retained; bug 1 is degenerate, so its re-key value is low. Flagged to the foreman — re-run for
  completeness vs accept (recommend accept; bug 3 is the substrate that matters).

## The ruling

**NO-GO.** On the sole real discriminator (bug 3) the signal configuration is **worse** than
the blind baseline: it finds the planted rare-value bug in **11/20 vs 18/20** campaigns and its
budget-censored median time-to-bug is **worse (299 vs 225)**; on common seeds it is a wash. Bug 1
is degenerate (and if anything slightly inverted). The species pool saturates at 4 cells within 2
branches — the bug-agnostic log-template signal carries almost no discriminating information, and
its one-dimension exploit actively *hurts* on a locality-free trigger. Cell novelty does track
progress *among the runs that find bug 3* (ρ = −0.671), but on too few finders and without
beating baseline.

Per the amended directional Gate 4, bug 3 is not clearly positive (flat-to-inverted) →
**NO-GO**. The route is the `docs/SCORING.md` **E-fails playbook**: freeze this campaign; its
retained traces are the evaluation set; **iterate the cell function** (task 67 — a descriptor
that produces bug-relevant, non-saturating cells, so the selector exploits the *right* parents)
and, given the exploit's poor fit to rare-value triggers, re-examine the selector's
explore/exploit split for non-conjunctive bugs; re-key candidates offline and re-run this
harness. **Phase F (task 70) does not dispatch on this evidence.**

If a tiebreaker is wanted before committing to the CellFn iteration, the amendment's conditional
follow-up applies: bug 2's rare-value-gate successor is a *conjunctive* bug on which the one-
dimension exploit should converge — the case where the signal has its best chance to beat
baseline — and would sharpen an otherwise underpowered (n=11) discriminator.

## Addendum — the explore/exploit ablation (Paul-authorized; does NOT reopen the NO-GO)

To separate *"the cells are blind"* from *"the exploit budget is harmful on rare-value bugs,"* the
signal config was re-run on bug 3 with the exploit turned off: **20 seeds, signal config,
`explore_period = 1`** (every step explores fresh, none exploit), same seeds / budget (512) /
deadline (50 000) / calibration / image as the campaign. The knob is the **recorded** `--explore-period`
flag (`explore_period: 1` in every ablation CampaignLog — no ambient env). Data:
`campaign-data/bug3/ablation/results/` (+ `traces.tar.gz`).

| bug-3 signal config | Found | Censored median TTB (@512) |
|---|---|---|
| `explore_period = 1` (explore-only, this ablation) | **18/20** | **225** |
| baseline (blind seed search) | 18/20 | 225 |
| `explore_period = 4` (the shipped campaign, ¾ exploit) | 11/20 | 299 |

**The ablation is byte-identical to baseline — seed-for-seed.** Every seed's time-to-bug matches
baseline exactly (the missed seeds are the same, {3, 11}); the co-tenant vs solo determinism check
matches baseline's hashes (seed 1 `5281f249…`, seed 2 `38a6540c…`). This is expected *by
construction*: at `explore_period = 1` the signal config never exploits, so it draws the **identical
PRNG stream** as baseline and mints the same fresh draws — the log-template sensor still runs and
records cells (4/campaign), but nothing uses them to redirect the search.

**Conclusion — it is the exploit, not the cells.** The campaign's drop from baseline's 18/20 to the
signal config's 11/20 (and 225 → 299 median) is caused **entirely by the exploit budget**: at
`explore_period = 4` the signal spends ~¾ of its branches jittering a frontier exemplar's fault, and
for bug 3 — a single-dimension, locality-free rare-value trigger — that jitter is non-convergent
(nudging a near-miss seed does not move it toward the target prefix), so those branches are wasted
and exploration is starved. What the ablation actually shows is narrower than "the cells are fine":
at `explore_period = 1` nothing is ever cell-guided (the sensor records cells but never steers the
search), so this run supports **no positive claim about cell-guided exploration**. It shows two
things only — the **sensor's presence is fully behavior-neutral** (running the log-template sensor
with no exploit reproduces baseline's find-rate exactly, 18/20), and the **exploit budget is the
entire deficit**. This **sharpens** the NO-GO — it does not reopen it: the signal configuration *as
shipped* (with exploitation) is still worse than baseline on the sole discriminator.

**Implication for the E-fails route.** The fix is not only a better `CellFn` (task 67 — cells that
actually track a bug's trigger) but also the **selector's explore/exploit policy for non-conjunctive
bugs**: exploitation only pays where the trigger has locality the one-dimension jitter can climb
(conjunctive fault-timing / interrupt-window bugs), and actively costs on rare-value bugs. A CellFn
whose cells correlated with trigger-proximity would let the selector exploit the *right* parents;
absent that, exploitation on a locality-free bug is pure budget waste. Re-key CellFn candidates
offline against the retained traces (this campaign's + this ablation's) and re-measure both the
correlation *and* the explore/exploit split before Phase F.
