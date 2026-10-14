# Harmony browser explorer

A static demonstration of state-space exploration. Emulators compiled to
WebAssembly run a game, and a Rust worker uses the Dissonance `Archive` and
parent selector to search it. The site plays three games: the NES game Twin
Dragons on QuickNES, the Game Boy game Tobu Tobu Girl Deluxe on SameBoy, and
Super Mario Bros., Mega Man 2 and Metroid on QuickNES from ROMs the visitor
supplies. The `game` URL
parameter picks the game, and the level selector switches between them. Each
game has one map per level. All execution happens on the visitor's computer,
so hosting needs only static files.

## Run

Prerequisites: the repository Rust toolchain, `wasm32-unknown-unknown`,
wasm-pack 0.14.0, Emscripten 4.0.15, Node 22.19 or later, and GNU coreutils
(`sha256sum`). Activate Emscripten before building. On macOS, put GNU coreutils
in `PATH`.

```sh
rustup target add wasm32-unknown-unknown
cd demos/twin-dragons
npm ci
./tools/build-runtime.sh
npm run build
npm run preview -- --port 4173
```

Open http://127.0.0.1:4173. The production `dist/` is self-contained. Serve it
at any URL path with a static HTTP server. ES modules and WASM require HTTP. The
site needs no shared memory, cross-origin isolation, server computation or
credentials.

`tools/build-runtime.sh` builds the pinned QuickNES core, the Rust worker, the
maps and the sprite sheet. The core carries the idle-loop patch from
`scripts/build-quicknes-core.sh`, the native Harmony build; the browser build
extracts it from that script, checks its SHA-256 and ships it in `licenses/`.
The patch skips whole iterations of the game's wait-for-interrupt loop and
leaves emulated state and timing unchanged. It gets the ROM from
`workloads/nes/scripts/build-twin-dragons-rom.sh`, which downloads the pinned
upstream release and zeroes its sound-effect data. The same script's pinned
source archive supplies level data, tiles and tables for the maps. The build
extracts only those files into the gitignored `.build/` directory and never
publishes the archive.

## The game

Twin Dragons has three levels and no save RAM. The adapter reads the 2 KiB work
RAM at these addresses:

| Address | Meaning |
| --- | --- |
| `$9E` | game state: 0 title, 1 playing, 2 loading |
| `$42` | current level, 0 to 2 |
| `$35` | next level; `$FF` after the last level |
| `$04E3` | goal touched |
| `$52`, `$57` | camera page and offset |
| `$62`, `$63` | player x on screen, player y |
| `$6B` | on ground |
| `$71`, `$72` | animation number and frame |
| `$74` | facing, 1 is right |
| `$75` | health, 0 to 3 |

The world x position is `$52 * 256 + $57 + $62`. For four frames after a level
change, the game reports state 1 with the new level while the goal flag is
still set and `$35` still names the old next level. The adapter treats a state
as settled when the game state is 1 and `$35` is the level after `$42` (or
`$FF` on the last level). Every other state is loading. A level counts as
cleared when a settled state has touched the goal. The game is complete when
the third level is cleared.

## Search

The search runs in one coordinator worker and a pool of rollout workers. The
coordinator owns the Dissonance archive and parent selector. Each rollout
worker holds its own QuickNES instance. The page starts every worker and hands
the coordinator a `MessageChannel` port to each rollout worker, because some
browsers fail to load a worker started from inside another worker. The archive place is (level, loading,
x / 32, y / 32), its progress is the number of levels cleared, and its identity
within a place is (x / 16, y / 16). The place rows match the map's cell rows.
The parent selector weighs each progress tier 2 times the tier below, the same
weight as the `workloads/nes` Super Mario Bros. key, so new work concentrates on
the furthest level without a direction preference. Within
a place it prefers more health. A state with zero health or on the
title screen ends a rollout. The 32-pixel heatmap projection is separate from
archive identity.

