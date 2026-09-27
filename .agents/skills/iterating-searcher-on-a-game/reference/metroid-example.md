# Metroid worked example

Metroid is the game this process was built on. Its workload is
`workloads/nes/src/metroid`, and its README lists the milestones, the key and
the census. This file shows what each step looks like on Metroid.

## Contents

- Milestones and hard parts
- What rooted segments show
- What the game reading must handle
- The handoff at Tourian
- Mechanisms a change must respect
- Applying this to another game

## Milestones and hard parts

The order a power-on run passes through, with the hard parts marked:

| Stretch | Milestones | Hard part |
| --- | --- | --- |
| Brinstar | `morph_ball`, `bombs`, `long_beam`, `brinstar` | |
| Kraid | `kraid_area`, `kraid_door`, `kraid_room`, `kraid_defeated` | Kraid: the fight needs missiles or bombs, and his hideout is a deep dead end to climb back out of |
| Norfair and Ridley | `norfair`, `high_jump`, `ridley_area`, `ridley_room`, `ridley_defeated` | Ridley, then the walk back to Brinstar |
| Statues | the statue raise at Brinstar (4,2) counts as an item | The raise opens Tourian's entrance |
| Tourian | `tourian`, `tourian_corridor`, `tourian_far`, `tourian_bottom`, `tourian_approach`, `tourian_end` | Tourian: Metroids, Rinkas and missile-breakable blocks drain energy and missiles on a long route |
| Mother Brain | `zebetite_destroyed`, `mother_brain_room`, `mother_brain_defeated` | Five Zebetite columns, then Mother Brain |
| Escape | `escape_started`, `ending` | |

`benchmarks/search/ladder.py` holds the milestone list the ladder scores, from
`brinstar` to `ending`. The Tourian route milestones give a stall a named place;
they change no key or draw.

## What rooted segments show

A chain of 25 rooted segments, each rooted at the tape of the milestone the
previous segment passed on two of three seeds, reaches the ending from a state
just after the Kraid kill. The chain shows three things:

- **A best-stocked root passes where a first arrival stalls.** The first
  arrival at a Tourian room holds a fraction of the energy and missiles that
  other arrivals at the same room hold, and most searches rooted there die. The
  workload writes `NAME-energy.json`, `NAME-missiles.json` and `NAME-boss.json`
  so a segment can root at the best-stocked arrival.
- **A fresh archive re-explores old ground.** A segment rooted at the Brinstar
  elevator spends 57% to 89% of its draws in Kraid's hideout, which the root
  has already left.
- **A chain is picked by hand.** Each root is chosen from the tapes, checked
  against the bytes and joined onto the inputs before it. A single power-on run
  has to pick and carry the state itself, which a chain never tests.

A segment's budget must exceed the slowest pass of that part in any earlier
run. Rooted starts leave Kraid's hideout 1.0M to 1.45M tries after the kill,
and power-on runs take 2.3M to 3.9M, so a 1M budget stops segments that would
pass.

## What the game reading must handle

Stalls in segments often come from the workload misreading the game. Film the
stall, then read the RAM bytes with `metroid-map-probe`. Metroid's reading
handles these cases, and a new game's reading needs the same checks:

- **Flags with more than one value.** Raising the Brinstar statues rewrites
  both boss bytes to `0x82`. The reading counts bit 7 as the defeat and each
  raised statue as an item; otherwise the raise looks like losing a boss and
  the raised state has the same key as before, so the archive never keeps it.
- **Progress the key must see.** Mother Brain's hits are the Tourian boss
  reading, and the Zebetite hits still needed are part of the place, so each
  hit on a column or on her opens a new place.
- **Readings that blink or get reused.** The frame after a hit, Kraid's slot
  shows a flash value, and after Samus leaves the room an ordinary enemy
  reuses the slot. Both read as zero health, which a key scores as a kill. The
  reading holds its last value across flash frames, clears it when the map
  cell changes, and takes a kill only from the game's defeat bits. Mega Man
  2's dragon reading showed the same defect.
- **Readings that change with the screen.** Each Tourian screen loads its own
  Zebetite slots on entry, so a sum over columns rises when Samus changes
  screens. Mother Brain's own hit byte only rises, and the reading uses it.
- **Milestones that latch on a death.** A first-arrival tape can end on an
  action where Samus dies. Root only at living endpoints; the best-stocked
  tapes are always living endpoints.

## The handoff at Tourian

In the example baseline, power-on runs passed Kraid and Ridley and stalled in
Tourian. The handoff comparison showed why:

- Power-on lines enter Tourian with 7 to 94 of 299 energy.
- Full-energy states exist one item tier lower, in Ridley's area, millions of
  tries before the first Tourian entry, and none is an ancestor of a Tourian
  state.
- The last item before Tourian is the statue raise, about 100 actions from
  Tourian's door. The new top tier starts at Tourian's mouth with whatever
  energy its first state had, and the full-energy states stay one tier down.
- Rooted segments from stocked Tourian tapes pass, so stock is what Tourian
  needs.

The gap is carrying a stocked state to the tier rise. That carry has to be
searched: at the same position and pose, two lineages differ in the frame
counter, the random bytes and about 130 other RAM bytes. One lineage's
recorded actions replayed from another mistime jumps and meet different
enemies within a few actions. About 30 such joins of higher-energy states
onto the Tourian route entered Tourian with at most 33.5 energy.

## Mechanisms a change must respect

- A key term that splits a place by resource level makes each drained arrival
  at a stocked place a new place, which spreads draws over drained states.
- A missed continuation replayed again from the same state misses again,
  because a replay is exact. Random actions in front of the retry change the
  timing, but a Metroid miss comes mostly from the state being replayed from.
- A higher tier takes most draws. When the top tier's states cannot finish a
  fight, the lower tier that could farm for stock gets few draws.

## Applying this to another game

For Mega Man 2 the workload is `workloads/nes/src/mm2`. Start at step 1 of the
skill: list its milestones in order, mark the hard parts, and check that the
workload supplies each item in that step. Where it lacks one, such as a
whole-game mode, best-stocked tapes or checkpoint support, that is workload
work in its folder. The Metroid workload's reporting and tapes are the model to
copy.
