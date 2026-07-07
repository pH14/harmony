# SCORING — the Scoring seam's interior

> **Status: RULED (Paul, 2026-07-07).** Companion to
> `docs/EXPLORATION.md` §"The Scoring seam": that doc rules the seam's *boundaries* (Sensor → Cell →
> Archive, the three signal tiers, coverage-is-terminal); this one rules its *interior* — what makes
> a state worth keeping (`CellFn`/`Archive`), what makes a kept state worth returning to
> (`Selector`/retention), and the playbook for iterating those answers when a gate says FAIL. Both
> imminent GO/NO-GO gates (task 69 M2, `tasks/69-signal-bug-correlation.md` + issue #66; task 84,
> `tasks/84-exploration-gate.md`) route FAIL to "fix the cell function, not the search" — this is
> the missing spec for *how*. Grounded in four primary-source research reports (§Sources); every
> borrowed term follows GLOSSARY citation discipline. The GLOSSARY scoring addendum rides this PR.

## Why this is one ruling, not two

The descriptor question ("which guest states count as the same, and which are worth keeping?") and
the economics question ("which kept state do we spend the next rollout on, and which seals earn
their keep?") meet at exactly one struct:

```rust
// dissonance/explorer/src/spine.rs:316
pub struct Reward { pub new_cells: u64 }
```

`Archive::admit(...) -> Reward` (spine.rs:557) scores what a run was worth; `Selector::reward()`
(spine.rs:590) feeds it back. That scalar is the **entire** feedback vocabulary between the two
halves. A descriptor spec and an economics spec written separately would contradict each other
precisely here — at what `admit` returns and what `reward` receives — so this document rules both,
and the `Reward` widening (R4) is its load-bearing ruling.

Throughout, **entry** means a frontier entry (today `FrontierEntry`/`VirtualExemplar`, `Entry` on
the GLOSSARY rename slate) and **`EntryRef`** its stable id (today `ExemplarRef`).

## What the verified literature settles

Six laws, each verified against primary sources (full extracts with per-claim citations and
UNVERIFIED flags in §Sources):

1. **Sensitivity is a partial order, and over-sensitivity starves scheduling** (Wang et al.,
   RAID 2019). "More sensitive" (preserves more mutation chains) is a strict partial order with
   incomparable pairs and **no dominant metric**; the two most sensitive metrics tested finish
   *below* baseline because promotion explodes — 706 seeds (baseline) vs **75,323** (memory-access)
   on the same target, starving every seed of attention. Too fine explodes, too coarse hides — now
   with numbers. `CellFnV1`'s cardinality knobs (`fold_k`, `Quant`, per-channel enable —
   `dissonance/logtmpl/src/cell.rs`) are the right dial; what was missing is the procedure for
   turning it.

2. **Descriptor drift is answered by re-key-and-rebuild** — two lineages converged independently.
   AURORA (Cully 2019; Grillotti & Cully 2022) keeps raw sensory data and, on every descriptor
   change, re-assigns descriptors to the whole repertoire and rebuilds the container, re-running the
   keep-best competition. Go-Explore (Nature 2021) never discards its archive on re-tune — it
   re-keys every stored cell's raw frame under the new cell function and lets the standard
   acceptance rule arbitrate collisions. Harmony holds the **strong form**: AURORA re-encodes
   through a noisy moving encoder, Go-Explore re-downscales frames; harmony re-keys by replaying
   retained `RunTrace`s through a pure `CellFn` fold — **exact, offline, repeatable at will**.

3. **Archive size is a controlled quantity.** AURORA-CSC holds archive size at a target with a
   proportional controller on the inclusion threshold (dimensionality-robust where its
   volume-based variant is not); Go-Explore's re-tune objective is explicit — maximize normalized
   entropy of samples-over-cells, penalized by deviation from a target cell count,
   `O = H_n(p)/√(|n/T−1|+1)`. Sober caveat, twice replicated: **fixed cell parameters beat the
   adaptive tuner** on Go-Explore's own headline domain. Auto-tuning proposes; a human ratifies.

4. **Cost never enters choice.** Legion (ASE 2020) explicitly declines to put solve-cost in its UCT
   score — the expensive operation fires *lazily, when selection has already said the node is
   worth it*. Agamotto (USENIX Sec 2020) has **no benefit formula at all** — its checkpoint economics
   are structural (prefix tree, depth-doubling placement interval, Non-Active → Last-Level → LRU
   eviction). The only cost term in any published scoring loop — EcoFuzz's `average_cost` — enters
   **energy assignment** (how much effort the chosen arm gets), never **arm choice**.