Rollouts have 1 to 24 controller actions. Each action holds its buttons for 2
to 12 frames or 6 to 18 frames, with equal odds. The coordinator plans rollouts
in rounds of 16. For each rollout it selects a parent and draws the actions
from the seeded generator. It plans four rounds ahead and plans a new round
each time the oldest round has fully merged. Rollout workers run the actions
from the parent snapshot and return the 2 KiB work RAM and the raw snapshot
after each action, plus a motion trail. The coordinator merges results into
the archive in plan order, whatever order they arrive in. The seed therefore
determines every search choice, and the worker count and timing change only
throughput and presentation. The first search uses seed 27, and Restart
increments it. Seed 27 enters Level 2 after about 1,760 paths, Level 3 after
about 8,850 and finishes the game after about 14,200. A history is bounded to
10,000 actions and 200,000 frames. Runs stop when a path finishes the game or
when the search WASM buffers reach their limit.

The coordinator sends one rollout per message and keeps two messages
outstanding per worker. Each worker caches up to 64 parent snapshots by id;
the coordinator tracks every worker's cache, so a message carries a snapshot
only when the worker lacks it. Results come back in one transferred buffer
per message. Pause stops sending new rollouts and holds results until resume,
so the counters freeze at once.

Desktops run `navigator.hardwareConcurrency - 2` rollout workers, at most 8, so
the page and the coordinator each keep a core. WebKit throughput drops about 4
times when the rollout workers use every core: on 8 cores of the machine below,
6 workers run 132,000 frames per second and 7 run 30,000.
Phones, coarse-pointer devices and devices reporting at most 4 GiB of RAM run
2 or 3. The `workers` URL parameter overrides the count, from 1 to 16. The
Details pane shows the count. Each rollout worker holds a 16 MiB QuickNES heap
and up to 0.8 MB of cached parents. Small devices use 32 MiB of snapshots and
96 MiB of coordinator WASM buffers; other devices use 1,024 MiB and 1,536 MiB. A
Twin Dragons snapshot is 12,792 bytes before compression, and 60,000 paths
retain about 10 MB of compressed snapshots.

The snapshot budget never stops a search. Every entry the archive retires
releases its snapshot at the end of each rollout. When resident snapshots
exceed the budget, the adapter lowers the archive's `max_entries` cap by about
1.5%, never below 256 active entries, so each later admission retires an older
entry. The archive selects only active entries, and the coordinator copies each
parent snapshot when it plans the rollout, so a rollout never needs a released
snapshot. Each admitted state keeps a 64-bit digest of its raw snapshot, so
replays of retired paths still verify exactly. Up to eight searches are
retained in the coordinator, with only one advancing at a time on all rollout
workers. Rollouts already sent for a search still merge into it after a
switch. The advancing search gets the global budget minus the bytes the others hold, with
a 2 MiB floor.

The coordinator sends at most 85 rollouts per second, so a viewer can follow
the search. Winning the game takes 9,700 to 15,600 rollouts across 18 seeds, so
a paced search finishes in about 2 to 3 minutes on any device that keeps up.
The `pace` URL parameter sets the starting rate in rollouts per second, and
`pace=0` removes the limit. The Search speed slider under Details changes the
rate while the search runs: 10, 30, the game's pace, 300, 1,000 or unlimited.
The browser tests run with `pace=0`, and the throughput
figures below are unpaced.

Throughput scales with the worker count. On the 8 performance cores of an
Intel Core Ultra 9 285HX in Chrome, 6 rollout workers run about 217,000 game
frames per second, 7 run 236,000 and 8 run 243,000 (3,600 paths per second),
and 8 workers reach Level 2 in 0.7 seconds and Level 3 in 2.6 seconds. Workers
wait on the round boundary less than 1% of the time. The core is built with
`-O3` and link-time optimization. On a 4-core phone layout with 3 workers and
the CPU slowed 6 times, the search runs about 18,600 frames per second. A
rollout first reaches the middle of Level 1 after 3.9 seconds, Level 2 after
9.3 seconds and Level 3 after 36 seconds. Slowed 4 times, the same
milestones take 2.6, 6.1 and 23 seconds. The page holds 60 frames per second
in both cases.

The map samples swarm sprites evenly when drawing falls behind. While display
frames run late and drawing the maps takes more than 6 ms, the sprite budget
shrinks by 10% per frame, down to 256; it grows back once frames are on time.

Each level's root comes from a fixed boot route. `boot(level)` restores a
power-on snapshot and presses nothing for 60 frames, Start for 2 frames, and
nothing for 240 frames. It then waits up to 120 frames for the player to land.
For Level 2 or 3 it writes the level to `$35` and sets `$04E3` during Level 1
gameplay, then runs until the requested level is settled, the goal flag is
clear and the player is on the ground. Every root has full health. The level
selector starts a fresh search at the chosen root. Replay and forks include the
starting level, and the worker rejects tapes from a different root.

