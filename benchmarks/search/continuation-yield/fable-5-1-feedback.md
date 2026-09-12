**Verdict first.** Retention aliasing is real and locally measurable, but the tranche has not shown it is the bottleneck, and the mechanistic evidence points to a low ceiling. Worse, the "failed" gates are close to uninformative, because nobody has measured how much milestone times vary between seeds under the frozen control. That one number decides whether any development gate in this program can see a real effect. Get it from existing logs first, then screen one selection-side intervention on a small pre-listed seed panel. Drop retention as the primary lever for the renewed tranche.

## Diagnosis

**1. Evidence versus hypothesis.**

Established by replay, exact fixtures, or complete streams:
- **Same-slot competitors have different useful futures.** P03, U01 and H02 show this at 24 actions and at one to six actions.
- **Local coverage gaps were avoidable at capacity two in hindsight.** The finite-cover certificates are checkable without assets.
- **Retained states that were never later selected were mostly continued in their birth job.** That kills "never selected means never explored."
- **Every missing map event in U01 and H02 points to a map already seen at an earlier retained birth.** The loss is local, not global.
- **Five retention families gave no qualifying speedup on one or two development seeds.** The motion-context gain reversed on a fresh seed.
- **Probes are expensive.** Blanket online probing is infeasible under a five percent overhead.

Still hypotheses:
- That fixing local aliasing would speed adaptive search. Untested in any informative way, see below.
- That the right objective is map events rather than survival. The two reverse on the same replayed data.
- That a better descriptor exists. P05 shows the motion partition does not concentrate useful differences.

**Is retention the wrong bottleneck?** Probably yes, for three reasons that do not depend on the failed gates. First, whole-archive redundancy. The prior-map check shows every locally lost event was already reachable from elsewhere. A slot-level loss only costs global progress when no other active state with comparable selection mass reaches the same future. Second, the effect ceiling. A retention rule can only change the outcome of jobs that select a mis-retained slot. That mass is bounded by selection probability times uncovered feature mass, summed over slots. Nobody has computed this, but four gaps in seven selected competitions, all on maps already seen, suggest it is small. Third, cost. Any online fix needs either a free predictor, which P04 and P05 say you do not have, or probes, which the cost scenario rules out at scale.

**The failed gates are weaker evidence than the synthesis implies.** Every development comparison is one seed per arm against a fixed percentage threshold on a milestone chosen among several. The fresh-seed reversal is the clearest measurement of noise in the packet. Ordinary went from no tank to a tank, and context went from a tank to no tank, on a change of seed alone. If the between-seed coefficient of variation of milestone time is anywhere near a third, a single-seed gate has almost no power and a high false-positive rate. Both failure modes appeared: an initial gain that reversed, and failures that may be noise. So the tranche cannot say retention does not help. It can say it has not built an instrument that could tell.

**The information that most changes the next decision** is the dispersion of control milestone times across seeds at matched work. It is probably already in historical control runs and the R05 arms. Second is the selection-weighted stake below. Neither needs a long run.

## Framing, decision rule, and experiments

**2. Framing.** Each job selects a parent from the active archive with empirical probability pi, draws a suffix from the action distribution D, executes, and updates the archive. Let F of s be the set of suffix-and-event features reachable from s under D within the job horizon, restricted to frontier events: a new cell, a capability pickup, or survival into an unseen cell. The progress rate r is the sum over parents of pi times the probability that the continuation yields a frontier event not already in the archive. Three levers move r. Retention changes which parents exist. Selection changes pi. The action distribution changes D.

A retention decision at slot k that replaces the retained set changes r by at most pi of k times the uncovered feature mass. Define the stake:

```
S   = sum_k  pi(k) * G_k
G_k = weight of measured frontier features in the offered set
      that are not covered by the retained set R_k
      and not covered by the union of F over other active states
      whose pi is at least pi(k) / 10
Y   = observed frontier events per selection in the same stream
```

The second clause of G is the whole-archive redundancy term. Approximate it with the prior-map witness rule already implemented, tightened to require that the witness is still active with non-negligible pi. Read pi directly from the recorded deterministic stream. It is the actual sampling probability, not a model. Compute S only over slots in the top decile of pi mass plus a small random sample of the rest. That avoids a global oracle. Charge any replay used to measure G as physical frames and report it.

The adaptive confound is explicit. Pi comes from one arm's adaptive path, which is the right population for that run and the wrong one for any other. Require the same qualitative answer on two independent streams before acting.

Physical compute enters as a per-decision inequality. A probe at slot k is worth running only if pi of k times G times the value per event exceeds the probe's frame cost times the frontier value per frame. Under the cost scenario this holds at a handful of high-pi slots, if anywhere.

**Decision rule.**

