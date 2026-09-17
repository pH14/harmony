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
| ROM | `~/Downloads/pokemon-blue.gb` on the workstation, copy to any lab box | SHA-256 `2a951313c2640e8c2cb21f25d1db019ae6245d9c7121f754fa61afd7bee6452d`; 1 MiB; add under key `blue` in the private assets inventory the way `benchmarks/search/README.md` describes |
| Emulator core | Gambatte libretro, https://github.com/libretro/gambatte-libretro | pin one revision in `scripts/build-gambatte-core.sh`, copied from `scripts/build-quicknes-core.sh` |
| TypeSafe key | `TYPESAFE_API_KEY` in `~/.zshrc` on the workstation | copy to msr1 and ms02 freely; only step 4 and 5 read it |
| Jev API | the `typesafe-jev-model` memory note, then https://docs.typesafe.ai/llms.txt | one endpoint, three question types, no generation, no images |

## Order

| Step | What is built | Check after |
|---|---|---|
| 1 | `workloads/gb-machine`: Gambatte libretro driver | loopback and real-core tests, determinism check |
| 2 | `workloads/gb`: Pokémon Blue workload with macro actions | new-game to Brock reachable by a scripted tape, replay exact |
| 3 | plain campaign, three seeds on msr1 | film, milestone table, stalled archives saved |
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
Measure frames per second headless on msr1 and put it in the crate README.
msr1 is arm64, so the core build script must handle `aarch64` the way
`scripts/build-quicknes-core.sh` handles its two hosts, and the pinned core
hash is per architecture.

Added while building step 1:

- Serialize and restore are not byte-exact as written above. Two things break
  it and the driver closes both; `workloads/gb-machine/README.md` records what
  they are. Every action must begin from a restored state, and the snapshot
  must clear the clock, HuC3 and DMG palette records.
- Build the core with `HAVE_NETWORK=0`; the serial-link listener would
  otherwise open a socket into the emulated machine.
- The driver refuses a cartridge type that declares a clock.
- `HARMONY_GAMBATTE_CORE` names the core and `HARMONY_BLUE_ROM` the ROM.
- `gb-machine` defines its own machine vocabulary rather than importing
  `nes-machine`, whose generic half is entangled with the NES Consonance
  backend.

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

Added while building step 2:

- A fifth module, `map.rs`, holds the overworld collision model and the
  routing over it. Walking is breadth-first over that model rather than A*,
  since a map is small enough that the two cost the same.
- Collision reads the lower-left tile of the 2×2 tile square a step covers,
  not the upper-left one. Reading the upper-left tile walks through ledges and
  walls.
- The three tiles in `wTilesetTalkingOverTiles` extend talking range to two
  steps. Without them the Poké Mart cashier and the Pokémon Center nurse are
  unreachable behind their counters. The alphabet generates both approach
  distances.
- The alphabet also holds one exit per map edge, and marks tiles under
  standing sprites unwalkable.
- `action_cost_fn` is a pure function of the action, so a macro cannot report
  the frames it spent as its cost. Each macro declares a frame ceiling as its
  cost and stops there; the frames actually spent are execution work.
- The observation summarises the 320-byte event block as a set-bit count, a
  digest, and the seven tracked flags. The warp table is read live from RAM
  rather than stored.
- The route flags latch once set, and the three "entered" milestones come from
  map ids rather than event flags.
- The archive key carries the number of set event bits, between the route flags
  and the map. The plan's key has no gradient through a scripted sequence: Oak's
  speech is ten actions in two cells, and the lineage holding the Poke Ball tile
  without having heard the speech cannot take the starter and is never displaced.
  The count is the game's own progress record, so it works for every script.
- The key's preference is party HP then party levels. The archive already
  breaks a preference tie by lower accumulated cost and the key cannot see how
  many actions reached it, so fewest actions stays the archive's rule.
- A drawn macro that the live state cannot run is mapped onto one it can by
  `ActionKind::in_context`, so every draw is usable.
- `interact` stops at a yes/no box (`wTextBoxID` `0x14`) and leaves the answer
  to the next action: interact means yes, advance means no.
- The driver gained `begin_action` and `hold_frame` so a macro runs frame by
  frame inside one action, and `set_wram_capture` so it does not copy work RAM
  on every frame.

### Step 3: plain campaign

Add a `blue` case to `benchmarks/search/pilot.json` with a new-game origin,
and run three seeds, eight workers, 2048 MiB, a 30-minute wall limit each, on
msr1 (see the `msr1` skill for access; ms02 is the fallback). Judge by the README rules for the searcher plans: film the deepest
witness per seed and write one sentence each, then read the milestone table.
Save each run's end-of-run report and the live archive entries; step 4 reads
them. If no seed reaches Brock, diagnose from the film before step 4, since
the goal of steps 4 and 5 is to speed up a search that can already solve, per
`measure-window-before-campaign`.

Added while running step 3:

- The panel is its own file, `benchmarks/search/blue-pilot.json`. The cases in
  `pilot.json` all run through the common runner and Blue runs through
  `blue-campaign`.