5. **Admission fine, scheduling coarse, arbitrated by a bandit** (AFL-HIER, NDSS 2021). Keep the
   finest signal for admission so no waypoint is lost; cluster on coarse descriptors and schedule
   the clusters with hierarchical UCB1 — ~77 node examinations per decision instead of 2,608 flat
   seed comparisons. The `Frontier` (coarse cells over fine entries) already *is* this shape.

6. **A descriptor may not be judged on discovery curves alone** (Böhme, Szekeres & Metzman,
   ICSE 2022). Coverage↔bugs correlate strongly *on average* (Spearman ρ ≈ 0.91) but coverage-based
   *rankings of configurations* agree with bug-based rankings only moderately (ρ ≈ 0.38–0.50;
   10–20% of programs disagree). Any gate that ranks cell functions must report a **bug-based**
   metric.

## The rulings

### R1 — the re-key contract: a `CellFn` change never invalidates a campaign

The archive is a **derived structure**. Because `RunTrace`s are retained and `Archive::admit` is a
pure fold over `(trace, forks, cells, sensors)`, changing the cell function means: re-key every
retained timeline through the new `CellFn`, rebuild the archive by re-running admission with
best-per-cell domination. The archive **shrinks, then regrows** — expected behavior in both source
lineages, not a regression. Harmony's re-key is exact (law 2), so re-keys are cheap enough to run
between campaigns or mid-campaign at a re-key epoch (R2).

Two preconditions are pinned as part of this ruling, not assumed:

- **Genesis-complete folding.** A suffix-only `RunTrace` cannot see an ancestor's features, so a
  re-key over a forked timeline must fold along the full genesis-rooted history (the cross-fork gap
  first surfaced by the differential-dataflow oracle investigation). This doc rules the *contract* —
  re-key semantics are genesis-rooted; whether that is implemented by storing genesis-complete
  traces or by folding down the parent chain is an implementation choice for the task that builds
  it, but the observable result must be identical.
- **Trace retention is the substrate.** Re-keyability requires the traces to exist.
  `docs/EXPLORATION.md` §Navigation rules *seal* retention; nothing yet rules *trace* retention, and
  for a k3s guest with real log volume traces are not free. Ruling: **retained traces live for the
  campaign's lifetime** — they are the re-key substrate and the descriptor-evaluation set (R2, the
  playbook). If trace storage must be bounded, its GC joins the R6 economics explicitly; silent
  trace GC breaks this contract.

### R2 — CellFn v2: keep the composition, make granularity principled

`CellFnV1`'s composition stands: species-progress ⊕ last-new-species ⊕ per-channel reified state,
coverage excluded by construction (the EXPLORATION ruling — coverage is terminal, never blended
into along-timeline keys). Three additions:

- **Granularity is controlled epoch-wise, never online.** AURORA-CSC adjusts a smooth, continuous
  threshold; `fold_k` is a discrete modulus whose every change re-keys all cell identities globally,
  and archive size is not smooth in it. The controller therefore runs in **re-key epochs**: measure
  archive size against target → adjust a knob (`fold_k`, `Quant`, channel set) → re-key (R1) →
  re-measure. Same negative-feedback pattern, discretized. Candidate knob-sets may also be ranked
  offline against retained traces by Go-Explore's entropy objective `H_n(p)/√(|n/T−1|+1)`.
  **Auto-tuning only ever proposes a config; a human ratifies it** — fixed beat adaptive on the
  source lineage's own benchmark, twice (law 3).
- **The v1 soft spots, answered.** The `mod fold_k` fold aliases unrelated states arbitrarily → the
  epoch controller makes `fold_k` a derived quantity with a stated target, not a magic constant
  (today's `DEFAULT_FOLD_K = 64` becomes the controller's starting point). The `last-new-species`
  channel depends on discovery order → deterministic given the campaign seed, and the task-69 M2
  ruling is pinned here: per-seed codebooks are independent; cell keys are **never compared across
  seeds**.
