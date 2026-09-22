# Searcher consolidation plan

Running plan. Started 2026-09-21. Edit in place; keep the log at the bottom.

## Decisions made

1. The archive has two levels: cells and the progress tiers over them. The
   five-depth walk is removed. This is an architecture call, not a
   hypothesis to test.
2. The continuation graph carries better states forward, and it works. Same.
3. A workload author supplies three things: what a place is, what orders
   progress, which arrival at a place is preferable. A mid-tier model must be
   able to instrument a new game with those three and nothing else.

Measurement exists to make these work, never to decide whether to do them.

4. Replaced code is deleted in the same commit. No switch between the
   five-depth walk and the new design, no legacy selector identifier, no
   compatibility path for old recordings. Version control holds the old
   code.

## Goal

Metroid finishes from power-on on one archive with no re-rooting. The
23-segment chain from the Kraid kill is the fixture.

## The design

| Author supplies | Today | Target |
|---|---|---|
| A place | `group(depth)` at five depths | `cell()`: one function, the unit of equal selection |
| Progress order | `progress_cmp` over the class group | `progress_cmp` over cells; the tier ahead takes more selections |
| Preference | `preference_cmp`, `preferences()`, `slot_capacity()` | `preference_cmp` and a capacity |

Selection: pick a tier by progress, pick a cell in it by count decay, pick
one of its holders. Retention: a cell keeps holders distinct by holder
identity (fine position, posture, door, boss damage), and at each identity
up to the capacity under each preference. Propagation: an improved holder
replays the exits recorded from its identity; a landing is an arrival
anywhere in the destination cell; an exit's source is keyed on holder
identity without items or tanks, so routes survive pickups and keep their
entry context. A landed state counts as useful when it takes a further exit.
The pre-replay holder check becomes a priority: edges whose recorded tail
gained resources are dispatched even when the source does not outrank the
destination's holders.

Settings that remain: the tier multiplier, the count decay exponent, the
continuation share, the capacity.

The trait as built: a key names its place (`place()`, the map screen), its
progress (`progress()`, an ordered value), and its holder identity
(`identity()`, the fine state at a place without resources or progress).
A cell is the pair of progress and place. A retention slot is a cell plus an
identity. An exit's source is a place plus an identity, so it carries no
progress or resources; an edge's target is a place. Settings are constants in
the archive: the tier multiplier is eight per rank capped at eight ranks, the
count decay is one over one plus the draw count, the continuation share is
one reservation in four, and the capacity is the key's. One selector
identifier names this; the stream schema version rises so old recordings are
rejected before replay.

For Metroid: the cell is area, map screen, items. Boss damage is holder
identity, so a hit that spent a missile is kept beside the state that did
not fire. Two preferences, missiles first and health first, with tanks
ahead of both. Route milestones and Zebetite columns leave the key and stay
in reporting; the Tourian boss reading already scores column hits.

## Build order

One branch. Each item is written before the next; nothing waits on a
measurement. The ladder runs when the build is complete.

1. Cell, progress and preference replace the five depths in the trait and
   the archive. Count decay per holder replaces leaf novelty, entry
   retirement and the per-depth energy scales. Recency reset on improvement
   stays.
2. Continuations target the cell. Exit sources keyed on holder identity
   without items or tanks; landing is arrival in the destination cell. Each
   edge records its resource change. The holder check orders dispatch and
   admits resource-gaining edges; acceptance is at the destination after
   replay.
3. Every workload moves to the new trait: Metroid, SMB, Mega Man 2, Nova,
   Smash, faults. Old selector identifiers deleted.
4. Metroid key: boss damage into holder identity, tanks into the
   preference, second preference on; milestones and columns out of the key.

Retention fixtures from the chain, replayed on every build: segment 12's
(10,11) arrival at 83.4 energy and 9 missiles must be held beside the
39-missile holder; segment 14's fired-missile state in the Zebetite room
must be held beside the unfired one.

## Acceptance

The ladder. Each of the 23 roots plus its segment's outcome is one fixture.
The score is segments passed in a row from a root without a re-root. Today
it is zero. The old build runs the same ladder once for the baseline, so a
segment the new build loses and the old build passed is visible on the
same table.

The three SMB cells run on every build of the branch.

A segment that fails gets diagnosed, never measured around. Film or heatmap
first. Then the counterfactual at that root: continue the previous
segment's archive against a re-root, and read whether the root state is
still held (retention), how often it was selected (selection), and whether
it was selected often and still failed (input draw or horizon). Fix the
named loss and re-run the ladder from that root.

Power-on runs on six seeds once the ladder passes end to end. Rooted runs
diagnose; power-on runs are the only completion test.

## Guards

- Delete what is replaced. One selection rule, one retention rule, one
  graph. A review finding a toggle or a dead path is a blocker.
- No new game-specific key coordinates. Reporting may name anything.
- One branch, one build under test at a time. The ladder is the regression
  check across all 23 roots, so a gain at one root and a loss at another
  show together.
- Never rule on one seed.
- Film or heatmap before counters when a segment fails.

## Log

- 2026-09-21: plan opened. Chain state: Ridley killed 3/3 in segment 3,
  Mother Brain killed on seed 11 in segment 22, segment 23 running on key
  v21. Five-depth walk found in every NES workload; only Metroid carries boss
  damage and tanks at every depth. Literature has no selection over more
  than two levels of granularity. Continuation graph measured 2026-09-18:
  24% of replays land at the exact slot, best hideout missiles 11 to 15,
  beat control on five of five roots.
- 2026-09-21: project decision: two levels and a working continuation graph
  are requirements. Plan rewritten from per-mechanism arms to one build against
  the ladder.
- 2026-09-21: Astra review (xhigh), four findings, all accepted with edits:
  missiles-first preference discards a fired hit (segment 14), so boss damage
  is holder identity; one holder per position discards stocked arrivals
  (segment 12), so the second preference is on and both cases are fixtures;
  executing every refused continuation cost landings 50K to 35K per cell, so
  the holder check stays as a priority with resource-gaining edges admitted;
  screen-keyed exits lose entry context, so exit sources keep holder
  identity and only the landing target is the cell.
- 2026-09-22: branch searcher-consolidation from 09a8bb60b. The ms02 disk
  was full; the September 1 autoresearch result trees were deleted (578 GB
  free). The ladder manifest `benchmarks/search/metroid-ladder.json` holds
  the seventeen distinct chain roots (segments sharing a root give the same
  run on one build); `ladder.py` scores segments passed in a row from each
  root on two of three seeds. The old-build baseline (builds/p3-recency17,
  key v21) launched on ms02 at 02:37 box time, five cells at a time.
- 2026-09-22: build items 1 and 2 landed in one commit on the searcher
  crate (two levels, count decay, continuations keyed on place and holder
  identity, edge resource gains, gaining edges admitted). Items 3 and 4
  landed together because the workload crates do not compile against the
  new trait until every key moves: Metroid's tier is the items held, its
  place is the area and map cell, its holder identity is position, posture,
  door and boss damage, and both resource orders are on with tanks ahead.
  SMB keeps its banded progress as the tier and its room as the place. Mega
  Man 2 keeps boss clears and boss damage as the tier. Nova, Smash and
  faults have no tier, so every place there is a peer, as before. Selector
  flags, identifiers and thresholds are gone from every binary, package and
  manifest. Both workload crates pass their tests and clippy.