| Quantity | Threshold | Decision |
|---|---|---|
| Control milestone CV across seeds | above 0.35 | No mechanism screen is interpretable at fewer than eight seeds. Fix the protocol first. |
| Control milestone CV | 0.15 to 0.35 | Screens need at least four paired seeds and a rank-based pass rule. |
| Control milestone CV | below 0.15 | Two to three paired seeds suffice. |
| Stake ratio S/Y on two streams | below 0.05 | Retention cannot deliver a qualifying speedup. Stop retention. |
| S/Y | above 0.20, concentrated in top-decile pi slots | Design sparse frontier probes, experiment E3. |
| S/Y | between | Stop retention this tranche. Revisit only with a free predictor. |

The rough sample-size logic behind the first rows, for an unpaired one-sided comparison at conventional power, with pairing possibly reducing it:

```
n per arm  ~  12 * (CV / delta)^2
```

At a 20 percent target and CV of 0.3 that is about 27 seeds per arm. At CV of 0.1 it is three. That is why the CV comes first.

**3. Experiments, ranked. E1 is the first and only committed step.**

**E1. Null dispersion and retention stake from existing artifacts.**
- **Question:** can any current gate see a 20 percent effect, and is the retention stake large enough to matter?
- **Minimum data:** every ordinary-control stream and milestone log from this and prior tranches, the three R05 arms, the five qualification streams. If fewer than four control seeds share a milestone, run pre-listed control seeds to the missiles milestone with a 25M-frame cap until four exist.
- **Implementation:** a log parser for milestone arrival and admitted work, plus a pi-weighted extension of the U01 and H02 analysis over top-decile slots, reusing the prefix-observing replay.
- **Comparator:** none for the CV. The frontier yield rate Y for the stake.
- **Primary outcome:** CV of milestone time at matched admitted work, and S/Y on two streams.
- **Result to decision:** exactly the table above. High CV means the next step is a multi-seed protocol, not a mechanism. Low S/Y means retention is dropped and E2 starts autonomously. High concentrated S/Y means E3 instead of E2.
- **Cost:** about one engineering day. Emulator work under one hour, plus up to four control cells if the logs are thin.
- **Hard stop:** one working day and two emulator-hours. If the logs cannot yield a milestone time per control seed, that is the finding. Run the four control cells. Do not reopen any retention design.

This fails in a day and decides two things at once. It is not a substitute for progress. It is the instrument calibration without which every subsequent hours-long run is a coin flip.

**E2. Yield-adaptive parent selection.** Runs if E1 shows small S/Y and a measurable CV.
- **Question:** does shifting pi toward cells whose recent continuations produced new cells or events speed milestones at matched work?
- **Minimum implementation:** one selector weight change in the generic searcher. Multiply the existing weight by a bounded function of recent yield per cell, with a floor so no cell starves and a fixed window. Freeze window and floor before any cell runs. No game knowledge, no tapes.
- **Comparator:** frozen ordinary retention and selector, same seeds, CPU swap.
- **Panel:** k fresh development seeds from the CV table, minimum four, listed before the first cell. Horizon is the earliest milestone with a measured CV on each game.
- **Primary outcome:** milestone admitted-frame time per seed, both arms.
- **Pass:** candidate wins at least k minus one pairs, and median speedup exceeds the larger of 0.15 and 1.5 times CV. A pass triggers the other game under the same protocol, without asking.
- **Cost:** two arms times k seeds times roughly 20M frames, so on the order of 200M frames. A few hours if search runs near replay throughput.
- **Hard stop:** twelve emulator-hours. A wall-censored cell is reported, never counted.

Why selection before action distribution: pi is where the frames go, the exposure audit says retained states are already being continued, and tree-growth methods are known to be dominated by selection bias. Action distribution is the next family if E2 fails, not a parallel arm.

**E3. Sparse frontier probe retention.** Runs only if E1 shows S/Y above 0.20 concentrated at high-pi slots.
- **Question:** does probing only the top one percent of pi mass, with the contract's union-coverage replacement, beat control at total physical frames including probes?
- **Minimum implementation:** gate the existing complete-slot observer on pi rank, run the 16-suffix probe, apply the two-state finite-cover selection online. Everything else exists.
- **Comparator:** frozen control at equal total physical frames, not admitted frames.
- **Primary outcome:** milestone time. Secondary, probe overhead fraction.
- **Pass and panel:** as E2.
- **Hard stop:** fifteen emulator-hours. Abandon if probe overhead exceeds five percent of physical frames.

## Goal, literature, and the case against this plan

**4. Revised goal draft.**

