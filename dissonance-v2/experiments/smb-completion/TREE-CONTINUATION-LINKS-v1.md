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
12023843674612131264. Measures the checkpointed bootstrap and gives the
search the Down button.