## Maps and sprites

`tools/build-panorama.mjs` draws one map per level from the pinned source:
the run-length `.meta` and `.attrs` level columns, the `metatiles` table, the
background tile table, `pallete0` and each level's background color. The build
checks each level's columns against its Tiled `.tmx` file. Maps are 224 lines
tall, matching the visible screen with 8 lines of overscan cropped at the top
and bottom. The build compares each map with the emulator's first frame of the
level and requires 97% agreement; the measured agreement is 99.5%, 99.5% and
99.2%, with the difference coming from sprites.

`tools/build-sprites.mjs` draws seven player poses (idle, four walking frames,
breathing fire and hurt) in both directions from the frame tables in
`anims.mfk`, the sprite tile table and the sprite palette. The build compares
the sheet with emulator frames while standing, walking in both directions and
breathing fire. Sprites appear on screen one frame after the RAM that places
them, so the check reads RAM one frame early.

Colors use the QuickNES palette after its RGB565 output conversion. Map and
sprite PNGs carry the game and artwork credits in their text metadata.

## Interaction

The page opens with a one-line pitch, one live total (timelines tried across
every search branch) and Take the tour. The per-search counters and the
rollout worker count sit in a collapsed Details disclosure at the bottom of the
Searches pane. The footer
holds the Harmony link, Credits & source and the Dark mode / Light mode switch.

Two toggles, Movement and Heatmap, choose what the map shows. Either or both
can be on, and the last one on stays on. Movement alone is the default.
Movement shows the level map with small dragon sprites replaying recorded
search rollouts. Each rollout worker samples its emulator every four game
frames, without changing inputs, random draws or captured endpoints. Loading
frames and frames with zero health are hidden. Each sprite fades in as its
rollout starts and eases to about two-thirds opacity as it finishes, followed
by two faint dots at its position six and twelve game frames earlier. Sprites
are drawn at least 18 CSS pixels tall, up to 2.5 times their native size.
Heatmap counts the rollouts that pass through each 32-pixel cell. A cell's
count halves every 2,000 rollouts and every 2.5 seconds while that search runs,
so the map fades quickly at the default pace and keeps up at full speed. Each
cell's color runs from blue through red by its count over the hottest cell in
the same level, so the cells where the search spends most of its rollouts now
are red. Heatmap draws nothing else
from the search. Cell rows start 8 pixels below a 32-pixel
boundary of the map, on the 16-pixel tile grid, so a dragon standing on a
floor sits inside one cell.

Each level shows through one camera viewport: a 4:1 strip on desktop and a 2:1
view on phones. The camera holds the full level height on screen and follows
the search frontier (the 80th-percentile x of live sprites, smoothed), or the
furthest cell the search has reached, or the level start. Following pauses
while the mouse is over the map and for 2.5 seconds after a touch. Auto view
and Full view sit beside the visualization toggles. Full view fits the whole
level and stops following. A drag, pinch or edge pan holds the camera where the
visitor left it, and following resumes 10 seconds after the last movement.
Selecting a cell stops following. Hovering the map shows chevrons at its left
and right edges; resting on one pans that way, and a tap on a touch screen
glides about half a screen. A strip under the view shows the whole level, with
each live sprite as a dot (or the heat cells in Heatmap mode) and a box for the
part on screen. Clicking or dragging the strip moves the view there, and a
sideways scroll on the map or the strip pans it. A room the visitor has not
seen yet opens at the close-up, centered on the frontier or the level start.

A game with `grid: true` has maps that run in two directions. Its view is 4:3,
and the whole-level map sits beside it, or under it on screens narrower than
720 pixels. Clicking the map moves the view on both axes, and a scroll moves it
on both. Chevrons at the top and bottom edges of the view pan it up and down
the same way the left and right ones pan it sideways. The game's `route(level, x, y)` ranks a map point by its order along
the level, and the frontier is the 80th-percentile live sprite in that order.
With nothing live, the camera goes to the furthest heat cell in that order, or
to `entry(level)`.

