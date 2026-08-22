# What the searcher needs to complete Super Mario Bros in one attempt

Sources: LAB-LOG.md, the 41 SOL-*.md records, TREE-CONTINUATION-LINKS-v1.md,
PICKUP-PLAN.md, NOTES.md, 395 commits on `dissonance-v2` (2026-08-08 to
2026-08-22), 17 Claude session transcripts and about 30 Codex session
transcripts that touch the search. Numbers are taken from those records.

Terms. A cell is the archive key `(world, level, progress, y bucket, engine
state, 6-bit RAM fingerprint)`; progress is `page*16 + x/16`. An execution is
one parent draw plus one appended chord (button mask held for N frames) plus
an admission attempt. A link is one 50,000-execution campaign resumed from
the previous one.

## What it took

- About 5.2 million executions over 12 days (2026-08-10 to 08-22): roughly
  2.0M in preregistered panels (H1–H59), 2.3M in the campaign chain
  (C49–C113), about 0.35M in C114–C120, 0.26M in the sealed harvests, 0.5M in
  tree links 1–12.
- About 90 resume boundaries (55 chain links, 22 harvest adoptions, 12 tree
  links).
- The winning input: 3,704 chords, 175,374 frames (48:43 of play), first
  reached at link 7 execution 14,466 on 2026-08-22.
- Throughput: emulator-bound at ~600 frames/s/core (msr1), 10–22
  executions/s on 12 workers.

## Why it took many attempts

Every wall falls into one of six classes. Counted over the whole program:

| Class | Walls | Executions spent before the fix |
|---|---|---|
| Missing input vocabulary | hold durations (1-2); Down absent at 4-2; Down absent again at 8-4 (links 1–5 and seven sealed experiments) | ~400k |
| Decoder defects | level number during the flag task; death not detected (engine 8→6→0); no victory decoder (links 8–12 searched past a won game) | ~450k |
| Key/coordinate defects | no player-x inside scroll-locked rooms (4-2); loop rooms aliased (7-4, 8-4 page-10 and page-14 loops); arrival-page rooms rewarded loops; same-area pipe landings opened false cells | ~600k |
| Selector starvation | deepest band takes 98% of draws; the same diagnosis was made four times (H58, C86, link 8, link 11) and fixed four times | ~500k |
| Capacity and continuation | action cap 512 then 4096; archive cap 32,768; 12–29 GB JSON archives; resume keeping one lineage (C103, C111, links 1–4); 43–75 min imports | ~300k |
| Level clock | 8-1: inherited prefix spent 7,279 of ~7,200 frames; needed entrance search and fewest-frames replacement | ~600k |

The remainder went to hypotheses that were null on their own terms (model
rankings and mutators, bursts, bands, ancestry, coherent splices, long
rollouts, sibling forks) and to replay gates.

Three things broke walls that were not defect fixes: mined chord tables
(7-4 last check, 8-1 gap), a depth-8 blind bridge (8-4 page-10 loop), and
restarting the search from the exact tip with single chords (8-1 p236 to
8-4 p153). The first two were never made continuous; the third is the base
mechanism.

## What the final mechanism is

Snapshot archive with two entries per cell; single-chord extensions from
restored snapshots; holds stratified 2–12 / 96–120 frames; death decode
`$000e == 0x0b || $00b5 >= 2`; 45-frame admission probe under three masks;
ten button masks including Down; `frozen_area_span` rooms as a key
coordinate; selector room-uniform, then band-uniform, then cell-uniform,
then recency-concentrated; 12 parallel workers; whole-tree resume from a
binary checkpoint.

With that configuration in place from the start, every wall in the table
above except two would not have occurred. The two are the level clock in
8-1 and the multi-step regression at 8-4 page 10.

## Necessary refinements, in order

1. **Victory decoder.** `$0770 == 2` read after each action, a terminal
   event that stops the run and writes the input. Without it the run cannot
   end. One afternoon.

2. **Full vocabulary from execution zero.** The ten masks (`down_ten_mask`)
   as the only default; no run may start under a nine-mask set. Start and
   Select excluded (the pinned-window experiment paused the game on 0x08).
   Trivial.

3. **Time as a per-cell objective.** Replacement rule `fewest_frames_in_level`
   as the default, and frames-in-level carried as a cell cost so the
   cheapest entry survives. The 8-1 wall was a clock wall: three policies
   converged at p351 with 7,260 frames spent. This also removes the need for
   entrance search from a derived origin, which was level-specific in
   practice. Already exists as an option; needs to be the default and needs
   a measured run.

4. **Mechanical scroll-lock and room detection.** `frozen_room_x_16` was
   registered per level (`3,1,208`); it must trigger from the emulator
   state instead (scroll column not advancing while player x changes, or
   area byte change). `frozen_area_span` already handles same-area loops and
   pipe landings. One general key rule instead of three registrations.

5. **Equal-effort selection as the only selector.** Room, band, cell
   uniform, then recency inside the cell. Link 12 verified it distributes
   40–77 draws per cell in thin regions instead of 0–2. The four repeated
   starvation diagnoses were one defect. Delete the other selectors.

6. **Multi-chord extension with a small probability.** The 8-4 page-10 loop
   needed eight blind chords because every single chord regressed four
   pages and the landing cells were full. Sol's fixed-horizon rollouts
   (H=2..32) failed, so the evidence is mixed; the cheapest test is a
   1-in-8 draw of 2–8 chords inside the existing campaign, measured on a
   known loop (7-4 or 8-4 from link 5's checkpoint). This is the one open
   question.

7. **Mined chord tables as continuous machinery.** Two walls fell to chords
   copied from recorded history. The C120 defect (table re-hashed per draw,
   26 → 2.7 executions/s) is fixed in the unification branch but was never
   run live. Make it a fixed fraction of draws, not a separate link.

8. **No resume boundaries.** One process, one budget, binary checkpoint
   every N executions, whole tree kept. The link concept caused the lost
   population at C103/C111 and links 1–4. Already built; the run must be
   launched once.

## What a single attempt would cost

At 20 executions/s on 12 cores, the recorded productive segments add to
roughly 0.5–1.0 million executions when no wall is caused by a defect
(link 6 to link 7 needed 20,000 executions for all of 8-4 once Down was
present; the sealed harvests needed about 100,000 for 8-1 p236 to 8-4 p0).
That is 7–14 hours on msr1. Matching a 45-minute run needs either 10–20×
the cores or a cheaper execution (the 45-frame probe under three masks is
~135 of the ~265 frames per execution; probing only on cells that would be
retained halves the cost).

## What was not the searcher

About a third of the wall-clock went to process: replay gates (2.5–4 h
each), sessions lost to compaction and a tmux restart, stale binaries,
unpreregistered promotions that moved nothing (the Luna phase, 19k lines
reverted), per-level mechanisms (pins, waypoints, registered rooms) that
each cost a link, and vocabulary churn. None of that changes the searcher;
it changes how runs are launched and judged.

## The test

One run, from the gameplay genesis snapshot, 12 workers, budget 1.5M
executions, items 1–5 and 8 on, items 6 and 7 at fixed draw fractions,
stop on the victory event. Success is the victory event; the record is the
executions-to-victory curve per level, which becomes the benchmark for any
later change.
