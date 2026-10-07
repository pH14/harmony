---
name: iterating-searcher-on-a-game
description: Plans and tests Dissonance searcher changes against a whole game with named milestones. Covers the power-on baseline with whole-search checkpoints, rooted segments, the handoff comparison, the tiny-worlds panel and its scorecard, checkpoint slices and the final power-on run, plus the rules for diagnosis, run budgets, seeds and regression checks. Use when planning how to get a game further from power-on, when a run stalls, when a searcher change needs testing on a game, when deciding what to run after a tiny-worlds panel result, or when a tiny world fails to predict a game result.
---

# Iterating the searcher on a whole game

This process has two goals:

- A searcher that finishes the game from power-on in one run, with no game
  concepts in the searcher.
- Tiny worlds that predict the game, so later searcher changes can be vetted in
  minutes instead of days of game runs.

Each step either finds what a hard part of the game needs or tests a change
against the run that has to deliver it. Every game result is also a test of the
tiny worlds.

## Terms

- **Try**: one search execution, a restore plus one suffix of actions. Progress
  lines and manifests count tries as `executions`.
- **Milestone**: a named progress event the workload reports with the first try
  that saw it, such as a boss defeat.
- **Hard part**: the stretch between two milestones where runs spend most of
  their tries or stall.
- **Power-on run**: a search from a new game. It is the only completion test.
- **Checkpoint**: a saved whole-search state that a later run resumes.
- **Rooted segment**: a search that starts from a recorded input with an empty
  archive.
- **Checkpoint slice**: a resume of a power-on run's checkpoint just before a
  hard part.
- **Handoff**: the state a search carries into a hard part. The
  **best-stocked state** holds the most of the resources the hard part
  consumes, of any state the search has found at the hard part's start. The
  **delivered state** is what the power-on run actually brings there.
- **Panel**: `workloads/tiny-worlds/panel.py`, which runs small synthetic worlds
  on the real searcher in minutes.
- **Scorecard**: panel-based predictions for game legs, each written before
  the game run, with the game outcome added after.

## Rules

These hold at every step.

1. Diagnose from film or screenshots first, then counters. Watch what the
   search does at the place: the attempts, where they end and what the states
   hold. Counters summarize, and they miss causes such as a room's layout, a
   misread byte or an enemy pattern. When film cannot cover the archive, draw a
   heatmap of draws per place and look at the image.
2. Reason from what the hard part needs. Treat the current searcher's limits
   as things to change, never as reasons a change cannot help.
3. A run that ends at its budget without the milestone is censored: it needed
   more than that budget, and a longer run might have passed. Count it at the
   tries it reached.
4. Before calling a censored run a failure, decide whether a longer run would
   have passed. Set each budget above the slowest pass of that part in any
   earlier run. Conclude that the search does not pass only with evidence that
   it would not recover. Examples: film shows the same failed attempts
   repeating; the archive gains no new places or better states at the hard part
   over a long stretch; draws never reach the states that could pass. Without
   that evidence, rerun with a longer budget before drawing a conclusion.
5. Stop runs early when several seeds show that evidence, then change the
   machinery. The runner's stall note only reports; stopping is a decision you
   make.
6. Never decide on one seed. Run at least three seeds per arm. A seed means
   nothing across two different searchers, so compare the arms' medians and
   ranges, never one seed against the same seed.
7. Confirm a change actually acts during the run before reading its outcome,
   because a change that never fires looks like a null result.
8. Run the cheapest test that can answer the question first: recorded
   artifacts, then a short probe from a saved state, then the panel, then long
   runs. Before a long run, show with a short probe that the target is
   reachable and that the counter can move.
9. Before every launch, write down the question, the arms, the counter you
   will read, the budget and the expected wall time. For a game run that tests
   a change at a hard part, also name the hard part's tiny world and the
   scorecard prediction the run will score; a hard part with no world gets one
   first (step 6). Skip a launch whose result would change no decision.
10. Improve the general search machinery. The searcher carries no game names,
    coordinates or per-level mechanisms; the custom lints reject game names in
    `dissonance/searcher`.
11. Keep game code in its workload folder, on that game's pull request. A
    searcher change is its own pull request and merges before any game work
    builds on it.