The page shows one room at a time. When a level has several rooms, such as the
Super Mario Bros. bonus areas, tabs above the view switch between them. Auto
view moves to a room that has had 1.5 times as many live sprites as the shown
room for 2.5 seconds. A newly found room in the shown level does not move the
view by itself, so one rollout entering a bonus room leaves the view on the
busy room. The level row
above the map lists the levels the search has reached. Once it holds four or
more, all but the last two and the shown level collapse into a +N button that
expands the full list. The row wraps instead of scrolling sideways.

A rollout that raises the cleared-level count is a clearing rollout. The map
always draws clearing rollouts and the camera follows the one that leaves the
level first. The map moves to the next level as soon as the search reports a
state there, so the page never shows a level the search has already left. New
rooms in levels behind the shown one never move the map back. The automatic
replay pick skips states outside the shown level. Showing a level also builds
the next level's map in the background, so the map is ready when the search
arrives. In a game whose maps are built in the browser, a search starts once
the map of its starting level is ready, so rollouts never play over a blank
view.

On a first visit the camera opens at 1.3 times the close-up, follows for 1.5
seconds, then eases to the close-up over 0.8 seconds. Returning and
reduced-motion visitors start at the close-up.

Clicking a cell with retained states opens History, listing up to twelve saved
paths to that cell. Selecting a path draws its trail in gold and replays its
controller history in a second emulator, comparing the endpoint byte for byte.
Play from here hands control to the visitor: arrows or WASD move, Z or Space
jumps and X breathes fire. Branch search from here verifies the controller
prefix and starts a separate search rooted at that state. The fork point
ripples in gold, the camera centers on it for 2.6 seconds, and the new row
slides into Searches. Each Searches row shows the idle dragon sprite as its
icon. On phones History is a full-width bottom sheet; closing it leaves a
History bar docked at the bottom edge that reopens it.

The guided tour has five steps: Multiverse exploration spotlights the live
map; Zooming in selects a cell, draws its route and lights History; Your turn
covers Play from here; Hand it back covers branching or discarding the played
draft; and Search branches spotlights the Searches pane. Next on Your turn
skips Hand it back until the visitor has played. The tour opens once after the
opening camera finishes and the search has populated a cell.

Replay and play round the frame count owed to elapsed time, so a 60 Hz display
steps one game frame per display frame. The map repaints every display frame
while the search runs and at 30 Hz while it is paused. Replays play the game's
music. The sound effects are absent from this build.

The Replay button reads Pause while a replay plays and Replay otherwise. A
replay the visitor starts pauses the search, and the search resumes when the
replay pauses, ends or closes, unless the visitor paused or resumed the search
in the meantime. The automatic replay pick plays while the search runs.

## Games

Each game is a module in `src/games/` that names its core, ROM, RAM size,
level count, screen and sprite sizes, controls, credits, rollout pace and
history limits, and supplies `decode`, `ended`, `motion` and `boot`. A game
with a scripted level change also supplies `levelComplete` and `transition`.
`src/emulator.js` lists the games and cores. The Rust `Game` enum in
`rust/src/lib.rs` holds the matching decoder and archive key, and
`tests/*.mjs` check that the two decoders agree.

### Tobu Tobu Girl Deluxe

`tools/build-runtime.sh` compiles the ROM from the pinned upstream source with
`workloads/gb/scripts/build-tobu-tobu-girl-rom.sh`. That toolchain runs only on
x86-64 Linux, so other hosts build it in a `debian:13` amd64 Docker container;
the ROM hash matches on both. `tools/build-sameboy-runtime.sh` builds the
pinned SameBoy core with a patch that skips idle CPU time during HALT. The
patch changes only speed, and the core's snapshots and work RAM match the
unpatched core frame for frame.

The adapter reads the first 1 KiB bank of work RAM:

| Address | Meaning |
| --- | --- |
| `$A5` | game state: 3 level select, 4 in level, 6 level won, 9 unlock notice, 10 ending |
| `$A6` | scene within a level: 0 playing, 1 died, 2 level complete |
| `$A7` | level, 1 to 3 |
| `$B0` | time left |
| `$C2`, `$C3` | player x and y on screen |
| `$230`, `$233`, `$237` | climb terms |

