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
`b1f3…` (see `results/link3/stream.jsonl`).

- Import rebuilt all 30,468 entries in 45 minutes on one core.
- 50,000 executions; 62,817 entries; watermark still (7, 3, 153).
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
