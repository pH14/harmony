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
the archive: the tier multiplier is eight per rank capped at eight ranks
unless the key says otherwise, the count decay is one over the square of one
plus the draw count, the continuation share is one reservation in four, and
the capacity is the key's. A key supplies its own tier multiplier when its
progress order has close steps: SMB's tier is the level and a four-screen
band ranked two to one, because a level alone as the tier failed all three
SMB cells with every other fix in place (build thirteen, log of 2026-09-22),
and a band at eight to one starved the maze levels' branch points. One selector
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
- 2026-09-22: the two retention fixtures are tests on the Metroid key: the
  stocked-energy arrival at the Tourian shaft bottom is held beside the
  missile holder in either insertion order, and a fired missile that hit a
  column is held beside the unfired state. Both pass. The new build
  (builds/consol1 on ms02, from commit b596b05d1) runs the three SMB cells
  and then the ladder at three cells at a time beside the baseline.
- 2026-09-22: the first build (consol1) failed the first SMB cell at two
  million executions, stalled in world 4-4, where the old build solved at
  1.43 million. The draws-per-cell heatmap of that cell shows nearly every
  draw in the two highest progress bands of 4-4 (a wrapped progress value
  from the maze sending Mario back) and a few hundred draws at the cells
  where the route branches. A tier of (world, level, band) starves places
  inside a level. The old build's class was the level alone, so places in a
  level were peers. Fix: SMB's tier is (world, level) and the banded
  progress stays in the place; by the same rule Mega Man 2's tier is the
  bosses cleared alone and boss damage stays in the place. Nova, Smash and
  faults already follow that rule. The second build (consol2) reruns the
  three SMB cells before the ladder.
- 2026-09-22: the second build (consol2) reached world 8-1 on the first
  SMB seed at 1.18 million executions (the old build: 515 thousand) and
  had not left 8-1 by the wall limit at 1.96 million. Its heatmap shows
  8-1's draws spread evenly over 7,700 live places, about forty each, with
  the frontier place drawing no more than the start of the level. The old
  build reset a group's draw count whenever a selection from it opened a
  new group, so the edge of explored ground kept drawing. The new archive
  reset only on a preference improvement. Fix: a cell's draw count also
  resets when a selection from it opens a cell that held nothing. Third
  build (consol3) reruns the SMB cells.
- 2026-09-22: the third build (consol3) reached 8-1 at 1.04 million on the
  first SMB seed and had 8-1 four fifths crossed at the two million limit.
  Its 8-1 heatmap spreads the draws over 7,700 live places in proportion to
  how many places each stretch of the level holds; the last sixty units of
  progress drew seven percent. With a decay of one over one plus the count,
  a freshly reset cell among thousands takes only a small share until it
  catches up. Fix: the count decay exponent is two, so a fresh or freshly
  reset cell takes most of its tier's draws until it catches up. Selector
  identifier v2. Fourth build (consol4) reruns the SMB cells.
- 2026-09-22: the fourth build (consol4) reached 8-1 at 543 thousand on the
  first SMB seed, the old build's pace, and then spent 1.46 million
  executions inside 8-1 without leaving. Its 8-1 heatmap gives each stretch
  of the level a share of draws in proportion to how many places the
  stretch holds; the last forty units of progress hold two hundred places
  and drew under two percent. With a place per unit of progress, a fresh
  cell's draws are spread over its neighbours as soon as it opens. Fix: the
  SMB place is the screen of the level (sixteen units of progress) and the
  room; the position within the screen, the height band, the clock band
  and the state fingerprint move to the holder identity. The cell count of
  a level falls by an order of magnitude and a freshly opened screen keeps
  its draws. Fifth build (consol5) reruns the SMB cells.
- 2026-09-22: the fifth build (consol5) reached only world 5-2 on the first
  SMB seed before the frame limit at 1.6 million executions. With the
  screen as the place, an advance inside a screen opens no cell and resets
  nothing, so the advancing holder competes evenly with the hundreds of
  holders its screen already has. Fix: the position within the screen is
  the SMB preference (one order, capacity two), so an advance displaces the
  shallower holder of its slot and resets the cell's draws, the same
  mechanism Metroid uses for tanks, missiles and health. Sixth build
  (consol6) reruns the SMB cells.
- 2026-09-22: the sixth build (consol6) never left world 1-1 on the first
  SMB seed in 1.4 million executions. A preference on the position within
  the screen keeps only the rightmost holders of each slot, and the states
  behind them that a jump needs are displaced. The screen place and the
  position preference are both withdrawn: the SMB key returns to the fourth
  build's, a place per unit of progress. The fourth build's 8-1 heatmap
  says a freshly opened cell takes about fifteen draws before the count
  decay levels it with the thousands of older cells, and the level needs
  more per cell. Seventh and eighth builds (consol7, consol8) rerun the
  first SMB cell side by side with a count decay exponent of three and of
  four; the one that clears 8-1 at the old build's pace runs the other
  two cells.