The level scrolls upward, so the camera viewport is a 432 × 900 canvas that
fits the 144-pixel level width, and it follows the highest players. The level
strip stands beside the view at its full height, and the mouse wheel scrolls
the view. Manual view takes the place of Full view and
stops following. Climb is `$233 * $237 + $230`. Map coordinates are x =
screen x - 24 and y = level height - 155 - climb + screen y. Level heights are
2,000, 2,896 and 3,792 lines. `tools/build-tobu-maps.mjs` climbs each level
with a save-state search and composes the maps and the sprite sheet from the
emulator's frames. The background scrolls slower than the level, so each map
stretches the 256-line background to the level's height.

A rollout ends when the player dies or finishes a level. After a finished
level, `transition` presses A and Start through the win and level-select
screens until the next level is playable. Those presses are part of the
history. The archive place is (level, loading, climb / 32, x / 32), progress
is (levels cleared, climb / 1,024), and the preference is more time left.
Seeds 1 to 8 finish the game in 15,806 to 44,693 rollouts, median 18,775, so
the pace is 120 rollouts per second.

### Super Mario Bros.

The site ships nothing from Super Mario Bros. The level selector accepts the
ROM with SHA-256 `0b3d9e1f…6dea`, the same ROM as `workloads/nes`. The page
keeps it in IndexedDB and never uploads it. A visit without a stored ROM shows
Twin Dragons and asks for the file.

The adapter uses the RAM rules from `workloads/nes/src/smb`:

| Address | Meaning |
| --- | --- |
| `$075F`, `$075C` | world and level; the level is one ahead once `$0746` is 5 |
| `$074E`, `$074F` | area type and area offset |
| `$0770`, `$0772` | mode (1 play, 2 castle cleared or game won) and play task (3 playing) |
| `$000E` | player state; `$0B` is dead |
| `$00B5` | vertical page; 2 or more has fallen into a pit |
| `$006D`, `$0086`, `$00CE` | player x page, x and y |
| `$071A`, `$071C` | camera page and x |
| `$0746` | flag-pole sequence |
| `$07F8`–`$07FA` | timer digits |
| `$06D9`, `$06DA` | castle loop checks passed correctly and in total |

The 32 levels are the map ids, world × 4 + level. A state in another area,
such as a bonus room or the World 8-4 underwater section, gets the map id 32 +
level × 128 + area. The page builds that area's map the first time the search
enters it and shows it as another room of the level. The builder enters an area
by writing `$0750` (area pointer) and `$0751` (entrance page) and setting
`$0772` to 0. Pipe scenes and other non-play tasks count as loading. `boot(level)` writes the world, level and area number on the title
screen and presses Start. The archive place is (level, loop on path, area ×
512 + camera / 16 + player screen x / 16, y / 16 and timer hundreds), progress
is (levels cleared, camera / 1,024), and the preference is more time left.
A separate area restarts the camera measure, so the World 8-4 underwater
section ranks below the castle tiers. At 8 times per tier the search stalls
there. A
rollout ends at death, the flag pole or a castle's end. `transition` presses
nothing until the next level is playable.

The page builds each map in a worker from the ROM. The builder holds Mario at
a fixed height, hides sprites, clears enemies and loop checks, holds Right
and B, and copies each newly scrolled column into the map. When Mario stops
moving it tries the next height. It keeps the flag pole, so each map ends after
the flag sequence. Built maps are cached in IndexedDB by ROM hash. Maps build in a pool of
one map worker per four cores, at most three, and the shown level's map
builds first. Map widths
are fixed per level, because the page lays out maps before they are built.
Seed 27 reaches World 1-2 from World 1-1 after about 2,300 rollouts.
`tests/smb.mjs` reads the ROM from `SMB_ROM` or
`~/workspace/roms/Super Mario Bros. (World).nes` and skips when it is absent.

### Mega Man 2

The site ships nothing from Mega Man 2. The level selector accepts the ROM with
SHA-256 `49136b41…1edf`, and the page keeps it in IndexedDB as for Super Mario
Bros. The 14 levels are the eight Robot Master stages in the order Crash, Flash,
Metal, Air, Bubble, Heat, Wood and Quick, then Wily 1 to 6.

| Address | Meaning |
| --- | --- |
| `$2A` | stage |
| `$2C` | player state: 0 dying, 1 fallen, 3 standing, 6 airborne, 9 ladder, 10 ladder top |
| `$29` | bank; `$0E` is play |
| `$38` | room |
| `$440`, `$460`, `$4A0` | player screen, x and y |
| `$6C0`, `$6C1` | player and boss health |
| `$B1` | boss phase; `$FE` defeated, `$FF` cleared |
| `$B3`, `$BC` | current boss and Wily 5 refights |
| `$9A`, `$9B` | weapons and items |
| `$F7` | menu mode; `$90` is stage select |