12. Check every searcher change on the SMB three cells and the tiny-worlds
    panel against main.
13. Keep run records, panel output, seed tables and plans out of the
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
- [ ] 5. Diagnose the gap from film; write one general change
- [ ] 6. Tiny world for the hard part: the unchanged searcher loses as in the game
- [ ] 7. Panel: reject clearly bad changes; write scorecard predictions
- [ ] 8. Checkpoint slices, changed and unchanged, 3+ seeds each
- [ ] 9. Power-on run with the change
- [ ] 10. Score the predictions; improve the tiny world that missed
- [ ] 11. SMB three cells, panel, pull request
```

Steps 5 to 10 repeat for each change, whether it lands in the searcher or in
the workload's key. A change that fails a step goes back to step 5 with the
film of the failure. Commands and manifest fields are in
[reference/tools.md](reference/tools.md).

### Step 1: Map milestones and hard parts

List the game's milestones in order and mark the hard parts between them. When
runs choose their own order, name each hard part by its entry and exit
milestones and read its legs per seed.

Check that the game's workload supplies what this process reads, and add what
is missing as workload code before any searcher work. The game's `Reporting`
implementation and `workloads/nes/src/bin/nes-eval.rs` show what exists:

- A named milestone for each area entry, item, boss and the ending, with its
  first try, plus reporting-only milestones inside a long hard part so a stall
  has a named place.
- A tape per milestone's first arrival, and best-stocked tapes beside it.
- What a place is, what orders progress, and which arrival at a place is
  preferable: the three things a workload supplies to the archive. The
  progress order must rise through every part of the game, or the search
  keeps no checkpoint and gives no new tier there.
- Checkpoint support, a film tool, a probe that prints the decoded state, and
  an end-of-run census of the archive per place.
- Runner support for the game's whole-game mode, rooted starts and resumes.

Then check the game reading against film and RAM bytes. Readings fail in a few
repeating ways:

- A flag with more than one value, read as on or off.
- Progress the key cannot see, such as hits on a boss.
- A value that blinks for a frame or is reused by another object after the
  player leaves, read as a real change.
- A milestone latched on the frame the player dies.

### Step 2: Power-on baseline with checkpoints

Run the current searcher from power-on on at least three seeds, keeping a
checkpoint at each new milestone and each new top tier. The checkpoint at the
milestone that opens each hard part is the start point for every later slice.
Film the witness and the milestone tapes, and note where each seed spent its
tries.

### Step 3: Rooted segments at each hard part

A rooted segment answers one question: what does this hard part need from a
good starting state? Root it at the best-stocked tape for the milestone that
opens the hard part, because the first arrival is usually drained. Run three
seeds and read which resources the passing seeds held.

- A segment passes when two of three seeds reach its milestone. Root the next
  segment at the best-stocked tape of that milestone. Never edit a tape.
- When a segment stalls, film it and read the bytes at the stall. Segments find
  game-reading defects fast.
- A fresh archive re-explores ground the root has already passed, so compare
  segment tries only with other segment tries.
- A person picks each root, so a passing segment shows what the hard part
  needs, never that a single run will deliver it.
- A change to the game reading cannot resume from an old checkpoint. Test it
  with segments and a new power-on run.

### Step 4: Handoff comparison

For each hard part, measure two numbers on at least three seeds:

- Tries to pass from the best-stocked state: a rooted segment from its tape.
- Tries to pass from the delivered state: a checkpoint slice of the unchanged
  searcher from the checkpoint that opens the hard part.

Also compare the resources the delivered state held on entry with the best the
archive held anywhere at that tier. Work on the hard part with the biggest
difference first.

Recorded inputs transfer poorly between states that look the same. Timers and
random bytes differ, so a replayed route soon diverges. A change that carries a
stocked state to a hard part has to search the route.

### Step 5: Diagnose the gap and write one change

1. Film the delivered state's attempts. Read counters after that.
2. List the conditions the hard part needs, with the odds you measured for
   each. Mark which ones the search redraws on every attempt instead of
   keeping a success.
3. Find the existing searcher component that does the job badly: selection,
   retention and preference, continuation replay, the suffix draw, or the
   workload's key. Change that component in general terms.
4. Make one change per test. Check that a searcher change can resume from a
   checkpoint. A key change cannot resume one, so step 8 tests it with rooted
   segments.

Some searcher behaviours recur across games. Check for them, and treat them as
starting questions, never as answers:

- A key term that splits a place by resource level makes each drained arrival
  a new place, which spreads draws over drained states.
- A missed replay from the same state misses again, because a replay is exact.
- The top tier takes most draws, so a lower tier that could gather resources
  gets few.

### Step 6: Tiny world for the hard part

Every diagnosed hard part gets a tiny world before its change runs on the
game, whichever component the change touches.

1. Name the mechanism the film shows, such as a consumable spent before the
   barrier that needs it, or progress the key cannot see.
2. Pick the family in `workloads/tiny-worlds` that has the mechanism, or
   extend one as [reference/tiny-worlds.md](reference/tiny-worlds.md)
   describes.
3. Set the world's options until the unchanged searcher loses there at about
   the game's rate: the share of seeds that pass and the tries to pass.
4. Record the world, its options and both loss rates in the notes.

A workload key change still gets a world. The world shows what the searcher
cannot find without the key's help, and a later searcher change is measured on
the same world with that information hidden.

### Step 7: Panel

Run the panel rules and the compare mode against the change's base, with the
hard part's world in the compare. For a workload key change, compare the world
with the information hidden against the world with it in the key. Details are
in [reference/tiny-worlds.md](reference/tiny-worlds.md).

- Drop a change the panel calls clearly bad on any world.
- A world still undecided at its layout limit means the change is not
  plausible yet.
- A plausible result means only that the panel did not reject the change. Treat
  it as a forecast of the game once the scorecard shows the panel's predictions
  land.
- Before any game run, write a scorecard entry for each game leg the change
  should move and for each watch leg the panel names.

### Step 8: Checkpoint slices

Slice each hard part the change targets and each hard part a watch leg names.
Resume the checkpoint that opens that hard part with the changed searcher and
with the unchanged searcher, under three or more new seeds each. Set the budget
above the slowest pass of that part in any earlier run (rule 4). The archive
and the delivered states come along, so every slice includes the handoff.

- A key change cannot resume an old checkpoint. Run rooted segments from the
  same best-stocked tape with the changed and the unchanged key instead, three
  or more seeds each, and read them the same way.
- Confirm from counters or film that the change fired during the slice.
- Read each leg as tries after the resume. Compare the arms' median tries with
  the leg's scorecard range. A few seeds resolve only large differences.
- If the change fails, film the failed attempts and return to step 5.

### Step 9: Power-on run

Run the change from power-on on three or more seeds. Compare the tries to each
milestone with the baseline's, and report emulator frames beside tries,
because a change can cut tries while adding frames per try. This is the final
test, because only a single run shows the search picking and carrying the
state itself.

### Step 10: Score the predictions and improve the tiny worlds

Add each game outcome to the scorecard beside its prediction, hit or miss. A
miss means a world lacks the game's mechanism or runs it at a different rate.
Film of the game leg shows which. Change the world, or build a new one, until
the unchanged searcher loses there the way it loses in the game. Every world
comes from a game loss seen on film, at step 6 or after a miss here. The steps
are in
[reference/tiny-worlds.md](reference/tiny-worlds.md).

### Step 11: Regression checks and pull request

- SMB three cells against main: every cell must solve; compare tries to the
  first win.
- The panel compare against main on the final commit.
- CPU and memory cost, which the Dissonance lens in `REVIEWING.md` asks for.
- Open the searcher change as its own pull request, with the slice and
  power-on numbers and the scorecard entries in its description. Fix every
  custom and semantic lint finding; never add a baseline.

## Reference files

- [reference/tools.md](reference/tools.md): the runner, checkpoint and resume
  fields, films, probes and the regression checks.
- [reference/tiny-worlds.md](reference/tiny-worlds.md): panel commands and
  verdicts, the scorecard, and improving a world.
- [reference/research.md](reference/research.md): the published work behind
  segments, restarts and small test worlds.
