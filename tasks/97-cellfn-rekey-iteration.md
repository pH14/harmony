# Task 97 — the E-fails re-key harness: offline CellFn iteration over the benchmark trace corpus

> **The E-fails playbook, operationalized.** GO/NO-GO #2 closed **NO-GO** (task 69 M2,
> `dissonance/benchmark/CORRELATION-REPORT.md`, merged 2026-07-09) with a *sharpened* diagnosis
> from the Paul-authorized explore/exploit ablation: the log-template **sensor is fully
> behavior-neutral but nearly blind** (3–4 species per campaign; STADS saturates by branch 2),
> and the **¾-exploit budget was the entire find-rate deficit** (`explore_period=1` reproduced
> baseline seed-for-seed). Per `docs/SCORING.md` §"The E-fails playbook", the response is a
> procedure: freeze (done — the campaign + ablation traces are committed), **re-key CellFn
> candidates offline over the retained traces, score on three axes, and hand Paul a ranked
> ratification menu.** This task builds that harness and produces that menu. It does NOT
> re-open the NO-GO, does not touch the search/Selector (the policy half has its own bounded
> follow-up below), and under the 2026-07-09 **workloads-first directive** any future gate
> re-run is a cheap red-flag check — the deciding weight lives with the game workloads.

Read first: `tasks/00-CONVENTIONS.md`; `docs/SCORING.md` (R1 re-key contract, R2 epoch
controller + candidate knob-sets, R3, the E-fails playbook — **this task implements playbook
steps 2–4**); `docs/EXPLORATION.md` (two-hard-problems discipline, coverage-is-terminal);
`dissonance/benchmark/CORRELATION-REPORT.md` (what failed and the sharpened diagnosis);
`dissonance/logtmpl/` (task 67's `LogSensor` + `CellFnV1` — the v1 being iterated);
`dissonance/runtrace/` (task 65 — the trace format you replay);
`dissonance/explorer/src/spine.rs` (the admission fold re-run in recorded order);
`dissonance/benchmark/campaign-data/` (the corpus).

## Environment

**Delegable, Mac-portable, no box.** Surface: a new `dissonance/rekey/` crate (or a
`benchmark` module if it stays under ~500 lines — implementer's call, record it) + report
artifacts under `dissonance/benchmark/`. `logtmpl`, `runtrace`, `explorer` are **read-only**:
a needed change to any of them is a finding to escalate (bead + report), not a patch. No new
dependencies without the usual justification bar.

## The corpus (pin it, don't trust paths)

The evaluation set is the committed benchmark traces:

- bug-3 campaign: `campaign-data/bug3/…/traces` — 40 campaign trace sets (20 seeds × 2
  configs). **Exclude the solo determinism re-runs** (3 from the campaign, 2 from the
  ablation) — they are replicas, and double-counting a seed biases every axis.
- bug-3 ablation (`explore_period=1`): include as a *separate* corpus slice — by construction
  it is baseline's trajectory with sensor observations attached, i.e. the only slice showing
  what the sensor sees on an **unsteered** search.
- ~~bug-1 campaign as the degenerate control~~ **AMENDED 2026-07-09 (foreman, same day):
  bug-1 retained NO traces** (retention shipped with the bug-3 launch; bug-1 ran before it),
  so offline re-keying over it is impossible. The noise control is instead a **synthesized
  trigger-orthogonal twin candidate**: bug-3's trigger tests the draw's TOP byte
  (`draw>>56 == 0xA5`), so a state channel on the draw's LOW byte is statistically identical
  but trigger-blind — the bug-fitted/bug-blind pair must score near-identically on axes
  (a)/(b), demonstrating on this corpus that breadth/granularity cannot distinguish a
  bug-aligned descriptor from a bug-blind one (law 6's point, sharper than the bug-1 slice).
  bug-1's recorded per-campaign cell counts still appear as a reference row with an explicit
  "no traces retained" note. Runbook lesson beaded: every future campaign retains traces
  from day one.

First deliverable: a **corpus manifest** (`campaign-data/rekey-corpus.json`) listing every
included trace file with its sha256, seed, config, and slice — the harness loads ONLY through
the manifest and fails loudly on a hash mismatch (the hm-xdp lesson: artifacts by content,
never by mutable path). Record total trace count and the exclusions explicitly.

## What to build

### 1. The replay core: exact re-keying (SCORING R1)

For a candidate `CellFn` configuration, recompute every retained timeline's cells from the
recorded sensor inputs — **no guest re-execution** — then rebuild the archive by re-running
the admission fold in the recorded campaign order (spine `claim` semantics, first-wins).
Determinism bar: the same candidate over the same manifest yields byte-identical scores on
every run and host (integer/ordered math only; any map iterated for output is collected-and-
sorted; no wall-clock anywhere).

### 2. The candidate space (SCORING R2's knob-sets — configs, not code)

Candidates are **declarative configs**, each recorded in the report verbatim:

- `fold_k` sweep around today's `DEFAULT_FOLD_K = 64` (e.g. {16, 32, 64, 128, 256} —
  R2 makes `fold_k` a derived quantity with a stated target, so report archive-size vs
  target per value);
- quantization (`Quant`) variants per R2;
- channel-set variants: v1's composition (species-progress ⊕ last-new-species ⊕ per-channel
  reified state) with each element ablated in/out — including at least one candidate that
  adds a **chosen sparse state channel** if the recorded traces carry one (IJON discipline:
  sparse chosen beats indiscriminate; the empty default is a ruling, not an accident);
- v1 exactly as shipped (the control — every axis must reproduce the campaign's recorded
  cell counts for it, which is the harness's own correctness gate).

### 3. The three axes (playbook step 3 — all three, no shortcuts)

- **(a) breadth** — species discovered over the fixed trace set, normalized by total cell
  count (raw QD-style scores scale with cell count; normalization keeps resolutions
  comparable);
- **(b) granularity** — the Go-Explore entropy objective `H_n(p)/√(|n/T−1|+1)` against a
  stated target count, per R2;
- **(c) chain-preservation (mandatory, law 6)** — re-run the admission fold in recorded
  order under the candidate and check that **every ancestor of every bug-finding run still
  claims a cell when it arrives**. A candidate that would have judged any link of a finding
  chain uninteresting would have lost the bug. Report per-bug; discovery curves alone are
  disqualified as evidence. The trigger-orthogonal twin control (see the corpus amendment)
  guards against noise-fitting here.

### 4. The deliverable: `dissonance/benchmark/REKEY-REPORT.md`

Ranked candidates with all three axes + the v1 control row, the corpus manifest hash, and a
**ratification menu**: the top ≤3 candidates with a one-paragraph "what changes and what it
risks" each. **A human (Paul) ratifies; the harness never auto-promotes** (R2: fixed beat
adaptive; auto-tuning only proposes). End the report with the playbook's step-5 limit stated
verbatim: offline re-keying proves a candidate *would have* distinguished the *recorded*
states — it cannot prove it will surface unrecorded ones; the counterfactual cascade caveat
applies to axis (c) too.

## Acceptance gates

1. Standard suite green on the new code (build / nextest / clippy `-D warnings` / fmt /
   deny), macOS + Linux-target cross-check. Determinism: the full scoring run twice →
   byte-identical `REKEY-REPORT.md` body (modulo a single generated-date line, if any).
2. **Harness correctness gate:** the v1-as-shipped candidate reproduces the campaign's
   recorded per-seed distinct-cell counts (3–4) and the recorded finding-chain admissions
   exactly, on every corpus slice. If it cannot, the harness is wrong — stop and fix, never
   tune candidates against a broken replay.
3. Corpus manifest with hashes; loud failure on mismatch; solos excluded and the exclusion
   counted in the report.
4. `REKEY-REPORT.md` committed with the ranked menu. Ratification is **Paul's** — the task
   is done when the menu is in his hands (bead the ratification as a PAUL item).

## Non-goals (fenced deliberately)

- **Touching the search/Selector or `explore_period`.** The ablation *proved* the exploit
  policy is the deficit on bug-3, but policy redesign is Phase-F territory behind the
  workloads-first sequencing — and the SCORING scope fence ("automatic search over feedback
  functions" stays unclaimed) holds. One named follow-up rides separately: after a CellFn is
  ratified AND task-95 M2's speedup lands, a **bounded** box confirmation (top candidate ×
  `explore_period ∈ {1, 2, 4}` on bug-3) becomes a cheap afternoon run — file it as a bead
  at completion, do not build it here.
- Re-running the GO/NO-GO gate — a future re-run is a red-flag check under the
  workloads-first directive, dispatched only by integrator ruling.
- New sensors (OTel etc. are their own tasks); guest or image changes; anything box.