- **The empty `cell_channels` default is a ruling, not an accident.** IJON's verified lesson:
  sparse, chosen state annotations beat indiscriminate state feedback ("all those tools use
  additional feedback indiscriminately… limited to low information gain feedback"). A campaign
  wires in the few state channels it means; the default stays empty.

### R3 — quality is a domination preference, never a key dimension

The "prefer more missiles" question (logged, not ruled, at R-L2) is ruled: an orthogonal quality
objective rides as a **secondary integer key carried per entry**, resolved at admission by per-cell
domination — `Frontier::occupy` (spine.rs:498), the unconditional-replace primitive, is its hook;
`claim` (spine.rs:485) remains first-wins for novelty. **Never fold the objective into the cell
key**: every added key dimension multiplies cells and divides per-cell search energy (Antithesis's
Metroid discipline; MOME's per-cell Pareto front is the escalation path if two objectives ever
genuinely trade off, not the v2 default). Best-per-cell domination itself stays mandatory from day
one, per EXPLORATION's standing ruling.

### R4 — the `Reward` widening (the seam itself)

`Reward` widens **additively** (task 70's spec anticipates this) to a fixed vector of meaning-blind
integer channels — exactly two:

- `new_cells` — novelty (unchanged);
- `quality` — the R3 domination magnitude, so selection can prefer depth of progress, not just
  breadth.

**Cost stays out of `Reward` — ruled, not deferred.** The temptation is real (materialization cost
— replay depth from the nearest retained ancestor — is a genuine cost the Selector cannot see), but
law 4 is unanimous: no published scoring loop puts cost in choice, and the spine's own docstring
has it right — `Reward` is "what a run's admission was worth," and cost is not worth. Feeding
reward-minus-cost to a bandit conflates value estimation with budgeting. Cost is handled in R5
(energy, sealing) and R6 (retention bounds it structurally); the engine derives replay depth from
the entry's parent chain and never surfaces it as reward.

Invariant 5 survives intact: the Selector stays cell-meaning-blind — `Reward` carries meaning-blind
integer magnitudes (conventions rule 4), and the **scalarization policy lives in the Selector**,
which is where the economics belong.

### R5 — Selector economics (grounds task 70)

- **v2 (count-based):** energy-weighted choice over the frontier — AFLFast-FAST shape
  (`2^{s}/f`: chosen-count in the exponent, visit-frequency in the denominator) with **Entropic's
  Laplace add-one smoothing** so a zero-visit entry gets high energy, not zero. The cold-start rule
  is the single most important detail in a snapshot-branching setting: a freshly admitted entry has
  no observations and must not be starved by its own newness.
- **v3 (bandit):** AFL-HIER's hierarchical UCB1 over the frontier's coarse-cell/fine-entry
  structure (law 5), with the STADS stop (R7) as the exhaustion signal — task 70's
  "bandit + STADS stop" as specced, now with its citations pinned.
- **Cost decomposition: choice is cost-blind; energy and sealing are cost-aware.** `choose()`
  (spine.rs:586) never sees cost (R4). Cost enters afterwards, twice: the engine may modulate
  **energy** — how many rollouts the chosen entry receives before reselection — by materialization
  cost (EcoFuzz's decomposition), and **sealing** follows Legion's lazy pattern: pay for the
  expensive operation only once selection has already said the state is worth it.
- **Replacement statistics (pins v3's bookkeeping):** when `occupy` replaces a cell's occupant
  (R3), **reset the cell's chosen/reward statistics; keep its visit counters** — Go-Explore's
  verified rule (a better entry deserves fresh scheduling attention; visit history still describes
  the cell). The spine half-enforces this: replacement mints a fresh `EntryRef` (never reused), so
  per-entry state resets naturally; this ruling covers any per-*cell* state the bandit keeps.

### R6 — retention economics (the seal pool)

Retention is a **pure cost knob, never a correctness concern** (EXPLORATION §Navigation: eviction
is always reproducibility-safe). Ruling: adopt Agamotto's structure over the task-68
`SealBudget`/pool — ancestor-chain prefix structure, restore from the nearest retained ancestor,
depth-scaled retention preference, and its eviction pipeline (never evict an active entry's
ancestor → prefer deepest → LRU). For *which* seals earn their keep beyond structure, the only
published cost-benefit math is Snappy's empirical amortization predictor (median time saved ÷ seal
cost) — start there. Young–Daly optimal-interval theory (`τ_opt = √(2δM)`) has **never been ported
to fuzzing** — flagged as a genuine open opportunity, with its memoryless-failure assumption stated
as the port's first checkpoint, not adopted here. Pool GC of traces, if ever needed, joins this
ruling per R1.

### R7 — the stopping rule

STADS Good-Turing (`explorer/src/stads.rs`, already merged): `Û(n) = f₁/n` — singleton cells over
total rollouts — bounds the probability the next rollout discovers a new cell. Two uses ruled: the
campaign-level abort trigger, and the per-subtree exhaustion signal feeding the v3 bandit (a mined-
out subtree's residual discovery probability tells the Selector to stop expanding it).

## The E-fails playbook (what a gate FAIL triggers)

When task 69 M2 or task 84 fails — signal doesn't correlate with bugs, archive explodes or
collapses — the response is a procedure, not a judgment call:

1. **Do not touch the search.** The cell function is the suspect (EXPLORATION's two-hard-problems
   discipline). Freeze the campaign; its retained traces are now the evaluation set.
2. **Re-key candidate configs offline** (R1): replay `CellFn` v2 candidates (knob-sets, channel
   sets) over the retained traces — exact, no re-execution.
3. **Score every candidate on three axes:**
   - *(a) breadth* — cells discovered over the fixed trace set, normalized by total cell count so
     resolutions compare fairly (raw QD-style scores scale with cell count);
   - *(b) granularity* — the entropy objective: are traces well-distributed over cells, near the
     target count?
   - *(c) bug preservation* — **mandatory, per law 6**: re-run the admission fold in recorded
     campaign order under the candidate and check that **every ancestor of the bug-finding run
     still claims a cell when it arrives**. A candidate that would have judged any link of the
     chain uninteresting would have lost the bug (RAID'19's chain-preservation, computable exactly
     over retained traces). Discovery curves alone are disqualified.
4. **Ratify and resume:** human picks from the ranked candidates (R2), the live archive re-keys
   (R1), the campaign resumes. Shrink-then-regrow is expected.
5. **State the limit** (EXPLORATION's "diagnostic, not predictive," operationalized): offline
   re-keying proves a candidate *would have* distinguished the *recorded* states; it cannot prove
   the candidate will surface *unrecorded* ones — admission order also shifts which runs would have
   existed (the counterfactual cascade, which step 3c inherits). The playbook is a cheap filter
   that kills bad cell functions, not an oracle that crowns the best one.

## Scope fences

- **Automatic search over feedback functions** — running the playbook's steps 2–4 under a bandit,
  unattended — is genuinely unclaimed in the fuzzing literature (verified sweep: runtime selection
  over fixed pools exists; synthesis/search does not) and is uniquely enabled by exact re-keying.
  It is **not ruled here**: it is the "agent re-instruments the search between epochs" seat that
  `docs/RESOLUTION.md` already reserves. Named so nobody mints a lesser version by accident.
- **The `Portfolio` arm-chooser** (GLOSSARY Reserved) is not the R5 Selector; tactic-arm scheduling
  stays in tasks 70/72 vocabulary.
- **Oracle economics** (what to judge, Elle wrapping) are task-75-resurrection territory, not
  scoring.

## Vocabulary minted here (mirrored in `docs/GLOSSARY.md`, same PR)

- **re-key** *(verb)* — recompute every retained timeline's cells under a changed `CellFn`, then
  rebuild the archive by re-running admission (AURORA's container rebuild / Go-Explore's archive
  conversion; harmony's is exact and offline).
- **re-key epoch** — the interval between re-keys; the cadence of the R2 granularity controller.
- **energy** — how many rollouts a chosen entry receives before the Selector chooses again
  (AFLFast's power-schedule term, used for AFLFast's mechanism).
- **`quality`** *(reserved)* — the second `Reward` channel (R4): the R3 domination magnitude.

## Sources

Primary-source research reports (per-claim citations, verbatim formulas, UNVERIFIED flags) were
produced 2026-07-06 and are archived in the session records; the load-bearing primaries: Wang,
Duan, Song, Yin, Song (RAID 2019); Wang, Song, Yin — AFL-HIER (NDSS 2021); Ba, Böhme, Mirzamomen,
Roychoudhury — SGFuzz (USENIX Sec 2022); Aschermann et al. — IJON (S&P 2020); Böhme, Szekeres,
Metzman (ICSE 2022); Ecoffet et al. — Go-Explore (arXiv:1901.10995; Nature 590, 2021); Cully —
AURORA (GECCO 2019); Grillotti & Cully (IEEE TEC 2022); Vassiliades et al. — CVT-MAP-Elites (IEEE
TEC 2018); Pierrot et al. — MOME (GECCO 2022); Böhme, Pham, Roychoudhury — AFLFast (CCS 2016); Yue
et al. — EcoFuzz (USENIX Sec 2020); Böhme, Manès, Cha — Entropic (FSE 2020); Liu et al. — Legion
(ASE 2020); Song et al. — Agamotto (USENIX Sec 2020); Böhme — STADS (TOSEM 2018); Daly (FGCS 2006);
Antithesis engineering posts (Zelda, Metroid, Castlevania, Gradius, deterministic-hypervisor).

Corrections logged against common misreadings, so this doc never inherits them: AFLFast-FAST's
coefficient is `α/β`, not `μ` (μ is only the COE cutoff); Entropic smooths with Laplace add-one,
not Good-Turing (Good-Turing is STADS); Böhme ICSE'22's statistic is Spearman ρ, not Kendall τ;
Agamotto has no benefit formula; Young–Daly appears nowhere in fuzzing; the Antithesis
"sdk_best_practices" URL does not exist (the SDK-guidance posts are Zelda/Metroid/Castlevania).
