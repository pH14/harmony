# REKEY-REPORT — offline `CellFn` iteration over the GO/NO-GO #2 trace corpus

> **The E-fails playbook, steps 2–4** (`docs/SCORING.md`). GO/NO-GO #2 closed **NO-GO**
> (`CORRELATION-REPORT.md`): the log-template signal is behaviour-neutral but nearly
> blind, and the ¾-exploit budget was the entire find-rate deficit. The playbook's
> response is a procedure — freeze the campaign, re-key candidates offline against its
> retained traces, score three axes, hand a human a ranked menu. This is that menu.
> **It does not re-open the NO-GO, and it promotes nothing:** R2 rules that fixed cell
> parameters beat the adaptive tuner on Go-Explore's own headline domain, twice —
> *auto-tuning proposes, a human ratifies.*

> Corpus manifest `campaign-data/rekey-corpus.json`, sha256 `36e954a23decf67b7918ee476af0ea6e3c9c07d067168594ce9c8b6e52fcafd1`.
> 60 trace files · 30720 recorded branches · 5 excluded · 40 reference logs.
> All arithmetic is integer/fixed-point; the report has no generated-date line, so two
> runs on any two hosts produce byte-identical bytes.

> **Regenerated after PR #94 round 1.** Axis (a) previously unioned cell keys across
> `(config, seed)` campaigns, which R2 forbids (per-seed codebooks are independent; cell
> keys are never compared across seeds). Breadth is now per-campaign, and the twin-control
> evidence moved with it: on the steered slice the trigger-*aligned* candidate now pools
> more cells, not fewer, and the trigger-blind twin's apparent advantage there is gone.
> **The top-three menu is unchanged** (`draw-top-64` → `v1-shipped` → `draw-top-256`), and
> so is every conclusion: on the unsteered ablation slice — the one slice where the
> comparison is clean — the twins remain indistinguishable on every axis.

## The finding, in one paragraph

