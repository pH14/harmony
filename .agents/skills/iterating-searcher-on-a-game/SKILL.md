---
name: iterating-searcher-on-a-game
description: Plans and tests Dissonance searcher changes against a whole game with named milestones, such as Metroid or Mega Man 2. Covers the power-on baseline with whole-search checkpoints, rooted segments, the handoff comparison, the tiny-worlds panel, checkpoint slices and the final power-on run, plus the rules for seeds, film, stop rules and regression checks. Use when planning how to get a game further from power-on, when a run stalls at a boss or area, when a searcher change needs testing on a game, or when deciding what to run after a tiny-worlds panel result.
---

# Iterating the searcher on a whole game

The goal is a searcher that finishes the game from power-on in one run, with no
game concepts in the searcher. Each step either finds what a hard part of the
game needs or tests a change against the run that has to deliver it.

## Terms

- **Try**: one search execution, a restore plus one suffix of actions. Progress
  lines and manifests count tries as `executions`.
- **Milestone**: a named progress event the workload reports with the first try
  that saw it (`workload_diagnostics.named_progress.first_seen`), such as
  `kraid_defeated`.
- **Hard part**: the stretch between two milestones where runs spend most of
  their tries or stall, usually a boss or a closed-off area.
- **Power-on run**: a search from a new game. It is the only completion test.
- **Checkpoint**: a saved whole-search state, a `.ckpt` file. A resume with the
  same seed repeats the original search; its progress lines match apart from
  timing-dependent fields. A resume with another seed keeps the archive and
  draws new choices.
- **Rooted segment**: a search that starts from a recorded input (`root_input`)
  with an empty archive.
- **Checkpoint slice**: a resume of a power-on run's checkpoint just before a
  hard part, run for a few million tries.
- **Handoff**: the state a search carries into a hard part. The
  **best-stocked state** holds the most of the resources the hard part
  consumes, such as health, ammunition or weapon energy, of any state the
  search has found at the hard part's start. The **delivered state** is what the power-on run actually
  brings there.
- **Panel**: `workloads/tiny-worlds/panel.py`, which runs small synthetic worlds
  on the real searcher in minutes.
- **Scorecard**: panel-based predictions for game legs, each written before
  the game run, with the game outcome added after.

## Rules

These hold at every step.

1. Never decide on one seed. Run at least three seeds, and run the changed
   and unchanged searchers on the same seeds.
2. Look at film or a heatmap of the place before reading counters, because
   counters hide the room's geometry.
3. Run the cheapest test that can answer the question first: recorded
   artifacts, then a probe of 200K to 500K tries from a saved state, then the
   panel, then long runs.
4. Before every launch, write down the question, the arms, the counter you
   will read, the stop rule and the expected wall time.
   Skip a launch whose result would change no decision.
5. Before a long run, show with a short probe that the target is reachable and
   that the counter can move.
6. Set every stop rule above the largest gap between milestones in the
   control's record, because a tighter rule stops runs that would have passed.
7. When several seeds stall at one place for longer than the control's largest
   gap there, stop the runs, film the place and change the machinery. The
   runner's stall note only reports; stopping is a decision you make. A
   stopped run keeps its progress lines, milestone tapes and checkpoints, and
   writes no witness or final census.
8. Confirm a change actually acts during the run before reading its outcome,
   because a change that never fires looks like a null result.
9. Count an unfinished or stopped run as censored at the tries it reached.
10. Reason from what the hard part needs. Treat the current searcher's limits
    as things to change, never as reasons a change cannot help.
11. Improve the general search machinery. The searcher carries no game names,
    coordinates or per-level mechanisms; the custom lints reject game names in
    `dissonance/searcher`.
12. Keep game code in its workload folder, such as `workloads/nes/src/mm2`,
    on that game's pull request. A searcher change is its own pull request and
    merges before any game work builds on it, so game branches never conflict
    in the searcher.
13. Check every searcher change on the SMB three cells and the etcd case
    against main. A change made for another game also runs Metroid checkpoint
    slices.
14. Keep run records, panel output, seed tables and plans out of the
    repository; the numbers a decision rests on go in the pull request
    description.

## Workflow

Copy this checklist into your notes and keep it current:

