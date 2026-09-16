# Pokémon Blue in dissonance, with Jev as adviser

Five steps, built one after another, each starting from the merge commit of
the step before. The goal is to reach the first gym badge (Brock, Pewter City)
from a new game, first with the plain searcher, then with the TypeSafe Jev
decision model weighting the input draw. Steps 1 to 3 need no API key.

Everything the searcher needs from the game is in Game Boy work RAM. The
pokered disassembly (https://github.com/pret/pokered, `wram.asm`) gives every
address. Read state from RAM only. Never decode the screen.

## Inputs

| Input | Where | Notes |
|---|---|---|
| ROM | `~/Downloads/pokemon-blue.gb` on Paul's Mac, copy to any lab box | SHA-256 `2a951313c2640e8c2cb21f25d1db019ae6245d9c7121f754fa61afd7bee6452d`; 1 MiB; add under key `blue` in the private assets inventory the way `benchmarks/search/README.md` describes |
| Emulator core | Gambatte libretro, https://github.com/libretro/gambatte-libretro | pin one revision in `scripts/build-gambatte-core.sh`, copied from `scripts/build-quicknes-core.sh` |
| TypeSafe key | `TYPESAFE_API_KEY` in `~/.zshrc` on Paul's Mac | copy to ms02 and msr1 freely; only step 4 and 5 read it |
| Jev API | the `typesafe-jev-model` memory note, then https://docs.typesafe.ai/llms.txt | one endpoint, three question types, no generation, no images |

## Order

| Step | What is built | Check after |
|---|---|---|
| 1 | `workloads/gb-machine`: Gambatte libretro driver | loopback and real-core tests, determinism check |
| 2 | `workloads/gb`: Pokémon Blue workload with macro actions | new-game to Brock reachable by a scripted tape, replay exact |
| 3 | plain campaign, three seeds on ms02 | film, milestone table, stalled archives saved |
| 4 | Jev offline test on the step 3 archives | picks compared against the known route; stop if no better than the searcher's own ranking |
| 5 | Jev weighting the alphabet draw, recorded in the stream | paired three-seed run against step 3 at matched executions |

### Step 1: Game Boy driver

Copy `workloads/nes-machine/src/quicknes.rs` into a new standalone crate
`workloads/gb-machine` and change what differs. The libretro symbol set is
the same. Differences:

- `retro_get_memory_data(RETRO_MEMORY_SYSTEM_RAM)` returns 8 KiB of work
  RAM at `0xC000`. Expose it as the RAM window. Gambatte also exposes
  cartridge SRAM as `RETRO_MEMORY_SAVE_RAM`; the workload does not need it.
- Input is one 8-button joypad. Keep the `[buttons, hold_frames]` action
  format from `nes.rs` with the Game Boy button set.
- Pokémon Blue has no real-time clock, so serialize and restore are
  byte-exact. Test that running N frames, snapshotting, running M more,
  restoring, and running M again gives identical RAM and identical
  serialized state.
- Snapshot the core hash and revision into the snapshot header as the NES
  driver does, and reject a different core.

Audio and video off during search. Keep the frame capture path for film.
Measure frames per second headless on ms02 and put it in the crate README.

### Step 2: Pokémon Blue workload

Standalone crate `workloads/gb`, structured like `workloads/nes/src/metroid`:
`target.rs` (RAM constants and decoding), `archive.rs` (key), `campaign.rs`
(alphabet, run policy), `progress.rs` (milestones), `README.md`, and binaries
`blue-campaign`, `blue-film`, `blue-probe`. Register it in
`scripts/check-dependency-boundaries.py` and the workspace lists.

**Observation.** Decode from work RAM: current map id, player x and y, facing
direction, badges byte, party count, each party member's species, level, HP
and max HP, bag items and counts, money, the event flag block, whether a
battle is active and its kind, the active text box and menu state, and the
current map's warp table. Document each address beside its constant.

**Key.** The archive key is position and permanent holdings only, following
the Metroid rule (memory notes `resources-must-be-key-coordinates` and
`tanks-belong-in-preference-not-cell`). Groups from coarse to fine: badges,
then key event flags reached (a fixed list: got starter, got Pokédex, got
Oak's parcel, delivered parcel, entered Viridian Forest, entered Pewter,
entered the gym), then map id, then the 4-tile cell of the player. Preference
within a slot: fewer actions to reach it, then party total HP, then party
levels.

**Milestones.** The seven event flags above plus the Brock badge, in order.
Each is a `Milestones` bit. Victory is the badge.

**Terminal.** A whiteout (party HP all zero in battle, or the whiteout
event) is terminal. Never admit it.

**Actions.** Every action is a macro the target expands into button presses
by reading RAM between frames. Six types:

| Macro | Parameter | Expansion |
|---|---|---|
| walk to | a warp or a tile on the current map | A* over the map's collision data, one D-pad hold per tile, stop early on a battle or a text box |
| interact | none | A facing the tile ahead, advance text until the box closes |
| battle move | slot 1 to 4 | FIGHT, pick the slot, hold through the turn |
| use item | bag slot | ITEM menu, pick, confirm |
| switch | party slot | PKMN menu, pick |
| flee or advance text | none | RUN in a wild battle, otherwise mash B until control returns |

The alphabet the draw samples from is generated per state from the
observation: in battle, the four moves plus switch and flee and a few usable
items; out of battle, every warp on the current map, every interactable tile
within a small radius, and a random walk of a few tiles. `sample_alphabet`
picks uniformly from that list. Action cost is frames spent.

**Setup prefix.** A scripted tape from power-on through naming to standing in
the bedroom, checked in as a fixture like the SMB power-on fixture. Also
write a scripted tape from the bedroom to the Brock badge, stored under
`workloads/gb/fixtures`, and a test that replays it and sees the badge. That
tape proves the macros and the RAM map before any search runs.

**Film.** `blue-film` renders a tape to MP4 the way `metroid-film` does.

### Step 3: plain campaign

Add a `blue` case to `benchmarks/search/pilot.json` with a new-game origin,
and run three seeds, eight workers, 2048 MiB, a 30-minute wall limit each, on
ms02. Judge by the README rules for the searcher plans: film the deepest
witness per seed and write one sentence each, then read the milestone table.
Save each run's end-of-run report and the live archive entries; step 4 reads
them. If no seed reaches Brock, diagnose from the film before step 4, since
the goal of steps 4 and 5 is to speed up a search that can already solve, per
`measure-window-before-campaign`.

### Step 4: Jev offline test

A PEP 723 Python script under `workloads/gb/scripts` that takes a step 3
archive dump, renders each live entry's observation to JSON, sends batches of
up to 255 entries to Jev, and asks:

- one `choice` over the entries: "which of these positions leads soonest to
  the next milestone";
- one `score` per entry with the milestone list as the scale: "how far
  along the route to Brock is this state";
- one `noul` per entry: "is this a dead end for reaching Brock".

Compare the picks with the known route. The check is whether Jev's top picks
sit on the route and its dead-end answers cover the places the searcher spent
draws on without progress. If they do not, stop here and write up the
comparison. Record cost and latency per batch.

### Step 5: Jev in the draw

Only after step 4 passes. In `workloads/gb/src/campaign.rs`, every N
admissions the workload sends the current state's generated alphabet to Jev
with one `choice` question, "which action leads soonest to the next
milestone", and multiplies each action's draw weight by its probability plus a
floor. The floor keeps every action reachable so a wrong answer slows the
search rather than trapping it (`never-revert-draw-strategy`). The answer and
its usage go into the campaign stream beside the duration draw, and replay
reads the record and never calls the API. The switch is a run policy
recorded in the run identity; no key in the environment means the plain run
and an identity that says so.

Run the paired three-seed comparison against step 3 at matched executions and
judge by film first, then the milestone table.

## Working rules

- Read `CLAUDE.md`, `REVIEWING.md`, `dissonance/searcher/README.md`,
  `workloads/nes/src/metroid/README.md` and the searcher plans README before
  starting. Rust carries no comments. Component knowledge goes in the crate
  README.
- Local checks are the fmt, clippy and test commands from the searcher plans
  README, plus the same three for `workloads/gb-machine` and `workloads/gb`,
  plus `scripts/custom-lints.py`, `scripts/strip-comments.py --check` and
  `scripts/check-dependency-boundaries.py`.
- Steps 1 and 2 are one pull request. Steps 3 to 5 each get their own.
- Keep each step to its plan. If a step needs something the plan does not
  say, add it to this file in the same pull request.
- When stuck for more than a couple of hours on one problem, stop and write
  down what was tried, what the checks say, and what the film shows.