Each stage's room table sits at offset `$3400` of the stage's bank. Each byte is
one room: the low 5 bits are the screen count less 1, and the top bits give the
exit (`$40` down, `$80` up, `$20` right through a door, 0 at the boss). The
Wily stages share banks with the Robot Masters and start at the room and screen
in the checkpoint tables at `$3B33` and `$3B39`. `layout(stage)` places each
screen on a grid of 256 by 224 cells from that table, so a map is one image per
stage. `boot(level)` picks Heat Man from stage select and writes the target
stage during the load. A Wily boot also awards every weapon.

The archive place is (level, loading, x / 32, y / 32 and boss damage), progress
is (levels cleared, half-screens along the route × 32 + boss damage), and the
preference is more health. A rollout ends at death, a defeated boss or a menu.
After a Robot Master, `transition` waits for the weapon screen, presses Start
until stage select appears, and picks the next unbeaten stage in the order
above, or Wily once all eight are beaten. After a Wily stage it waits for the
next stage.

The map builder flies Mega Man through each room at fixed heights, keeps his
health full, clears enemies, hides sprites and rolls back to a snapshot on
death. When he stops moving it moves him forward. It copies a camera frame into
the map whenever the camera is still in the expected room. It finds a room's
exit by trying a grid of positions with the exit's direction held, with
rendering off. A try ends once Mega Man stands still for 30 frames. Crash
Man's map takes about 2 seconds to build. The Wily 2, 3 and 6 maps stop
partway, because the builder does not yet find some of their exits. Seed 3
climbs about 420 pixels of Crash Man's stage in 3,000 rollouts. `tests/mm2.mjs`
reads the ROM from `MM2_ROM` or `~/workspace/roms/mm2.nes` and skips when it is
absent.

### Metroid

The site ships nothing from Metroid. The level selector accepts the ROM with
SHA-256 `e6e6b701…65b6`. Metroid has one level, Zebes, whose five rooms are the
areas Brinstar, Norfair, Kraid's Lair, Ridley's Lair and Tourian. The search
starts from a new game, and the view follows it between areas the same way it
follows Super Mario Bros. between bonus areas.

Metroid keeps its items in cartridge RAM. The QuickNES shim keeps a 10 KiB copy
of memory, the 2 KiB console RAM followed by the 8 KiB cartridge RAM at
`$6000`, and refreshes it after every run and restore. A game with
`cartridgeMemory: true` reads that copy, and the loader sets the iNES battery
flag so the core exposes the cartridge RAM. `engine.commitMemory()` writes the
copy back after a game changes it.

| Address | Meaning |
| --- | --- |
| `$1E` | game mode; 3 is play |
| `$49` | scroll direction: 0 up, 1 down, 2 left, 3 right |
| `$4D` | facing; 0 is right |
| `$4F`, `$50` | map row and column |
| `$56` | door state; 0 and 5 are outside a door |
| `$74` | area: low nibble 0 Brinstar, 1 Norfair, 2 Kraid, 3 Tourian, 4 Ridley |
| `$106`, `$107` | health in BCD tenths |
| `$30C`–`$30E` | Samus's name table, y and x |
| `$6877`–`$687C` | energy tanks, equipment, missiles, missile capacity, Kraid and Ridley |
| `$6883` | ending |

Samus's map cell is the map position corrected by the scroll direction and
name table, as in `workloads/nes/src/metroid`. Each area's map is the bounding
box of its cells, and the table of which area owns each of the 32 × 32 world
cells comes from a search over doors and passable screen edges. A rollout ends
at death, a health reading of 800 or more (a BCD borrow), or the ending. A
state inside a door transition is loading. The archive place is (area, loading,
x / 32, y / 32), progress is the count of equipment bits, tanks and defeated
bosses, and the preference is more missiles, then more health. The draw adds a
Select press to one action in eight, because Select switches to missiles. Start
pauses the game and is never drawn.

