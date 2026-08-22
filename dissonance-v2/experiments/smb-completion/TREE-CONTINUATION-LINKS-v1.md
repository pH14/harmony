<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Whole-tree continuation links from World 8-4 progress 153

Campaign-mode runs on `msr1`, 12 workers, 50,000 executions each, key
`frozen_rooms`, selector `concentrated_recency_128`, retention
`probe_at_admission`, suffix `one_or_two`. These are mechanism runs, not
sealed claims.

## Link 1 (source eeb5c3d6, root `/root/harmony-smb-campaign-tree-2048c9ee`)

Origin: a two-entry archive wrapping the adopted progress-153 input
(`results/origin-p153.json`, SHA-256 `fd96ddca…`), resume `frontier_shortest`.
Seed 3100073064080082122. Stream SHA-256
`9c6635b8f2e72a59f0f9a1eefa621f92726af42f453585c5a5cbcc5d68d474ba`.

- Bootstrap retained 3,164 boundaries in 20 minutes on one core.
- 50,000 executions at 14.4 per second; 30,468 entries; watermark (7, 3, 153).
- 20,790 entries in 8-4: 11,800 on page 9, 4,548 on page 5, 4,177 on page 6.
- One pipe was taken: entry 27835, parent at progress 73 (page 4), child at
  progress 17 (page 1). Its key showed no novelty (`rooms` 1, room bytes
  unchanged) and the selector never drew it again.

## Link 2 (source b5f62ae0, root `/root/harmony-smb-campaign-tree-b5f62ae0`)

Resume `whole_tree` from link 1's archive under a room identity that added
the entrance-mode byte 0x0752. Stopped by the operator at 10,149 executions:
the byte is set only during the entrance animation and is zero in every
settled snapshot, so the post-pipe entry imported as `rooms` 1 and the run
behaved like link 1. `results/link2/STOPPED.txt` records the stop.

## Link 3 (source 95bd7863, root `/root/harmony-smb-campaign-tree-95bd7863`)

Resume `whole_tree` from link 1's archive under the arrival-page room
identity (area bytes plus the level page the lineage arrived at; an arrival
is a child more than one screen behind its parent inside one level). Seed
3567950349743117301. Stream SHA-256
`8e73ce954bc4f103b15bad0438c5e6215c61ec61d52f9d2fcd54c729e4efdb2a`.

- Import rebuilt all 30,468 entries in 45 minutes on one core.
- 50,000 executions at 10.3 per second; 62,817 entries; watermark still
  (7, 3, 153).
- 8-4 entries by `rooms`: 1: 12,295; 2: 10,549; 3: 11,007; 4: 13,981.
  Rooms 3 and 4 each reach progress 153 again.
- Room arrivals inside 8-4, from parent page to child page: 9→5 (3,727),
  9→6 (2,503), 9→7 (9), 4→1 (1), 8→5 (1), then 9→5 and 9→7 again from
  rooms 2 and 3. The progress-153 wall is a loop that returns the player to
  page 5, 6, or 7 depending on where the loop triggers; 6,239 of those
  returns were already in link 1's tree as `rooms` 1 entries on pages 5–6.
- The first tie-class draws retained 85–90% of the time; retention settled
  near 50% per 1,000 draws and no class was ever skipped.

## What the links show about the searcher

- Whole-tree continuation works and keeps every lineage, at the cost of a
  45-minute single-core import and a 16 GB pretty-printed archive.
- Counting distinct arrivals in the key rewards a loop: every return at a new
  page is a new room, the selector ranks it above everything else, and the
  budget re-walks pages 5–9 once per arrival page. The single pipe room
  (arrival at page 1) ranks no higher than the loop rooms and gets no more
  draws than they do.
- A room should be a cell coordinate, not a count: with the room identity
  in the key, each room is explored once and repeated loops add nothing.
  Which room to concentrate on is then a selector question; splitting draws
  evenly across the rooms of the deepest level, and concentrating at each
  room's own frontier inside it, gives the pipe room a fixed share.

## Link 4 (source 3869dde5, root `/root/harmony-smb-campaign-tree-3869dde5`)