```
Goal: find a generic search change that reaches the Metroid boss or MM2 Wily 4
sooner than the frozen control at matched admitted work and 8 GiB, replicated
on fresh seeds. No solution tapes, no game-specific navigation.

Tranche bound: 7 calendar days or 60 emulator-hours, whichever comes first.
Permission for every cell inside this bound is granted here. Frozen baselines,
replay verification, provenance, matched work and memory, and the untouched
validation panel are mandatory in every phase.

Phase 0, decision (hard stop 1 working day, 2 emulator-hours):
  Compute control milestone CV across seeds and the selection-weighted
  retention stake from existing streams. Apply the published decision table.
  Write a one-page decision record. Proceed without asking.

Phase 1, screen (autonomous on Phase 0 clearance, cap 12 emulator-hours):
  One frozen intervention chosen by the table. Pre-listed fresh development
  seeds, k from the CV table, minimum 4, both arms, CPU swap.
  Pass: wins k-1 of k pairs and median speedup exceeds max(0.15, 1.5 CV).

Phase 2, cross-game and long horizon (autonomous on Phase 1 pass, cap 30
emulator-hours): same candidate on the other game, same protocol. Then
the boss or Wily 4 endpoint on 3 fresh seeds per game.
  Pass: at least one candidate-only boss or Wily 4 success at matched work
  with a replayed witness, and no control-only success.

Phase 3, validation (only on Phase 2 pass): the untouched 10 Metroid and
5 MM2 panel per validation-protocol.md, seeds listed before launch.

Genuine progress means a replayed boss or Wily 4 witness earlier than the
control on a fresh seed. Diagnostics, counterexamples, documentation and
passing CI do not count.

Stop and pivot rules:
  Phase 0 CV above 0.35: stop mechanism screening. The deliverable becomes a
  multi-seed protocol with a variance-reduced endpoint, then re-enter Phase 1.
  Phase 1 fail: switch family once, from selection to action distribution.
  Two Phase 1 fails: stop the tranche and write a terminal record.
  Wall-censored cells are reported separately and never counted.
  No retention family re-enters unless a free predictor is shown on a
  frozen suite first.
```

**5. Literature tied to decisions.**

Supplied and verified in the packet:
- **Lehnert and Littman on reward-predictive representations.** Use it to insist any E3 predictor is action-conditioned. It does not license a policy-averaged fingerprint.
- **Intelligent Go-Explore, arXiv 2405.15143.** Its per-state tried-action history is the closest published analogue to E2's yield window. Its ablations vary by environment, so expect no fixed effect size.
- **TopoExplore, arXiv 2607.09971.** Dated July 2026, after my knowledge cutoff, so I cannot vouch for details. Its Montezuma degradation without a wall mask is the reason to keep frontier bonuses out of E2.
- **Hoeffding 1963,** for the probe-count bounds in the contract.

Recalled from memory, details to be checked:
- **Ecoffet et al., "First return, then explore," Nature 2021.** Cell selection uses a count-based weight with optional domain bonuses. E2 is a minimal bandit version of that weight. Confident of the paper, less of the exact formula.
- **LaValle's RRT, 1998 technical report.** Nearest-to-random-sample selection yields a Voronoi bias toward large unexplored regions, and nodes are never pruned. This is the cleanest analogy for "selection, not retention, drives growth."
- **Maron and Moore, "Hoeffding races," 1994.** Sequential elimination of candidates under a shared budget. Use it to stop a Phase 1 panel early instead of running every seed. Moderately confident of the citation.
- **Mouret and Clune, MAP-Elites, 2015.** One elite per cell is the standard. The two-representative slot is a departure worth keeping only if E1 shows stake.
- **Ferns, Panangaden and Precup, bisimulation metrics, around 2004.** Relevant only if anyone returns to a formal equivalence claim.

**6. Strongest rival and cheapest falsifier.**

The strongest rival is that retention matters late and early milestone gates cannot see it. H02 shows both survivors dying at six actions while unretained continuations live. Near the boss the archive is thin, redundancy is lower, and one lost survivor may cost a corridor. If so, E1's stake on the 50M streams will rise sharply with stream position and concentrate on late high-pi slots. That falsifies my "drop retention" recommendation, and E1 produces it for free. The second rival is that everything is noise and no generic mechanism is visible below ten seeds. E1's CV row is its falsifier. If CV is small, that rival dies.

**On the gates.** The single-seed percentage milestone gate created the outcomes in this packet. With unknown null dispersion it is at once a coin flip and a winner's-curse machine. Run several families across several milestones, one seed each, and one will pass, then reverse. Replace it with an effect size in units of measured dispersion, a pre-listed seed panel, and one milestone per game fixed before the first cell. The two-game breakthrough framing has a second cost. It rewards big-swing mechanisms that might plausibly move a boss and punishes small measured improvements that compound. Keep boss and Wily 4 as the definition of progress, but let the screening endpoint be the earliest milestone whose dispersion has been measured. A candidate that reliably reaches missiles sooner across six seeds is real progress. A candidate that reached a tank once is not.