- The archive key carries the stage of the fight, between the event count and
  the map: 0 out of battle, 1 to 8 in one as the opponent's remaining health
  falls. The first plain seed spent 59,517 executions inside Pallet Town and its
  buildings and never crossed Route 1. Every one of its 24 lineages holding the
  starter sat in Oak's lab, and 293 of its 325 entries were at full health. The
  rival battle that blocks the lab door is eight actions at one cell, the player
  does not move during a battle, and each turn costs health, so the preference
  handed the slot back to the lineage that had not fought. With the stage in the
  key a 12,000-execution run reaches twelve maps and Oak's parcel.
- The stage is a place and not a rank. A fight in progress is not more progress
  than a milestone, so `progress_cmp` does not read it.

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

Added while running step 4:

- `blue-probe trace` writes the archive key and the milestone flags after each
  action of the scripted route to `workloads/gb/fixtures/brock-trace.json`.
  This step needs a position-by-position record of the route and the campaign
  report does not carry one.
- Jev's ranking beats the archive's own. Measured over 271 live entries: Jev's
  score orders a pair of on-route entries by true route position 83% of the
  time against the archive's own preference at 75%. Nine batches of 32 cost
  0.6 cents and the median request took 0.28 s.
- Of the three questions, `score` is the one that ranks positions. The `choice`
  answer reproduces the score ordering and adds nothing over it. The `noul`
  dead-end answer does not separate the two populations, 0.31 on the route
  against 0.33 off it.
- The comparison reports each ordering's agreement with the route, not only
  whether a top pick stands on a route tile. Standing one tile off the scripted
  path is not the same as being behind, and every entry holding the starter
  inside Oak's lab reads as off-route under the tile test.
- A place on the route is the badges byte, the route flags, the map and the
  cell. The route crosses Pallet Town and Oak's lab twice, so without the flags
  a late revisit reads as early progress and a correct ranking scores zero.
- The baseline orders equal progress by accumulated frames, which is what the
  archive's cheapest-first rule uses. Action count is not the same ordering: one
  walk costs 1,650 frames and two interactions cost 1,500.

### Step 5: Jev in the draw

Only after step 4 passes. In `workloads/gb/src/campaign.rs`, the workload
sends the current state's generated alphabet to Jev with one `score` question
per action, "how much does this action advance the route to Brock", and
multiplies each action's draw weight by its score plus a floor. The floor keeps every action reachable so a wrong answer slows the
search rather than trapping it (`never-revert-draw-strategy`). The answer and
its usage go into the campaign stream beside the duration draw, and replay
reads the record and never calls the API. The switch is a run policy
recorded in the run identity; no key in the environment means the plain run
and an identity that says so.

Run the paired three-seed comparison against step 3 at matched executions and
judge by film first, then the milestone table.

The two arms run side by side on one box, six workers each, so a seed sees the
same machine in both arms. That is why `blue-pilot.json` declares six workers
rather than eight.

Added while building step 5:

- The draw weights the macro kind, not the slot inside it. A slot index only
  means something against the live alphabet and `expand_suffix` never sees the
  live state, so a weight on a slot would be a weight on nothing.
- The question is `choice` over the six kinds, not `score` per kind. Step 4
  found `choice` no better than `score` for ranking positions; over six kinds
  its answer is a distribution that has to sum to one, and it separates them
  far more sharply. Asked in Oak's lab it puts 0.92 on interact; asked on
  Route 1 it puts 0.95 on walk. The same places scored one at a time come back
  between 0.6 and 1.9 on a four-rung scale, which is almost no separation.
- Each kind is described by both of its meanings. `ActionKind::in_context` maps
  a drawn kind onto one the live state can run, so `battle_move` walks when no
  battle is running. Described as a battle move alone it is rated useless in a
  town, and that rating lands on walking.
- An advised run is not replay-checked. The recorded table is what a replayed
  draw reads, and rebuilding the counters would mean reaching the API again, so
  `--verify-replay` refuses `--advise`.
- A place is the badges byte, the route flags and the map id, taken from the
  parent key through `duration_request`, which is the only hook that carries any
  part of the parent's position into the draw. Cells are left out: a per-cell
  table would ask thousands of questions for one map.
- The answer arrives one stream record late. `duration_request` cannot call out
  and `finish_stream_record` is the hook that can change the table, so the first
  draws at a new place use even weights.
- The adviser posts through `curl` rather than a Rust HTTP client. No crate in
  the repository speaks HTTP, and `blue-film` already shells out to `ffmpeg`.
  The key goes in on standard input so it never reaches a command line.
- Map names come from pokered's `constants/map_constants.asm`, so the model can
  tell a house from a route.

## Working rules

- Read `CLAUDE.md`, `REVIEWING.md`, `dissonance/searcher/README.md`,
  `workloads/nes/src/metroid/README.md` and the searcher plans README before
  starting. Rust carries no comments. Component knowledge goes in the crate
  README.
- Local checks are the fmt, clippy and test commands from the searcher plans
  README, plus the same three for `workloads/gb-machine` and `workloads/gb`,
  plus `scripts/custom-lints.py`, `scripts/strip-comments.py --check` and
  `scripts/check-dependency-boundaries.py`.
- All five steps land in one pull request, one commit per step.
- Keep each step to its plan. If a step needs something the plan does not
  say, add it to this file in the same pull request.
- When stuck for more than a couple of hours on one problem, stop and write
  down what was tried, what the checks say, and what the film shows.