The shipped `CellFn` v1 discovers **not one cell while the bug-3 search is still searching**,
summed over all 40 campaigns of the primary slice. Three species arrive on branch 0 (a
blank line, the supervisor's checkpoint message, and the `UUID_DRAW` line), and the
archive is then **frozen** — until the finding branch, where the kernel's general
protection fault message mints a fourth species. v1's entire novelty signal on this bug
is a *post-hoc crash artifact*: it arrives only after the bug has already been found.
That is the mechanical explanation of both the ρ = −0.671 the correlation report
computed and the frontier that saturates at two entries. **No setting of v1's knobs
changes this** — `fold_k`, quantization, and channel ablation can only coarsen a
three-species vocabulary, never enrich it. Only a *new channel* moves the needle, and
the report's second finding is that no offline axis can tell a good new channel from a
useless one.

## The corpus

Loaded **only** through the manifest, every artifact pinned by content hash (the `hm-xdp` lesson: reference artifacts by content, never by mutable path). A hash mismatch aborts the run; it is never a warning.

| slice | bug | campaigns | `explore_period` | what it is |
|---|---|---|---|---|
| `bug3-campaign` | 3 | 40 | 4 | GO/NO-GO #2 bug-3 campaign: 20 seeds x {baseline, signal}, explore_period = 4, 512 branches each |
| `bug3-ablation` | 3 | 20 | 1 | The Paul-authorized explore/exploit ablation: signal config at explore_period = 1 (never exploits), 20 seeds. The only slice showing what the sensor sees on an UNSTEERED search |
| `bug1-reference` | 1 | 40 | 4 | **recorded logs only — not re-keyable** |

### Exclusions

| slice | member | reason |
|---|---|---|
| `bug3-campaign` | `./b3-baseline-1-solo.traces.json` | solo determinism re-run: a replica of seed 1, not an additional seed. Double-counting it would bias every axis |
| `bug3-campaign` | `./b3-baseline-2-solo.traces.json` | solo determinism re-run: a replica of seed 2, not an additional seed. Double-counting it would bias every axis |
| `bug3-campaign` | `./b3-baseline-3-solo.traces.json` | solo determinism re-run: a replica of seed 3, not an additional seed. Double-counting it would bias every axis |
| `bug3-ablation` | `./b3-signal-ep1-1-solo.traces.json` | solo determinism re-run: a replica of seed 1, not an additional seed. Double-counting it would bias every axis |
| `bug3-ablation` | `./b3-signal-ep1-2-solo.traces.json` | solo determinism re-run: a replica of seed 2, not an additional seed. Double-counting it would bias every axis |

**5 excluded**, all `-solo` determinism re-runs. Each is pinned by sha256 in the manifest too: an exclusion names a *known* artifact, not merely an absent one.

### Bug 1 — a reference row, not an evaluation slice

Bug 1's campaign ran before the trace-retention amendment, so no RunTraces were retained and it CANNOT be re-keyed (docs/SCORING.md R1: retained traces are the substrate). Its recorded per-campaign cell counts appear in REKEY-REPORT.md as a reference row only. Bead hm-5sv.

Its recorded per-campaign distinct-cell counts, for reference: **2** over 20 signal campaigns and **2** over 20 baseline campaigns — a *two*-cell vocabulary, thinner even than bug 3's. Every campaign found the bug (it fires on any canary bit-flip), so it was never a discriminator. The trigger-orthogonal twin candidate (`draw-low-256`) replaces it as this report's noise-fitting control, per the tasks/97 amendment.

## The candidate space (R2's knob-sets — configs, not code)

Each candidate is a `logtmpl::CellConfig` recorded verbatim below, optionally composed with one **chosen sparse state channel** (IJON's discipline: sparse chosen state annotations beat indiscriminate state feedback; the empty `cell_channels` default is a ruling, not an accident). The corpus offers exactly one such observable — the `UUID_DRAW: draw=0x… prefix_bits=8` line the workload prints once per branch.

Corpus constants used by the key-space normalizer, derived from the observations rather than assumed: **max_species = 4**, **|top-byte alphabet| = 256**, **|low-byte alphabet| = 256**.

| candidate | state channel | `CellConfig` (verbatim) | `\|K\|` |
|---|---|---|---|
| `v1-shipped` | — | `{"template_channel":1,"species_progress":true,"species_quant":"Log2","last_new_species":true,"cell_channels":[],"fold_k":64}` | 12 |
| `foldk-16` | — | `{"template_channel":1,"species_progress":true,"species_quant":"Log2","last_new_species":true,"cell_channels":[],"fold_k":16}` | 12 |
| `foldk-32` | — | `{"template_channel":1,"species_progress":true,"species_quant":"Log2","last_new_species":true,"cell_channels":[],"fold_k":32}` | 12 |
| `foldk-128` | — | `{"template_channel":1,"species_progress":true,"species_quant":"Log2","last_new_species":true,"cell_channels":[],"fold_k":128}` | 12 |
| `foldk-256` | — | `{"template_channel":1,"species_progress":true,"species_quant":"Log2","last_new_species":true,"cell_channels":[],"fold_k":256}` | 12 |
| `quant-identity` | — | `{"template_channel":1,"species_progress":true,"species_quant":"Identity","last_new_species":true,"cell_channels":[],"fold_k":64}` | 16 |
| `species-only` | — | `{"template_channel":1,"species_progress":true,"species_quant":"Log2","last_new_species":false,"cell_channels":[],"fold_k":64}` | 3 |
| `lastnew-only` | — | `{"template_channel":1,"species_progress":false,"species_quant":"Log2","last_new_species":true,"cell_channels":[],"fold_k":64}` | 4 |
| `no-channels` | — | `{"template_channel":1,"species_progress":false,"species_quant":"Log2","last_new_species":false,"cell_channels":[],"fold_k":64}` | 1 |
| `draw-top-64` | draw >> 56 (trigger-aligned) | `{"template_channel":1,"species_progress":true,"species_quant":"Log2","last_new_species":true,"cell_channels":[2],"fold_k":64}` | 780 |
| `draw-top-256` | draw >> 56 (trigger-aligned) | `{"template_channel":1,"species_progress":true,"species_quant":"Log2","last_new_species":true,"cell_channels":[2],"fold_k":256}` | 3084 |
| `draw-top-only-256` | draw >> 56 (trigger-aligned) | `{"template_channel":1,"species_progress":false,"species_quant":"Log2","last_new_species":false,"cell_channels":[2],"fold_k":256}` | 257 |
| `draw-low-256` | draw & 0xFF (trigger-blind control) | `{"template_channel":1,"species_progress":true,"species_quant":"Log2","last_new_species":true,"cell_channels":[2],"fold_k":256}` | 3084 |

- `v1-shipped` — CellFn v1 exactly as the campaign ran it — the control
- `foldk-16` — fold_k = 16
- `foldk-32` — fold_k = 32
- `foldk-128` — fold_k = 128
- `foldk-256` — fold_k = 256
- `quant-identity` — species-progress quantized Identity (raw count) instead of Log2
- `species-only` — channel ablation: species-progress only (last-new-species off)
- `lastnew-only` — channel ablation: last-new-species only (species-progress off)
- `no-channels` — channel ablation: both template channels off — the one-cell floor
- `draw-top-64` — v1 + chosen state channel on the entropy draw's top byte, folded mod 64
- `draw-top-256` — v1 + chosen state channel on the entropy draw's top byte, unfolded (k = 256)
- `draw-top-only-256` — the chosen state channel alone (both template channels off), k = 256
- `draw-low-256` — TWIN CONTROL: identical to draw-top-256 but keyed on the trigger-blind low byte

## The three axes

- **(a) breadth** — cells discovered over the fixed trace set. Every campaign's archive is keyed in **its own namespace**: `docs/SCORING.md` R2 pins that per-seed codebooks are independent and *cell keys are never compared across seeds*, because a template species id is minted in per-campaign first-seen order, so the same key bytes name different behaviour in two campaigns. `total` therefore sums each campaign's distinct cells rather than unioning keys across seeds; `mean` is per campaign; `coverage` is `mean / |K|`, the QD coverage of one campaign's archive, normalized because raw QD-style scores scale with resolution and would crown the finest candidate by construction.
- **(b) granularity** — Go-Explore's re-tune objective `O = H_n(p)/√(|n/T−1|+1)`, per campaign, averaged. `p` is the arrival count per cell (the STADS abundance stream). The **stated target** is `T = 64` — a cell per ~8 branches of the 512-branch budget: fine enough that the frontier has somewhere to go, coarse enough that each cell still earns search energy. `O@256` re-scores at a second target so the ranking's dependence on `T` is visible rather than hidden.
- **(c) chain preservation** — mandatory, law 6. The admission fold is re-run in recorded campaign order under the candidate; every **proper ancestor** of every bug-finding run must still claim a cell when it arrives. A candidate that would have judged any link uninteresting would have lost the bug.

Diagnostics, clearly *not* a fourth axis: `admitted` is the mean frontier size a selector would have had to exploit; `cells>0` counts cells first claimed after branch 0; **`steering`** counts cells first claimed strictly between branch 0 and the find — the cells a search could actually have used; **`crash-only`** counts pooled cells never keyed before the guest crashed, which a search can never have used at all.

### `bug3-campaign` — 40 campaigns, 29 found the bug

| candidate | (a) total | (a) mean | (a) coverage | (b) O@64 | (b) O@256 | (c) chains | admitted | cells>0 | steering | crash-only |
|---|---:|---:|---:|---:|---:|---|---:|---:|---:|---:|
| `v1-shipped` | 149 | 3.725000 | 0.310417 | 0.529853 | 0.524069 | 29/29 chains, 4/4 ancestors | 1.725000 | 29 | 0 | 29 |
| `foldk-16` | 149 | 3.725000 | 0.310417 | 0.529853 | 0.524069 | 29/29 chains, 4/4 ancestors | 1.725000 | 29 | 0 | 29 |
| `foldk-32` | 149 | 3.725000 | 0.310417 | 0.529853 | 0.524069 | 29/29 chains, 4/4 ancestors | 1.725000 | 29 | 0 | 29 |
| `foldk-128` | 149 | 3.725000 | 0.310417 | 0.529853 | 0.524069 | 29/29 chains, 4/4 ancestors | 1.725000 | 29 | 0 | 29 |
| `foldk-256` | 149 | 3.725000 | 0.310417 | 0.529853 | 0.524069 | 29/29 chains, 4/4 ancestors | 1.725000 | 29 | 0 | 29 |
| `quant-identity` | 149 | 3.725000 | 0.232812 | 0.529853 | 0.524069 | 29/29 chains, 4/4 ancestors | 1.725000 | 29 | 0 | 29 |
| `species-only` | 109 | 2.725000 | 0.908333 | 0.381124 | 0.378162 | 29/29 chains, 4/4 ancestors | 1.725000 | 29 | 0 | 29 |
| `lastnew-only` | 149 | 3.725000 | 0.931250 | 0.529853 | 0.524069 | 29/29 chains, 4/4 ancestors | 1.725000 | 29 | 0 | 29 |
| `no-channels` | 40 | 1.000000 | 1.000000 | 0.000000 | 0.000000 | 29/29 chains, 4/4 ancestors | 1.000000 | 0 | 0 | 0 |
| `draw-top-64` | 2628 | 65.700000 | 0.084231 | 0.784615 | 0.605753 | 29/29 chains, 4/4 ancestors | 63.375000 | 2508 | 1331 | 29 |
| `draw-top-256` | 7513 | 187.825000 | 0.060903 | 0.446541 | 0.672845 | 29/29 chains, 4/4 ancestors | 185.100000 | 7393 | 2841 | 29 |
| `draw-top-only-256` | 7444 | 186.100000 | 0.724125 | 0.417338 | 0.624551 | 29/29 chains, 4/4 ancestors | 185.100000 | 7364 | 2841 | 0 |
| `draw-low-256` | 6778 | 169.450000 | 0.054945 | 0.438099 | 0.610901 | 29/29 chains, 4/4 ancestors | 166.625000 | 6658 | 2614 | 51 |

### `bug3-ablation` — 20 campaigns, 18 found the bug

| candidate | (a) total | (a) mean | (a) coverage | (b) O@64 | (b) O@256 | (c) chains | admitted | cells>0 | steering | crash-only |
|---|---:|---:|---:|---:|---:|---|---:|---:|---:|---:|
| `v1-shipped` | 78 | 3.900000 | 0.325000 | 0.508232 | 0.502380 | 18/18 (vacuous) | 1.900000 | 18 | 0 | 18 |
| `foldk-16` | 78 | 3.900000 | 0.325000 | 0.508232 | 0.502380 | 18/18 (vacuous) | 1.900000 | 18 | 0 | 18 |
| `foldk-32` | 78 | 3.900000 | 0.325000 | 0.508232 | 0.502380 | 18/18 (vacuous) | 1.900000 | 18 | 0 | 18 |
| `foldk-128` | 78 | 3.900000 | 0.325000 | 0.508232 | 0.502380 | 18/18 (vacuous) | 1.900000 | 18 | 0 | 18 |
| `foldk-256` | 78 | 3.900000 | 0.325000 | 0.508232 | 0.502380 | 18/18 (vacuous) | 1.900000 | 18 | 0 | 18 |
| `quant-identity` | 78 | 3.900000 | 0.243750 | 0.508232 | 0.502380 | 18/18 (vacuous) | 1.900000 | 18 | 0 | 18 |
| `species-only` | 58 | 2.900000 | 0.966667 | 0.348770 | 0.345826 | 18/18 (vacuous) | 1.900000 | 18 | 0 | 18 |
| `lastnew-only` | 78 | 3.900000 | 0.975000 | 0.508232 | 0.502380 | 18/18 (vacuous) | 1.900000 | 18 | 0 | 18 |
| `no-channels` | 20 | 1.000000 | 1.000000 | 0.000000 | 0.000000 | 18/18 (vacuous) | 1.000000 | 0 | 0 | 0 |
| `draw-top-64` | 1338 | 66.900000 | 0.085769 | 0.793086 | 0.614937 | 18/18 (vacuous) | 64.600000 | 1278 | 941 | 18 |
| `draw-top-256` | 4503 | 225.150000 | 0.073006 | 0.404594 | 0.716860 | 18/18 (vacuous) | 222.250000 | 4443 | 2189 | 18 |
| `draw-top-only-256` | 4465 | 223.250000 | 0.868677 | 0.379366 | 0.667117 | 18/18 (vacuous) | 222.250000 | 4425 | 2189 | 0 |
| `draw-low-256` | 4469 | 223.450000 | 0.072455 | 0.405902 | 0.714294 | 18/18 (vacuous) | 220.200000 | 4409 | 2194 | 39 |

## Why v1 is blind: the fourth cell *is* the crash

Across the 40 primary-slice campaigns, **40** have every template species debut either on branch 0 or on the finding branch — nothing in between. Of the 29 campaigns that found the bug, **29** mint their last species *exactly at the find*. Of the 11 that did not find it, **11** mint every species on branch 0 and then discover nothing for all 512 branches.

The species, and the lines that mint them (parameters masked to `<*>`, as Drain's own clustering masks them):

| species | debut line |
|---|---|
| 0 | *(a blank line)* |
| 1 | `supervisor: checkpoint committed, batch complete` |
| 2 | `UUID_DRAW: <*> <*>` |
| 3 | `[    <*> traps: <*> general protection fault <*> <*> <*> in <*>` |

Species **3** is the guest kernel's fault message. The campaign filters the bug's `UUID_BUG` attribution marker out of the console before clustering — precisely so the signal cannot key its own marker — but the kernel's `traps: … general protection fault` line rides *behind* the marker and is not filtered. So the one cell v1 ever discovers after branch 0 is minted by the crash itself.

This is not a bug in the marker filter's intent; it is the honest consequence of a bug-agnostic console. It does mean that **v1's `cells@256` statistic, and therefore the ρ = −0.671 the correlation report computed, is a restatement of "did this campaign find the bug before branch 256?"** — not a graded novelty↔progress relationship. `CORRELATION-REPORT.md` already suspected as much ("degenerate: `cells@256` takes only *two* values"); the re-key proves the mechanism.

**The knob space cannot fix it.** With a three-species pre-crash vocabulary, species-progress ranges over `1..=3` and last-new-species over ids `0..=2`. Every `fold_k` in the sweep exceeds 3, so the fold is the identity; `Quant::Identity` distinguishes counts the `Log2` bucket already distinguishes. Every knob-set candidate in the table above therefore ties the control or falls below it, and this is a *proof from the corpus*, not a sampling accident.

## The ranking (on `bug3-campaign` — the sole real discriminator)

Chain preservation **gates**: a candidate that breaks any finding chain is disqualified outright, whatever its curves. Survivors are ordered by the granularity objective at the stated target, tie-broken by raw breadth and then by declaration order — an exact tie means the two candidates *are the same descriptor on this corpus*, and the control is declared first, so a knob variant can never displace the v1 row it is indistinguishable from. On this corpus the gate disqualifies **nothing**; see below.

| # | candidate | (b) O@64 | (a) total | (c) chains | steering | verdict |
|---:|---|---:|---:|---|---:|---|
| 1 | `draw-top-64` | 0.784615 | 2628 | 29/29 chains, 4/4 ancestors | 1331 | eligible |
| 2 | `v1-shipped` | 0.529853 | 149 | 29/29 chains, 4/4 ancestors | 0 | eligible |
| 3 | `foldk-16` | 0.529853 | 149 | 29/29 chains, 4/4 ancestors | 0 | eligible |
| 4 | `foldk-32` | 0.529853 | 149 | 29/29 chains, 4/4 ancestors | 0 | eligible |
| 5 | `foldk-128` | 0.529853 | 149 | 29/29 chains, 4/4 ancestors | 0 | eligible |
| 6 | `foldk-256` | 0.529853 | 149 | 29/29 chains, 4/4 ancestors | 0 | eligible |
| 7 | `quant-identity` | 0.529853 | 149 | 29/29 chains, 4/4 ancestors | 0 | eligible |
| 8 | `lastnew-only` | 0.529853 | 149 | 29/29 chains, 4/4 ancestors | 0 | eligible |
| 9 | `draw-top-256` | 0.446541 | 7513 | 29/29 chains, 4/4 ancestors | 2841 | eligible |
| 10 | `draw-low-256` | 0.438099 | 6778 | 29/29 chains, 4/4 ancestors | 2614 | eligible |
| 11 | `draw-top-only-256` | 0.417338 | 7444 | 29/29 chains, 4/4 ancestors | 2841 | eligible |
| 12 | `species-only` | 0.381124 | 109 | 29/29 chains, 4/4 ancestors | 0 | eligible |
| 13 | `no-channels` | 0.000000 | 40 | 29/29 chains, 4/4 ancestors | 0 | eligible |

**The ranking is a function of the stated target, not of the corpus.** The top three are drawn only from the 13 of 13 candidates that preserve every finding chain (axis (c)); any chain-breaker is omitted here, never shown as a recommendation. At the stated `T = 64` the order is `draw-top-64` → `v1-shipped` → `foldk-16`. At `T = 256` it becomes `draw-top-256` → `draw-top-only-256` → `draw-low-256` — the two `draw-top-*` candidates swap, because Go-Explore's penalty term `√(|n/T−1|+1)` is asymmetric (undershooting the target costs at most `√2`, overshooting is unbounded), so `T` alone decides how much resolution is "too much". Choosing `T` is a human judgment about how much search energy a cell should get; the harness cannot make it, and it is precisely the kind of decision R2 reserves for ratification.


### Axis (c) has no discriminating power on this corpus — say it out loud

The primary slice's 29 finding chains contain **4 proper ancestors in total**, of which **4 are branch 0** — measured over the reconstructed ancestry, not assumed. That follows from the NO-GO's own diagnosis: v1 admits branch 0 (three fresh cells) and then, at most, the finding branch (the crash cell), so a first-finding chain is at most genesis → find. Branch 0 claims a fresh cell under *every* candidate, because the archive starts empty.

**This is a claim about the short first-finding chains, not about the search's ancestry at large.** Across the slice's **7660 exploit branches, 1524 descend from a parent that is not branch 0** — a finding branch enters the frontier and a later exploit step selects it. So axis (c)'s vacuity here is a fact about the finding chains being *shallow* (their proper ancestors are genesis), not about the search never leaving genesis; on a corpus with deeper finding chains the same computation would put non-genesis branches under axis (c)'s microscope, where a coarse candidate could fail.

The consequence is not subtle: **`no-channels` — the candidate that keys all 30 720 branches into a single cell — passes axis (c) with 29/29 chains, 4/4 ancestors.** The playbook's one **bug-based** axis, the one law 6 makes mandatory, cannot distinguish the shipped descriptor from a constant function. It is computed and reported because it is mandatory, and because it *would* fail a candidate on a corpus with real chain depth (the unit tests exercise exactly that). Here it crowns nothing and kills nothing.

So the ranking rests entirely on axes (a) and (b) — the discovery curves law 6 disqualifies as sole evidence. **And on the ablation slice, the one slice free of the exploit's confound, the trigger-aligned `draw-top-256` and its trigger-blind twin `draw-low-256` are indistinguishable on every axis:**

| `bug3-ablation` (unsteered) | (a) total | (a) mean | (a) coverage | (b) O@64 | (b) O@256 | steering | (c) chains |
|---|---:|---:|---:|---:|---:|---:|---|
| `draw-top-256` — reads the trigger byte | 4503 | 225.150000 | 0.073006 | 0.404594 | 0.716860 | 2189 | 18/18 (vacuous) |
| `draw-low-256` — reads a byte no bug uses | 4469 | 223.450000 | 0.072455 | 0.405902 | 0.714294 | 2194 | 18/18 (vacuous) |

The two candidates read the same 64-bit draw. One reads the byte the bug compares; the other reads a byte no trigger in the benchmark ever looks at. Total and mean cells, coverage, the objective at both targets, steering, and chain preservation all agree to within noise. That is Böhme–Szekeres–Metzman (ICSE 2022) reproduced on harmony's own corpus, and it is the reason this report hands over a menu rather than a winner.

They part company in exactly two places, and **neither is evidence of trigger alignment**:

1. **Crash fragmentation.** Even on the clean slice, `draw-low-256` mints 39 `crash-only` cells to `draw-top-256`'s 18 — and the top byte's count is *exactly* one per finding campaign, because every crashing branch draws the same top byte `0xA5`, while the low byte scatters those branches across as many cells as they have distinct low bytes. Those cells are keyed only after the guest has already crashed. **Raw breadth rewards the trigger-blind descriptor here, for fragmenting the crash it should be ignoring.**
2. **The steered slice's cell counts** (7513 vs 6778 total, 187.825000 vs 169.450000 mean) — an artifact of the exploit kernel, not of the trigger. Measured over the 7660 exploit branches of that slice: a child inherits its parent's draw **low byte 43.9% of the time** but its **top byte only 0.4%** (chance is 1/256 ≈ 0.4%). Twiddling a *low* seed bit preserves the low byte in 0/925 of those exploits; twiddling a *high* one preserves it in 3366/6735 (50.0%). So a steered campaign resamples the low byte far less often than the top byte, which inflates the top-byte candidate's cell count for a reason that has nothing to do with `0xA5`. The ablation slice never exploits, which is exactly why the comparison is clean there.

## The ratification menu

**A human (Paul) ratifies. The harness never auto-promotes.** The three highest-ranked *distinct* eligible proposals, each with what it changes and what it risks. Candidates whose every axis is identical to one already listed are the **same descriptor on this corpus** — the knob that separates them addresses a distinction the traces cannot make — so they are folded into that entry and named there rather than padding the menu. A candidate that breaks a finding chain is never offered, whatever its curves. That is why the third entry carries a double-digit rank: the six rows between it and the second are all the same descriptor.

### `draw-top-64` — ranked 1

**What changes.** The cell key gains one chosen sparse state channel — the entropy draw the workload already prints on its console (`UUID_DRAW: draw=0x… prefix_bits=8`), keyed on its top byte and folded `mod 64` by the shipped `DEFAULT_FOLD_K`. The template channels stay exactly as shipped. It ranks first because 65.7 cells per campaign sits almost exactly on the stated target `T = 64` — roughly a quarter of `draw-top-256`'s cells, so a quarter of the promotion pressure, at the cost of aliasing the trigger byte `0xA5` with `0x25`, `0x65`, and `0xE5`.

**What it risks.** The same trigger-alignment critique as `draw-top-256`, plus the fold: three unrelated draws now share the bug's cell, so a selector exploiting that cell is three-quarters of the time exploiting a state that has nothing to do with the trigger. It is the conservative version of the same bet — smaller archive, blunter signal.

### `v1-shipped` — ranked 2

> Partitions the recorded arrivals **identically** to `foldk-16`, `foldk-32`, `foldk-128`, `foldk-256`, `quant-identity`, `lastnew-only` — same cells, same admissions, same chains, same steering; the same descriptor up to cell renaming. Ratifying any of them ratifies this one. (They differ only in `|K|`, and so in normalized coverage: `|K|` counts the cells a config *could* key, not the ones it did.) The `fold_k` and `Quant` knobs have **no effect whatsoever** on this corpus: with a three-species pre-crash vocabulary, every modulus in the sweep exceeds the largest species id, so every fold is the identity.

**What changes.** Nothing. This row is the control, and its reproduction of the campaign's recorded discovery events (all 60 campaigns, every branch, exactly) is the harness's own correctness gate.

**What it risks.** Keeping it is keeping the NO-GO: 3 cells at branch 0, then a frozen archive until the crash mints a fourth. It cannot steer a search because it discovers nothing while the search is running.

### `draw-top-256` — ranked 9

**What changes.** The cell key gains one chosen sparse state channel — the entropy draw the workload already prints on its console, keyed on its top byte, unfolded. The template channels stay exactly as shipped. Cells go from 3–4 per campaign to hundreds, the frontier stops saturating after branch 0, and — for the first time — the archive grows *while the search is still searching* rather than only when it crashes.

**What it risks.** This is the trigger byte. Bug 3 fires exactly when `draw >> 56 == 0xA5`, so this descriptor was chosen with the answer in hand, and its twin control (`draw-low-256`, the same draw's trigger-blind low byte) matches it on every axis of the unsteered ablation slice. Where the two part company it is the exploit kernel's bit-locality talking, or the blind twin fragmenting the crash — never the trigger. That is law 6 (Böhme–Szekeres–Metzman, ICSE 2022) reproduced on harmony's own corpus. Ratifying it is a bet that *some* projection of a guest's chosen state correlates with *some* class of trigger — which is IJON's claim, and a reasonable one — not evidence that this one does. Its cost is also real: 257× the cell space divides per-cell search energy 257 ways (RAID'19: the two most sensitive metrics tested finish below baseline because promotion explodes). Confirm live before believing it.

### The recommendation the harness is entitled to make

Not a candidate — a **sequencing**. The offline filter did its job: it killed the entire v1 knob space (proof, not evidence: a three-species vocabulary has no granularity to tune) and it surfaced one class of candidate that is not blind. It cannot tell you whether that class *works*, because its only bug-based axis is vacuous on this corpus and its two curve axes rate a bug-blind descriptor exactly as highly as a bug-aligned one.

What decides it is a live run, and the spec already names the cheap one: once a `CellFn` is ratified **and** task-95 M2's snapshot speedup lands, run the top candidate against `explore_period ∈ {1, 2, 4}` on bug 3 — an afternoon on the box. Under the 2026-07-09 workloads-first directive, the deciding weight belongs to the game workloads (tasks 86/87) regardless; a bug-3 re-run is a cheap red-flag check, not the gate.

Two structural findings ride out of this report independently of any ratification:

1. **The console the signal reads is not instrumented for search.** Three bug-agnostic lifecycle lines and one crash message is not a species ladder. Every candidate that improved anything did so by reading a *state value* off the console, not a template. This is IJON's thesis, and it points at the guest SDK (`assert`/state-register annotations, task 73's seams) rather than at the cell function.
2. **The marker filter has a hole.** `UUID_BUG` is filtered before clustering; the kernel fault message it precedes is not. Any future campaign whose guest crashes noisily will key its own crash as novelty. Filtering post-crash console output — or, better, keying only on records at or before the seal — is a one-line discipline that should land before the next correlation campaign.

## The limit (playbook step 5, verbatim)

> **State the limit** (EXPLORATION's "diagnostic, not predictive," operationalized): offline
> re-keying proves a candidate *would have* distinguished the *recorded* states; it cannot prove
> the candidate will surface *unrecorded* ones — admission order also shifts which runs would
> have existed (the counterfactual cascade, which step 3c inherits). The playbook is a cheap
> filter that kills bad cell functions, not an oracle that crowns the best one.

It applies to axis (c) with particular force here. The chains this report checks are the chains the *v1* campaign walked. Under a candidate that admits hundreds of cells, the frontier would have held hundreds of exemplars, the selector would have exploited different parents, and the runs that exist in this corpus would largely never have been minted. Re-keying tells you what a candidate would have *said* about the states we recorded. It cannot tell you which states it would have led the search to.