Resume `whole_tree` from link 1's archive with key `frozen_room` (the room
identity as a key coordinate, `rooms` count unused) and selector
`room_uniform_128` (frontier draws split evenly across the rooms of the
deepest pair, frontier-band walk and recency window inside the room). Seed
18146079665610312082. Stream SHA-256
`d2ab1018d001847b1251bba78ec5401101945d3dbbe6c48ba513806d723d1d10`.

- 50,000 executions at 10.4 per second; 57,582 entries; watermark still
  (7, 3, 153). No room was ever exhausted; room-draw retention fell from
  58% to about 35% by the end.
- 8-4 rooms (area bytes, arrival page): entries, of which new this link,
  best progress, pages held:
  - `[3 5 0]` start room: 13,633, 1,714 new, best 153, pages 0–4 and 7–9.
  - `[3 5 5]`: 11,511, 6,255 new, best 153, pages 5–9.
  - `[3 5 6]`: 9,020, 5,417 new, best 153, pages 6–9.
  - `[3 5 7]`: 4,015, 4,004 new, best 153, pages 7–9.
  - `[3 5 1]` pipe room: 4,220, 4,219 new, best 72, pages 1–4.
- The start room holds no page-5 or page-6 states: the source lineage left
  page 4 through a pipe and came out on page 7. Pages 5–6 entered the tree
  only through the loop return.

## What link 4 shows

- Room as a coordinate removes the loop reward: repeated returns add no
  room, and the pipe room receives a steady fifth of the frontier draws.
- Three of the five rooms cover the same pages 5–9 and end at the same
  wall; an even split spends three fifths of the budget on them. Rooms
  whose cells overlap are one room arrived at three ways; the identity is
  too fine, or allocation should follow retained yield per draw instead of
  a flat share.
- Throughput is the limit that matters now: 10 executions per second on
  12 workers is about 215 emulated frames per second per worker, far below
  what one emulator core does, so the coordinator or per-job overhead, not
  emulation, bounds the budget.

## Throughput measurements (after link 4)

- The emulator is the per-frame floor: tetanes-core clocks the PPU per dot
  (about 12 ns per dot), 590 frames per second per core on the ARM box and
  905 on the Mac. Restore, snapshot, hashing and serialization cost under
  0.15 ms per job. Fat LTO with one codegen unit gains 1%.
- Link 4's 80 minutes were 43 minutes of single-threaded whole-tree import,
  37 minutes of jobs at 22.5 executions per second (398 frames per second
  per worker, 67% of twelve cores), and one minute writing the archive.
  The 10.4 per second figure in link 4's record averaged the import in.
- On the Mac, 8 workers reach 87% of emulation capacity, so the
  coordinator is not the limit there. The ARM box runs 13 threads on 12
  cores; an 11-worker pilot is pending.
- The origin archive (14.7 GB of JSON) parses in about 80 seconds.

## Link 5 (source cf3697fd, root `/root/harmony-smb-campaign-tree-cf3697fd`)