```
Game iteration progress:
- [ ] 1. Map milestones and hard parts; check the game reading
- [ ] 2. Power-on baseline with checkpoints on 3+ seeds
- [ ] 3. Rooted segments at each hard part
- [ ] 4. Handoff comparison; pick the biggest gap
- [ ] 5. Diagnose the gap; write one general change
- [ ] 6. Panel: reject clearly bad changes; write scorecard predictions
- [ ] 7. Checkpoint slices on 3+ seeds against the unchanged searcher
- [ ] 8. Power-on run with the change
- [ ] 9. SMB three cells, etcd case, pull request
```

Steps 5 to 8 repeat for each change. A change that fails a step goes back to
step 5 with the film of the failure.

Commands, manifest fields and file locations are in
[reference/tools.md](reference/tools.md).

### Step 1: Map milestones and hard parts

List the game's milestones in order and mark the hard parts between them. In
Metroid the hard parts are Kraid, Ridley, Tourian and Mother Brain; see
[reference/metroid-example.md](reference/metroid-example.md). When runs choose
their own order, as Mega Man 2's stage select allows, name each hard part by
its entry and exit milestones and compare legs per seed between those two.

Check that the workload in `workloads/nes/src/<game>` supplies what this
process reads. Add what is missing as workload code before any searcher work:

- A named milestone for each area entry, item, boss room, boss defeat and the
  ending, with its first try. Names use underscores, because the runner reads
  a tape's milestone from its file name up to the first `-`.
- Reporting-only route milestones inside a long hard part, so a stall has a
  named place. Metroid added `tourian_corridor`, `tourian_far` and
  `tourian_bottom` for this.
- A tape per milestone's first arrival, and best-stocked tapes beside it.
  Metroid writes `NAME-energy.json`, `NAME-missiles.json` and `NAME-boss.json`.
- The three things a workload author supplies to the archive: what a place is,
  what orders progress, and which arrival at a place is preferable.
- Checkpoint support through `Reporting::evidence_checkpoint`,
  `Reporting::evidence_from_checkpoint` and `Reporting::checkpoint_marks`.
- A film tool and a probe that prints the decoded state at each action
  endpoint, and an end-of-run census of the archive per place
  (`retained_diagnostics`).
- Runner support: `nes-eval` accepts the game's whole-game mode,
  `root_input` and `resume`, and `benchmarks/search/milestones.py` and
  `ladder.py` hold the game's milestone list beside Metroid's.

### Step 2: Power-on baseline with checkpoints

Run the current searcher from power-on on at least three seeds with
`checkpoint_on_progress` set, so a checkpoint is kept at each new milestone and
each new top tier, and with `checkpoint_every` for the interval checkpoints.
The checkpoint at the milestone that opens each hard part is the start point
for every later slice. Keep those files with the run.

Read the run with `milestones.py`, film the witness and the milestone tapes,
and note where each seed spent its tries.

### Step 3: Rooted segments at each hard part

A rooted segment answers one question: what does this hard part need from a
good starting state? Root it at the best-stocked tape for the milestone that
opens the hard part, run three seeds, and read which resources the passing
seeds held.

- Root at the best-stocked tape, because the first arrival is usually drained.
- A segment passes when two of three seeds reach its milestone. Root the next
  segment at the best-stocked tape of that milestone. Never edit a tape.
- When a segment stalls, film it, then read the bytes at the stall. Check
  whether the game reading sees the progress being made. Segments find these
  reading defects fast: in Metroid they showed that the statue raise and
  Mother Brain's hits were never read.
- Name the missing quantity, change one thing, and rerun the segment.

Segments have three limits:

- A fresh archive re-explores ground the root has already passed, 57% to 89%
  of the tries in one Metroid segment, so compare segment tries only with other
  segment tries.
- A person picks each root. A single run has to pick and carry the state
  itself, so a passing segment shows what the hard part needs and never that
  the run will deliver it.
- A change to the game reading changes key extraction, which a checkpoint
  cannot resume across. Test reading fixes with segments and a new power-on
  run.

### Step 4: Handoff comparison

For each hard part, measure two numbers on at least three seeds:

- Tries to pass from the best-stocked state: a rooted segment from its tape.
- Tries to pass from the delivered state: a checkpoint slice of the unchanged
  searcher from the checkpoint that opens the hard part.

Also compare the resources the delivered state held on entry with the best the
archive held anywhere at that tier. Work on the hard part with the biggest
difference first. In the Metroid example that was Tourian: power-on runs
entered with 7 to 94 of 299 energy, while full-energy states sat far from
Tourian for more than 15M tries.

Recorded inputs transfer poorly between lineages. At the same position, the
frame counter, the random bytes and about 130 other RAM bytes differ, so a
replayed route mistimes jumps within a few actions, and a continuation hop
replayed from another state lands in 12% to 24% of tries. A change that
carries a stocked state to a hard part has to search the route.

### Step 5: Diagnose the gap and write one change

1. Film the delivered state's attempts, or render a heatmap of the archive
   census around the hard part. Read counters after that.
2. List the conditions the hard part needs, with the odds you measured for
   each. Mark which ones the search redraws on every attempt instead of
   keeping a success.
3. Find the existing searcher component that does the job badly: selection,
   retention and preference, continuation replay, the suffix draw, or the
   workload's key. Change that component, in general terms.
4. Make one change per test. Check that it can resume from a checkpoint, and
   give the policy it alters a new identifier so the resume records the
   change (details in [reference/tools.md](reference/tools.md)).

### Step 6: Panel

Run the panel rules and the compare mode. Details and verdict rules are in
[reference/tiny-worlds.md](reference/tiny-worlds.md).

- Drop a change the panel calls clearly bad on any world.
- A verdict needs every world resolved. A world still undecided at its layout
  limit means the change is not plausible yet. A watch leg passes, and its
  game leg must be measured in step 7.
- A plausible result means only that the panel did not reject the change.
  Treat it as a forecast of the game once the scorecard shows panel
  predictions match game outcomes.
- Before any game run, write a scorecard entry: for each game leg, the
  predicted ratio of changed to unchanged tries with an 80% range. Name the
  game leg for every watch leg the panel reports.

Build a new tiny world only after the real game shows where the loss is, and
check that the world reproduces that loss at the game's measured rate.

### Step 7: Checkpoint slices

Slice each hard part the change targets and each hard part a watch leg names.
Resume the power-on run's checkpoint that opens that hard part, once with the
changed searcher and once with the unchanged searcher, on the same three or
more new seeds. Run each slice for a few million tries. The archive
and the delivered states come along, so every slice includes the handoff.

- Confirm from the progress counters or film that the change fired during the
  slice.
- Read each leg as tries after the resume. Compare the median ratio of changed
  to unchanged across seeds with the leg's scorecard range, and list each
  seed's ratio. A few seeds cannot resolve effects as small as 0.8 to 0.9, so
  a slice catches only large differences.
- Add the outcome to the scorecard next to its prediction, hit or miss.
- If the change fails, film the failed attempts and return to step 5.

### Step 8: Power-on run

Run the change from power-on on the baseline's seeds. Compare the tries to each
milestone per seed with the baseline, and report emulator frames next to
tries, because a change can cut tries while adding frames per try. This is the final test, because only a
single run shows the search picking and carrying the state itself.

### Step 9: Regression checks and pull request

- SMB three cells: `benchmarks/search/smb-regression-three.json`. Every cell
  must solve; compare tries to the first win with main.
- The etcd case, `workloads/bugs/historical/etcd-3.5-inconsistency`: it must
  still find the bug.
- CPU and memory cost, which the Dissonance lens in `REVIEWING.md` asks for.
- Open the searcher change as its own pull request. Put the slice and power-on
  numbers and the scorecard entries in its description. Fix every custom and
  semantic lint finding in the file; never add a baseline.

## Reference files

- [reference/tools.md](reference/tools.md): manifests, checkpoint fields,
  resume rules, films, probes and regression commands.
- [reference/tiny-worlds.md](reference/tiny-worlds.md): panel commands and
  verdicts, the scorecard, and when and how to build a world.
- [reference/metroid-example.md](reference/metroid-example.md): the whole
  process applied to Metroid.
- [reference/research.md](reference/research.md): the published work behind
  segments, restarts and small test worlds.