- 2026-09-22: exponents three and four (consol7, consol8) reached 8-1 on the
  first SMB seed at 711 thousand and 553 thousand and both stalled inside
  8-1 at the wall limit, at progress 280 and 322. With the fourth build
  (543 thousand, stalled at 321) and the third (1.04 million, stalled at
  316) that is four builds reaching 8-1 at about the old build's pace and
  none leaving it; the old build left at 1.28 million. The exponent stays
  at two. Next: film the deepest 8-1 tape of the fourth build (a
  diagnostic copy that keeps the deepest tape in witness mode) and name
  what the frontier needs.
- 2026-09-22: the fourth build's 8-1 draw table, read by room, time band
  and progress: the frontier stretch of the main level (progress 300 to
  327) took under two percent of 8-1's draws, every frontier cell has
  under a hundred seconds on the clock, and the deepest cells sit two
  screens before the level's widest pit, which needs a running start
  from those cells. The old build's band rank gave the frontier stretch
  about half the level's draws. Under count decay a stretch's share is
  its cell count, and the only persistent gradient in the design is the
  tier rank. Fix: the SMB tier is (world, level, progress band of sixty
  four units, four screens); the place keeps the unit progress. The
  first build's band was four units, fine enough for the top eight
  bands to take every draw; four screens keeps a level's branch points
  within two ranks of its frontier. Ninth build (consol9) runs the three
  SMB cells.
- 2026-09-22: the ninth build (consol9) reached 7-4 on the first SMB seed
  at 382 thousand executions, faster than every other build including the
  old one, then spent 1.3 million executions inside 7-4 and ended at two
  million in 8-1 at progress 336. 7-4 is a maze: a wrong turn keeps the
  band of the frontier and loops back, so with eight times per rank the
  looping states in the leading band take almost every draw while the
  branch point one band behind gets an eighth. The old build ranked bands
  at two to one. Fix: the key supplies its tier rank shift; the default is
  the same eight to one, and the SMB key halves per band. Tenth build
  (consol10) runs the three SMB cells.
- 2026-09-22: the tenth build (consol10) crossed 7-4 in 380 thousand
  executions and reached 8-1 at 790 thousand on the first SMB seed, then
  stayed in 8-1 to the two million limit at progress 333. Every build of
  the branch stops in 8-1 between progress 316 and 336; the old build
  left 8-1 after 770 thousand executions there. Next: film the deepest
  8-1 tape of the tenth build (diagnostic copy, first seed) and name what
  the stretch after progress 330 needs.
- 2026-09-22: film of the tenth build's deepest 8-1 tape (first seed): the
  tape reaches the frontier stretch with the clock at one and dies to the
  timer; the last fifty seconds of clock cover the final two screens of
  progress. Every frontier cell of 8-1 sits in the lowest clock band, and
  the faster band's deepest place is a hundred units behind. The old build
  drew the cheapest tape of a group first; the new archive keeps the
  cheapest holders of a slot but draws slow and fast places alike. Fix:
  the SMB preference is the game clock, so an arrival with more time left
  displaces the slower holder of its slot and resets the cell, the same
  mechanism Metroid uses for health; the clock band stays in the place.
  Eleventh build (consol11) runs the three SMB cells.
- 2026-09-22: the eleventh build (consol11) stalled in 8-1 at progress 330
  on the first SMB seed, and its 8-1 draw table shows the faster clock
  bands no deeper than before. The SMB holder identity was a six bit hash
  of the whole RAM, so a faster arrival at a place almost never lands in
  the slot of the slower holder it should displace, and the clock
  preference never fires. Fix: the SMB holder identity is the screen
  position alone; the hash leaves the key. Twelfth build (consol12) runs
  the three SMB cells.
- 2026-09-22: the twelfth build (consol12) solved the first SMB cell at
  1,058,642 executions (old build: 1,430,664), reaching 8-1 at 715
  thousand and leaving it 57 thousand later. The other two SMB seeds and
  then the Metroid ladder run on this build.
- 2026-09-22: at the other session's request, a thirteenth build
  (consol13) ran the level alone as the SMB tier with every later fix in
  place (reset on open, squared decay, the game clock preference, the
  screen position identity, no band, the default eight to one ratio). It
  failed all three SMB cells at two million executions: seed 20260905
  ended in 8-2 at progress 145, seeds 20260906 and 20260907 in 8-4 at
  273 and 276. Each level took longer than under the band tier; 8-1 took
  350 to 415 thousand executions against 56 to 102 thousand on the twelfth
  build. The band tier and the per-key ratio stay; the reason is written
  beside the setting in the design section.