Same policies as link 4, resumed `whole_tree` from link 4's archive. The
source commit adds the snapshot checkpoint: every run writes
`snapshots-live.bin` (each retained entry's emulator snapshot), and
`--checkpoint <file>` lets a whole-tree resume restore entries instead of
re-emulating them. Seed 18191268080075492392. Stream SHA-256
`58cc536dd691a24a…`, checkpoint SHA-256 `0024cd71c3d48f05…` (332 MB for
77,970 entries).

- Import: 57,581 entries, none rejected, 75 minutes (2.66 M frames).
- Jobs: 50,000 at 21.8 per second; 77,970 entries; watermark (7, 3, 153).
- Draws per room were even (8.5k–9.8k each) and no room exhausted. The
  pipe room `[3 5 1]` took 8,831 draws, added 2,512 entries, and stayed at
  progress 73: it is a loop as well (page 1 to page 4, then the same pipe).
- Every link so far ran the default `frozen_nine_mask` vocabulary, which
  has no Down button. Pipes entered from above were unreachable for all
  250,000 executions. The `down_ten_mask` vocabulary already exists.

## Link 6 (source cf3697fd)

Resume `whole_tree` from link 5's archive through its checkpoint, with
vocabulary `down_ten_mask`; key and selector unchanged. Seed
12023843674612131264. Stream SHA-256 `f755edb421efabf9…`, checkpoint
`2b892c98fd76e20f…` (468 MB).

- Checkpoint bootstrap: about 35 seconds for 77,970 entries; the 20 GB
  origin parsed in about two minutes. Whole run 40 minutes wall, 50,000
  executions at 20.7 per second including bootstrap.
- Watermark (7, 3, 233) after 6,318 executions; 110,305 entries.
- 8-4 rooms (entries, new this link, best, pages):
  - `[3 5 0]` start: 18,816, 3,094, 233, pages 0–4, 7–9, 12–14.
  - `[3 5 1]`: 9,411, 2,679, 152, pages 1–4, 7–9.
  - `[3 5 5]` / `[3 5 6]` / `[3 5 7]`: 20,007 / 15,664 / 9,768 entries,
    best 233 / 233 / 232, all also reaching pages 12–14.
  - `[3 5 10]` / `[3 5 11]` / `[3 5 12]`: 4,743 / 5,358 / 686, all new,
    best 233, pages 10–14.
  - `[0 2 0]` water area: 999 entries, best 56, pages 0–3; entered 14
    times from page 14 of the main area.
- Transitions: pages 10–11 are skipped by a forward pipe from page 9 (not
  an arrival under the room rule, since the screen moves forward); the
  page-14 wall is a loop returning to pages 10–12; its exit is the water
  pipe at page 14, which needs Down.
- Every same-area room (`[3 5 1/5/6/7/10/11/12]`) walks pages the start
  room already holds. The even split gives the water room one share in
  nine.

## Storage and the entry ceiling (source 556103c3)

Link 6's archive was 29 GB and its report 32 GB because each entry carried
its full input. Entries now serialize the actions past their parent
(`input_suffix`) and are rebuilt on load; archives with full inputs still
load. The archive ceiling rises from 131,072 to 1,048,576: link 6 ended at
110,305 entries, and at the ceiling every candidate is refused.

## Link 7 (source 556103c3)

Resume `whole_tree` from link 6 through its checkpoint, vocabulary
`down_ten_mask`, key `frozen_room`, selector `room_uniform_128`. Seed
11819261505613539993. First run writing the suffix archive.

Result: stream SHA-256 `7844936b21108135…`, checkpoint `53fc101f2da05119…`.
Archive 92 MB (link 6's was 29 GB). 133,959 entries; watermark (7, 3, 304)
after 14,562 executions.

- The water room `[0 2 0]` exits at its page 3 into the main area at page
  16 (86 such transitions), opening room `[3 5 16]`: 1,285 entries, best
  304 (page 19), pages 16–19, 3,374 draws of about 46,000 room draws.
- Nine of the ten 8-4 rooms walk the same pages 0–14.

## Link 8 (source 35bace58)

Key `frozen_area`: the room changes only when the area bytes change, so a
same-area loop return stays in the room it left. Resume from link 7 through
its checkpoint, selector `room_uniform_128`, vocabulary `down_ten_mask`.
Seed 9678475058303591893. Stream SHA-256 `65bc5655ce821581…`, checkpoint
`3d9ce7d04d7bf5a3…`.

- Import: 97,282 kept, 36,676 refused as the nine same-area rooms folded
  into one; 41 minutes wall for the whole link.
- Three rooms: main `[3 5 0]` (71,111 entries, pages 0–14, 18,390 draws),
  water `[0 2 0]` (10,158, pages 0–3, 13,324 draws), after-water
  `[3 5 16]` (1,979, pages 16–19, 12,792 draws). Watermark (7, 3, 304).
- The after-water room spent 12,570 of its 12,754 draws on the band
  296–304 and retained 160 there; 15,572 candidates were refused. No
  death was recorded. p304 is a loop returning to page 16, whose landing
  cells are full, so returns are refused and the room's other pages
  (256–295) received 184 draws. A trickle of new tip fingerprints kept the
  band unexhausted, so the deepest-band walk never fell through.

## Link 9 (source 35bace58)

Same as link 8, resumed from link 8's checkpoint; seed
13993258458984251027. Measures whether more budget alone moves p304.

## Link 10 plan (source f218b79e)

Selector `room_band_uniform_128`: the room is drawn uniformly as before,
then one of the room's progress bands (8 wide) that still holds an
unexhausted entry, uniformly, then the concentrated draw inside it. Each
page of the after-water room gets an equal share instead of the tip taking
98%. Resume from link 9 through its checkpoint.