The maps are drawn from the ROM's level data with no emulation. The world map
at `$A53E` in bank 0 gives each cell's room. Each area bank holds the room,
structure and macro tables at `$959A`, `$959C` and `$959E`, and its palette
behind `$9560`. The pattern tables are copied in the order of the area's entries
in the table at `$C6E0`. Door caps are drawn as bars in their palette colors.
Enemies, items and elevators are sprites and are not drawn. Each screen is a
separate 256 × 240 bitmap, because a whole area is too large for one canvas on
a phone, and the strip beside the view draws a reduced copy. Seed 3 moves
Samus about 600 pixels from the start in 3,000 rollouts. `tests/metroid.mjs`
reads the ROM from `METROID_ROM` or `~/workspace/roms/Metroid (USA).nes` and
skips when it is absent.

## Verification and publishing

```sh
npm test
cargo test --locked --manifest-path rust/Cargo.toml
cargo clippy --locked --manifest-path rust/Cargo.toml --all-targets -- -D warnings
wasm-pack test --node rust
npx playwright install chromium
# While the preview server is running:
npm run test:browser
```

`tests/levels.mjs` boots all three levels and checks health, progress, a fresh
search, exact replay, repeated boot and a rooted fork. `tests/search.mjs`
requires seeds 27 and 34 to enter Level 2 within 6,000 paths and exactly
replays each arrival. It runs the same seed twice with three emulators and
once with one, requires identical batches and digests, and replays a state
found by the third emulator. It replays the `level-one`, `level-two` and `level-three`
fixtures against their endpoint checksums, and checks that the Level 1 fixture
plus the rest of the Level 2 controller history reproduces the Level 2
snapshot. It also forks mid-action and from manual play in Levels 1 and 2 and
checks the audio emulator against the search emulator. The fixtures came from
real seed 3 and seed 5 searches and contain only controller inputs.
`TWIN_DRAGONS_PROGRESS_PATHS=25000 npm run test:search` requires seed 11 to
enter Level 3, which it does at about 5,700 paths. `tests/parallel.mjs` loads
the page twice with three workers and once with one, and requires the first
120 archived histories and digests to match.

`tests/search.mjs` also runs a 2 MiB snapshot budget for 3,000 rollouts and
checks that the search keeps going, retires snapshots and still replays
exactly. `tests/media.test.js` checks the snapshot digest against the Rust
reference vectors.

The browser tests cover the swarm, heatmap, route selection, exact replay,
scrubbing across the Level 1 to Level 2 change, 1x and 4x playback, human play,
branching, the guided tour on desktop and phone, the level selector and the
credits. `tests/browser.mjs` runs one worker so its Level 1 checks finish
before the search reaches Level 2. `tests/room-cameras.mjs` checks one camera viewport per level on
desktop and phone, Full view and Auto view, edge panning and the level strip,
rotation, and a retained-state hit near the far end of Level 1 using the
hash-verified `level-one` history. `tests/level-finish.mjs` checks that
the map moves to Level 2 within half a second of the first batch that clears
Level 1 in Twin Dragons and Tobu Tobu Girl, and that
the level strip moves the view in both. `tests/smb.mjs` also checks that
cleared levels collapse into the +N button and that nothing scrolls sideways
at 1,280 and 390 pixels wide. `tests/mm2.mjs` boots every stage, builds three
maps, replays a Crash Man state exactly, compares the Rust and browser
decoders, checks that the stage map is drawn when the first rollouts appear,
and checks that the stage map and the top and bottom edge chevrons move the
view.
`tests/metroid.mjs` does the same for Metroid's five area maps.
`tests/replay-pause.mjs` checks that the path
counter stays still while a visitor's replay plays and advances during the
automatic replay pick and after the replay pauses, ends or closes.
`CHROME_CHANNEL=chrome` uses an installed Chrome, and
`DEMO_URL` targets another server.

`Checks / Dissonance Workloads / Twin Dragons Browser` rebuilds and tests the
static app, then uploads `twin-dragons-browser-dist` and browser evidence. No
generated assets or ROM are checked in.

## Credits

Twin Dragons is by Garydos, with code under the zlib license. Some artwork is
by surt under CC BY 3.0, and the music is by Garydos under CC BY 4.0. This
build is altered: the sound-effect data is zeroed, and the sound effects by
Shiru are not included. [CREDITS.md](CREDITS.md) and the site's Credits dialog
hold the full notices. The site ships the ROM, the upstream zlib license, the
credits, the QuickNES source and build recipe, Harmony's source and the Rust
dependency sources in `licenses/`.
