# Harmony browser explorer

A static demonstration of state-space exploration. Emulators compiled to
WebAssembly run a game, and a Rust worker uses Dissonance to search it. The
site plays the NES homebrew games Twin Dragons and Famidash on QuickNES, the
Game Boy game Tobu Tobu Girl Deluxe on SameBoy, and Super Mario Bros., Mega
Man 2 and Metroid on QuickNES from ROMs the visitor supplies. The `game` URL
parameter picks the game, and the level selector switches between them. The
page opens the level selector and starts searching once a level is chosen, or
from the first level if the selector is closed. A `level` URL parameter with a
level id starts that level's search at once. Each
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
leaves emulated state and timing unchanged. The core also carries the native
build's four-screen state patch, which the browser build extracts and checks
the same way. Without it the core cannot restore a state of a four-screen
cartridge such as Famidash. It gets the Twin Dragons ROM from
`workloads/nes/scripts/build-twin-dragons-rom.sh`, which downloads the pinned
upstream release and zeroes its sound-effect data. The same script's pinned
source archive supplies level data, tiles and tables for the maps. The build
extracts only those files into the gitignored `.build/` directory and never
publishes the archive. `workloads/nes/scripts/build-famidash-rom.sh` downloads
the four official Famidash 1.2.8 release ROMs, the MIT license, the music
credits and the music metadata and checks their SHA-256. The site ships the
ROMs unchanged.

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

The world x position is `$52 * 256 + $57 + $62`. The decoder is
`decode_state` in `workloads/nes-games/src/twin_dragons/target.rs`, and the page
calls it through the `twin_dragons_observation` export of the Rust worker. The
game is complete when the third level is cleared.

## Search

The search runs in one coordinator worker and a pool of rollout workers. Each
rollout worker holds its own QuickNES instance. The page starts every worker and
hands the coordinator a `MessageChannel` port to each rollout worker, because
some browsers fail to load a worker started from inside another worker. The
workers import `emulator.js` dynamically and set their message handler first.
The Famidash module waits for the Rust module when it loads, and a module
worker drops messages that arrive before its handler is set.

Twin Dragons, Famidash, Super Mario Bros., Mega Man 2 and Metroid run the
same search as their native workloads. `rust/src/campaign.rs` holds `JsMachine`,
a `machine::Machine`, `NesBackend` and `NesMemory` that drives the browser
QuickNES core through `globalThis.harmonyCores`, and a coordinator and rollout
worker that are generic over `BrowserGame`. `BrowserGame` is the `nes-games`
`ArcadeGame` over `Traced<JsMachine>`; `ArcadeGame` holds each game's view,
motion samples and level count, so a native live campaign draws the same page.
Each game's file (`rust/src/twin_dragons.rs`, `rust/src/famidash.rs`,
`rust/src/smb.rs`, `rust/src/mega_man.rs`, `rust/src/metroid.rs`) adds its root
snapshot and the page's observation functions. `CampaignSearch` and
`CampaignWorker` take the game id. The coordinator worker owns a Dissonance
`CampaignCoordinator`. It sends each planned `JobSpec` to a rollout worker as
postcard bytes; the rollout worker runs it with `feed::run_noted` and returns
the `JobOutcome` and its `Note` as bytes. The coordinator admits outcomes in
reservation order, whatever order they arrive in, so the seed alone decides the
search. The browser always uses an admission window of 16 and records workers
as 16, independent of the device's worker count, so its decision stream equals
the native campaign run with 16 workers. The archive key, draw, milestones,
admission rules and objective are the workload's; the game READMEs under
`workloads/nes/src` describe them. Famidash draws from the controller alphabet
like its native run; the other games use the `energy_splice:6` mixture of
`nes-eval`, the native Twin Dragons campaign and the search benchmarks. The
coordinator's `feed::Feed` releases a trail with the batch only once its job is
admitted. Twin Dragons archives the level-load frames in stages; the other
games do not, so a clearing rollout there often ends before the next level
loads.

### Live view

`?live` replaces the browser search with a native campaign. `nes-eval` with a
`live` request serves the built page, its `roms` directory and the campaign's
event stream from one loopback address, and `src/live-source.js` reads that
stream in place of the coordinator worker: the same `ready`, `batch`, `states`
and `limit` messages, without rollout workers. Open
`http://127.0.0.1:8790/?game=super-mario-bros&live` on the host, or forward the
port over SSH and open the same address on another machine. Pause stops drawing
new results and leaves the campaign running. A live view cannot branch. Build
the page with `npm run build` and pass the `dist` directory, with a `roms`
directory added beside `index.html`, as the request's `live_site`.
[`workloads/nes`](../../workloads/nes/README.md#live-view) documents the request
fields and routes.

A target can press buttons the archived input does not hold: the Mega Man 2
whole-game target walks through the weapon screen and stage select after each
boss. `Traced` records every action the core runs, and the feed keeps each
state's frame count. When a state's frame count differs from the sum of its
archived actions, `feed::archived_state` replays the input through a fresh
target and returns the recorded actions, so replay, forks and Play from here use
the actions the core really ran. The native live view uses the same rule.

Tobu Tobu Girl runs on SameBoy and has no `nes-games` workload, so it uses the
older `Explorer` in `rust/src/lib.rs`, with its decoder and archive key beside
it. Rollouts have 1 to 24 controller actions. Each action holds its buttons for
2 to 12 frames or 6 to 18 frames, with equal odds. The coordinator plans
rollouts in rounds of 16, four rounds ahead, and merges results in plan order.
Rollout workers return the 1 KiB work RAM bank and the raw snapshot after each
action, plus a
motion trail. Each worker caches up to 64 parent snapshots by id.

The first search uses seed 27, and Restart increments it. Across seeds 27 to
36 the native Twin Dragons campaign, which makes the same decisions, clears
Level 1 after a median of 6,300 executions, Level 2 after 37,000 and the game
after 89,000; every seed finished within 94,000. A coordinator job averages
about 17 frames. A history is
bounded to 10,000 actions and 200,000 frames. Runs stop when a path finishes the
game or when the search WASM buffers reach their limit. Pause stops sending new
rollouts and holds results until resume.

Desktops run `navigator.hardwareConcurrency - 2` rollout workers, at most 8, so
the page and the coordinator each keep a core. WebKit throughput drops about 4
times when the rollout workers use every core: on 8 cores of the machine below,
6 workers run 132,000 frames per second and 7 run 30,000.
Phones, coarse-pointer devices and devices reporting at most 4 GiB of RAM run
2 or 3. The `workers` URL parameter overrides the count, from 1 to 16. The
Details pane shows the count. Each rollout worker holds a 16 MiB QuickNES heap.
Small devices use 32 MiB of snapshots and 96 MiB of coordinator WASM buffers;
other devices use 1,024 MiB and 1,536 MiB. For Twin Dragons the snapshot figure
is the coordinator's `memory_budget_mib`, which evicts snapshots and replays
from an ancestor when it binds. A binding budget changes the search, so the
search reproduces for a given seed and device memory class. Each admitted state
keeps a 64-bit digest of its raw snapshot, so replays verify exactly. Up to
eight searches are retained in the coordinator, with only one advancing at a
time on all rollout workers. Rollouts already sent for a search still merge
into it after a switch.

The coordinator sends at most 120 rollouts per second for Famidash and Tobu
Tobu Girl and 300 for Twin Dragons and the Nintendo games, so a viewer can follow the
search.
The `pace` URL parameter sets the starting rate in rollouts per second, and
`pace=0` removes the limit. The Timelines per second slider at the top of
Details changes the rate while the search runs: 10, 30, the game's pace, 300, 1,000 or unlimited.
The browser tests run with `pace=0`, and the throughput
figures below are unpaced.

Throughput scales with the worker count. In Chrome on an Apple laptop under a
load average of 10 to 25, the Twin Dragons search ran about 122,000 game frames
per second (2,200 executions per second) with 4 rollout workers and 160,000
(2,800) with 8. Nearly all the time goes to the emulator. The core is built
with `-O3` and link-time optimization.

The map samples swarm sprites evenly when drawing falls behind. While display
frames run late and drawing the maps takes more than 6 ms, the sprite budget
shrinks by 10% per frame, down to 256; it grows back once frames are on time.

For a game with `campaign: true`, the first level's root is the workload's
genesis. `campaign_genesis` builds the workload's target over `JsMachine`,
records the controller actions that lead from power-on to its genesis, and
`boot(0)` replays those actions on the page's core, so the root equals the
native campaign's genesis and the screen shows its last frame. Other levels
come from the game module's `boot(level)`, which writes RAM on a fixed route.
For Twin Dragons Level 2 or 3, it writes the level to `$35` and sets `$04E3`
during Level 1 gameplay, then runs until the requested level is settled, the
goal flag is clear and the player is on the ground. Every root has full
health. The level selector starts a fresh search at the chosen root. Replay and
forks include the starting level, and the worker rejects tapes from a different
root. A later level, and every fork, starts from a
`CampaignOrigin::SnapshotRoot` built from that root. The coordinator first
checks that the workload's admission probe accepts the root.

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

Colors use the QuickNES palette after its RGB565 output conversion. The page
loads a shipped map when it first draws that map, so a game with many large
maps holds only the ones on screen. Map and
sprite PNGs carry the game and artwork credits in their text metadata.

## Interaction

The page opens with a one-line pitch, one live total (timelines tried across
every search branch) and Take the tour. The per-search counters and the
rollout worker count sit in a collapsed Details disclosure at the bottom of the
Searches pane. The footer
holds the Harmony link, Credits & source and the Dark mode / Light mode switch.

Two toggles, Movement and Heatmap, choose what the map shows. Either or both
can be on, and the last one on stays on. Movement alone is the default.
Picking a level the search has already cleared switches to Heatmap, and
picking the live level again restores the earlier choice.
Movement shows the level map with small dragon sprites replaying recorded
search rollouts. Each rollout worker samples its emulator every four game
frames and at each action's end, without changing inputs, random draws or
captured endpoints. Loading
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
the search frontier (the 80th-percentile x of live sprites, smoothed), and
stays where it is while fewer than four sprites are in the level. Sprites replay
rollouts from their start states, so the frontier drops back whenever the
search draws earlier states; the camera moves back only after fewer than two
sprites have been near its target for 1.5 seconds, and it closes a large gap
faster than a small one. Following pauses
for 3 seconds after the mouse moves over the map and for 2.5 seconds after a
touch, so a pointer left resting on the map does not stop it. A drag, pinch or edge pan holds the camera where the
visitor left it, and following resumes 10 seconds after the last movement.
On a touch screen a one-finger drag on a zoomed view pans it sideways, and up
and down as well when the level is taller than the view; on a level that fits,
a vertical swipe scrolls the page.
Selecting a cell stops following. Hovering the map shows chevrons at its left
and right edges; moving the mouse onto one and resting there pans that way, so a
layout change that leaves a still pointer over an edge does not pan, and a tap on a touch screen
glides about half a screen. A mouse click on a chevron selects the cell
beneath it. A strip under the view shows the whole level, with
the heat cells when Heatmap is on, each live sprite as a dot when Movement is
on, and a box for the
part on screen. Clicking or dragging the strip moves the view there, and a
sideways scroll on the map or the strip pans it. When a level is taller than
the view, as in Famidash, chevrons at the top and bottom edges pan it up and
down, and the strip moves the view on both axes. Outside Play from here, the
arrow keys pan the view the same way the chevrons do, including when the map
has focus. Enter on the map inspects the cell under the pointer, or the cell
at the center of the view. A room the visitor has not
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
Super Mario Bros. bonus areas, tabs above the view switch between them. While a
clearing rollout that started in the shown level plays, the view picks the one
with the least time left and shows its room until that rollout ends, so it
follows one rollout through intro scenes, exit pipes and warp zones. Otherwise
the view moves to a room that has had 1.5 times as many live sprites as the
shown room for 2.5 seconds and then stays there for at least 10 seconds, and one
rollout entering a bonus room leaves the view on the busy room. A tab click
holds the view on that room until the visitor clicks the live level in the
level trail.

One toolbar sits above the map. New search is on the left, then the level
trail, and the Movement and Heatmap toggles are on the right. Every control in
the toolbar has the same height. The level trail lists the levels on the
followed path in the order the view reached them and names the shown level, so
the map has no separate title. A check mark means a rollout finished that
level. Once the trail holds four or more levels, all but the last two and the
shown level collapse into a +N button that expands the full list. The trail
stays on one row and scrolls sideways, fading at a clipped edge, and it scrolls
to keep the shown level in view as the search moves on. When the exploration
column is narrower than 620 pixels, New search shrinks to a + button and the
toggles move to a second row.

A rollout that raises the cleared-level count is a clearing rollout, and the
level it enters first is its exit. For Super Mario Bros. the exit of a warp
pipe is the warp's destination. The view follows one path through the game: it
moves to a level only when a clearing rollout that starts in the level the path
is on arrives, and it moves to that rollout's exit once the exit has at least
three live sprites, so the view never shows a level the search has not started
exploring. Levels that other
lineages reach, such as the worlds past a warp zone the path did not take, do
not move the view. When the path's level has had no live rollouts for 8 seconds
while a deeper level has some, the path moves to the deepest such level. A
search whose root has already cleared its level, such as a branch started after
the flag or the axe, moves as soon as a deeper level has live rollouts. The map
always draws clearing rollouts and the camera follows the one that leaves the
level first. The automatic
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
history limits, and supplies `decode` and `boot`. `src/emulator.js` lists the
games and cores. Every NES game has `campaign: true`: it decodes in Rust through
its `*_observation` export and searches with its `nes-games` workload through
`src/campaign.js`. Tobu Tobu Girl, on the older `Explorer` path, also supplies
`ended`, `motion`, `levelComplete` and `transition`, and `tests/tobu.mjs` checks
that its JavaScript and Rust decoders agree. `maxActionFrames` and
`menuButtons` bound the controller actions an imported history may hold;
Mega Man 2, Metroid and Tobu Tobu Girl allow Start and Select.

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
the view. Climb is `$233 * $237 + $230`. Map coordinates are x =
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

### Famidash

Famidash by TFDSoft is an unofficial fan demake of RobTop's Geometry Dash. The
site ships the four official 1.2.8 ROMs unchanged: the main game and the
B-Sides, C-Sides and D-Sides. The picker shows one Famidash entry. Its level
selector lists all 109 levels in five groups: Main (22), Community (21),
B-Sides (31), C-Sides (25) and D-Sides (10). The main game ROM holds the main
and community levels. Each group is its own entry in `GAMES`, with the id
`famidash`, `famidash-community`, `famidash-b`, `famidash-c` or `famidash-d`
and the family `famidash`. A card of another group opens that group's page
with `?game=` and `?level=`, and that page loads the group's ROM.

`src/games/famidash.js` builds the five games from the `famidash_groups()`
export. Level names, ROM files and checksums come from
`workloads/nes-games/src/famidash/levels.rs`, and the map tops come from
`workloads/nes-games/src/arcade/famidash.rs`. In Rust each group is the game
type `Famidash<GROUP>`, where `GROUP` indexes `GROUPS` in `levels.rs`, so the
static `ArcadeGame` functions know the group's side.

A level is complete when the game state reaches 3. A search plays its group's
levels in order and its goal is completing the group's last level. After an
earlier level, the workload target presses A, Right and A through the win
screen and level select to start the next level. Those presses are part of the
history, so replay, forking and Play from here use them, and the map follows
the search into the next level. The page decodes RAM through
`famidash_observation(game, ram)`; the workload README in
`workloads/nes/src/famidash` lists the addresses. The observation carries the
level percent, the scroll divided by the level's length, and the Details pane
shows it as Level progress. One A button is the whole controller.

Map coordinates are the world x and the world y minus the level's map top.
`MAIN_MAP_TOPS`, `B_MAP_TOPS`, `C_MAP_TOPS` and `D_MAP_TOPS` in
`workloads/nes-games/src/arcade/famidash.rs` hold the top of each level on
each ROM, and `famidash_groups()` exports them with the level names. Each map
covers the rows the camera showed on the recorded route plus 16 lines above
and below, from 256 lines for Oceane to 905 for Motion, and is as wide as the
level's scroll plus one screen, from 3,843 pixels for The Challenge to 73,130
for Every End. Map x is 16 bits, so a point past x 65,535 in Every End shows at
that column.
The pose is the game mode (cube, ship, ball, UFO, robot, spider, wave, swing)
times two plus flipped gravity.

`tools/build-famidash-maps.mjs` builds the maps when the runtime is built,
because the site ships the ROMs. Each group's maps, `maps.json` and sprite
sheet go to `public/famidash/<group>/`. `tools/famidash-routes/<side>.json`
holds one clearing route per level of that side's ROM. A route is a string of
8-frame input patterns, or `{"inputs": [[buttons, frames], ...]}`, the
controller history of a Dissonance search that cleared the level. Each route
starts from the level start of `boot(level)`. The build
replays each route once on one thread and renders every 16th frame. Before
each frame it sets the parallax phase at `$04A0` to 0 and copies the backdrop
color at `$01A0` to the parallax colors at `$01A1` and `$01A9`, so the
parallax art draws as the flat backdrop and the background stays still. It
composes each frame at the scroll and camera of the previous frame, because
pixels show the RAM of one frame earlier. Each map pixel keeps the frame where
its column was nearest screen x 176, ahead of the player, so trails and the
player stay out of the map. Pixels the camera never showed take their column's
most common color. The build fails if a route no longer clears its level or
a map top differs from the Rust tables, and prints the new tops.

`node tools/build-famidash-maps.mjs --search [group [levels]]` searches routes
for the levels that have none, or for the given levels. The group is `main`,
`community`, `b`, `c` or `d`. The search plays the plain pattern until a
death, then retries one or two other patterns a few chunks back, then a
12-wide beam search, and checks two chunks past the repair. Some levels need a
press at a frame the patterns never use. For those the search runs again, and
where no repair passes a death it plays one plain chunk with deaths ignored,
written as `a` in the route. Before each frame of that chunk the build sets the
game's debug mode at `$0595` to 3, which skips the death reset, and the
invincibility counter at `$04A1` to 8, which skips wall and spike collisions.
After each frame it clears the debug mode and the death bits. A route also
ends once the level percent has stayed at 99 for 16 chunks, because on some
levels a route that ignored a death runs under the end trigger and never shows
it; the map is complete at that point.
`node tools/build-famidash-maps.mjs --inputs <group> <level> <victory-input.json>`
records the victory input of a Dissonance run as the level's route after
checking that it clears the level. The build sets the parallax pokes for these
routes too, unless the input clears the level only without them; then the
route records `"parallax": true` and the map keeps the parallax art. Of the 109
routes, 78 are pattern strings, 17 have chunks with deaths ignored and 14 are
Dissonance victories. The sprite sheet comes from each group's first level
with the background switched off through the PPU mask copy at `$0B`: the build
writes each game mode to `$7A` and captures the player. The flipped-gravity
row is the same sprite turned upside down. The 109 maps take about 63 seconds
of CPU time.

`tools/build-famidash-music.mjs` reads the four pinned music metadata files
and writes `public/famidash/music.json`. The Credits dialog lists each side's
songs with the original artist and the arranger the metadata names.

`boot(0)` replays the campaign genesis, the target's own setup tape.
`boot(level)` for any other level runs the group's level-select tape from
`famidash_level_select_tape(game)`, writes the level to `$047B` when the menu
shows another level and runs `famidash_start_level_tape()`, the workload's own
A press and 40-frame wait. The community group and the side ROMs open the
community menu, so their tape presses Right on the title menu.

The native search with 16 workers clears Stereo Madness in 10,900 to 14,700
executions on seeds 1 to 5, so at 120 rollouts per second the browser search
reaches Back on Track in about two minutes.

### Super Mario Bros.

The site ships nothing from Super Mario Bros. The level selector accepts the
ROM with SHA-256 `0b3d9e1f…6dea`, the same ROM as `workloads/nes`. The page
keeps it in IndexedDB and never uploads it. A visit without a stored ROM shows
Twin Dragons and asks for the file.

The game picker lists the games the site ships under Homebrew and the games
that need a visitor's ROM under Bring Your Own ROM. Choosing a game that
needs a ROM opens a page that takes
the file by drag and drop or a file picker. A game with a `playthrough` entry
also shows that YouTube video below, embedded from youtube-nocookie.com
through the IFrame Player API, which sets the playback rate to 2. Super Mario
Bros., Mega Man 2 and Metroid each have one.

A private build can ship ROMs beside the page. At startup the page reads
`roms/manifest.json`, an object whose `games` lists game ids and whose optional
`default` names the game a link without `game` opens. It loads `roms/<id>.nes`
for a listed game that has no stored ROM. The file must match the game's size
and SHA-256. A listed game skips the ROM page. The public site has no manifest,
so the request fails and every game asks for its ROM.

The search is the `SmbGame` workload from `nes-games` with the
`nes_pressable_36` vocabulary and the game-victory objective, as `nes-eval`
runs it. `workloads/nes/src/smb/README.md` describes its RAM map, archive key
and admission probe. A rollout ends at death or at the end of the game; flag
poles and castles do not stop it. `rust/src/smb.rs` builds the page's
observation from the workload's decoders in
`workloads/nes-games/src/smb/target.rs`: world and level, player position,
death, the flag sequence, play mode and the area bytes `$074E` and `$074F`.

The 32 levels are the map ids, world × 4 + level. A state in another area,
such as a bonus room or the World 8-4 underwater section, gets the map id 32 +
level × 128 + area. The table of each level's main area lives in
`rust/src/smb.rs`, and the page reads it through `smb_main_area`. The page
builds that area's map the first time the search enters it and shows it as
another room of the level. The builder enters an area by writing `$0750` (area
pointer) and `$0751` (entrance page) and setting `$0772` to 0. Pipe scenes and
other non-play tasks count as loading. A frame where Mario enters a pipe also
counts as loading, because a warp pipe writes the destination's world and level
about 50 frames before the area changes, so those frames would read as the
destination level holding the old area. An intro scene, such as the walk into World 1-2's
pipe, never hands Mario control, so the builder starts on it once Mario appears. For a later level, `boot(level)` writes
the world, level and area number on the title screen, presses Start and waits
until Mario stands on the level.

The page builds each map in a worker from the ROM. The builder holds Mario at
a fixed height, hides sprites, clears enemies and loop checks, holds Right
and B, and copies each newly scrolled column into the map. When Mario stops
moving it tries the next height. When Mario enters a pipe that leaves a level's
main area, or every height stops him, the builder returns to its last
checkpoint and holds him above the screen with the scroll lock (`$0723`)
cleared, which draws the World 1-2 warp zone past the exit pipe. It keeps the flag pole, so each map ends after
the flag sequence. Built maps are cached in IndexedDB by ROM hash. Maps build in a pool of
one map worker per four cores, at most three, and the shown level's map
builds first. Map widths
are fixed per level, because the page lays out maps before they are built.
Seed 27 reaches World 1-2 from World 1-1 after about 14,600 executions.
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
stage during the load.

The search is the whole-game `Mm2Game` workload from `nes-games`, as
`nes-eval` runs it with `whole_game`. `workloads/nes/src/mm2/README.md`
describes its archive key, chords, Start taps, weapon menu and admission
rules. The target picks each next stage itself after a boss.
`rust/src/mega_man.rs` builds the page's observation from the workload's
decoders. A level jump becomes a `CampaignOrigin::SnapshotRoot` only when the
workload's admission probe accepts it. A jump writes the weapons, items and
full weapon energy of every stage before it, so the Wily 1 jump matches a
player who has just beaten Quick Man.

The map builder flies Mega Man through each room at fixed heights, keeps his
health full, clears enemies, hides sprites and rolls back to a snapshot on
death. When he stops moving it moves him forward. It copies a camera frame into
the map whenever the camera is still in the expected room. It finds a room's
exit by trying a grid of positions with the exit's direction held, with
rendering off. A try ends once Mega Man stands still for 30 frames. Crash
Man's map takes about 2 seconds to build. The Wily 2, 3 and 6 maps stop
partway, because the builder does not yet find some of their exits.
`tests/mega-man.mjs` searches from the first stage and from the Wily 1 jump, replays
the farthest state exactly, and branches from a played history that opens and
closes the weapon menu with Start. It reads the ROM from `MM2_ROM` or
`~/workspace/roms/mm2.nes` and skips when it is absent.

### Metroid

The site ships nothing from Metroid. The level selector accepts the ROM with
SHA-256 `e6e6b701…65b6`. Metroid has one level, Zebes, whose five rooms are the
areas Brinstar, Norfair, Kraid's Lair, Ridley's Lair and Tourian. The search
starts from a new game, and the view follows it between areas the same way it
follows Super Mario Bros. between bonus areas. The observation's cleared count
adds items, energy tanks, bosses and the ending, which ranks search progress,
while the page marks Zebes finished, and offers to watch it, only at the ending.

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

The search is the new-game `MetroidGame` workload from `nes-games`, as
`nes-eval` runs it. `workloads/nes/src/metroid/README.md` describes its map
cell, archive key, chords and terminal policy. `rust/src/metroid.rs` places
Samus's map cell on the area map. Each area's map is the bounding box of its
cells, and the table of which area owns each of the 32 × 32 world cells comes
from a search over doors and passable screen edges. A state inside a door
transition is loading. A history may press Start, which pauses the game.

The maps are drawn from the ROM's level data with no emulation. The world map
at `$A53E` in bank 0 gives each cell's room. Each area bank holds the room,
structure and macro tables at `$959A`, `$959C` and `$959E`, and its palette
behind `$9560`. The pattern tables are copied in the order of the area's entries
in the table at `$C6E0`. Door caps are drawn as bars in their palette colors.
Enemies, items and elevators are sprites and are not drawn. Each screen is a
separate 256 × 240 bitmap, because a whole area is too large for one canvas on
a phone, and the strip beside the view draws a reduced copy.
`tests/metroid.mjs` searches from the new game, replays the farthest state
exactly, and branches from a played history that pauses and resumes with
Start. It reads the ROM from `METROID_ROM` or `~/workspace/roms/Metroid (USA).nes` and
skips when it is absent.

## Verification and publishing

```sh
npm test
node tests/core-oracle.mjs
cargo test --locked --manifest-path rust/Cargo.toml
cargo clippy --locked --manifest-path rust/Cargo.toml --all-targets -- -D warnings
wasm-pack test --node rust
npx playwright install chromium
# While the preview server is running:
npm run test:browser
```

`tests/levels.mjs` boots all three levels and checks health, progress, a fresh
search, exact replay, repeated boot and a rooted fork. `tests/search.mjs`
requires seed 1 to enter Level 2 within 12,000 executions and exactly replays
the arrival. It runs a seed with results admitted in order and in reverse order
and requires identical decision streams, digests and an exact late replay. It
replays the `level-one`, `level-two` and `level-three` fixtures against their
endpoint checksums, and checks that the Level 1 fixture plus the rest of the
Level 2 controller history reproduces the Level 2 snapshot. It runs a 3 MiB
memory budget for 6,000 executions and checks that the search keeps going and
still replays exactly. It also forks mid-action and from manual play in Levels 1
and 2 and checks the audio emulator against the search emulator. The fixtures
came from real searches and contain only controller inputs.
`TWIN_DRAGONS_PROGRESS_PATHS=25000 npm run test:search` requires seeds 1, 2 and
27 to enter Level 2. `tests/parallel.mjs` loads the page twice with three
workers and once with one, and requires the first 120 archived histories and
digests to match.

`node tests/campaign-oracle.mjs` runs each campaign game natively with 16
workers and in the browser coordinator in Node on the same seed and execution
budget, and requires every job and skip line of the two decision streams to be
equal. The native stream's closing end record must report the browser's
execution count. Twin Dragons runs `twin-dragons-campaign` for 600 executions. Famidash
runs `nes-eval` with `alphabet_only` for 10,000 executions. Super
Mario Bros. runs `nes-eval` with `energy_splice:6`, a window of 16 and a
1,024 MiB memory budget for 400 executions, Mega Man 2 runs it with
`whole_game` for 400, and Metroid for 160. `ORACLE_GAMES` (comma-separated
game ids), `ORACLE_SEED` (default 1) and `ORACLE_EXECUTIONS` set the run. A
game without a ROM at the path `tests/core-oracle.mjs` uses is skipped. It
needs the native core like `tests/core-oracle.mjs`. The workloads digest each
job result over its actions, observations, milestones, outcomes and candidate
keys, without the emulator state, so the two streams compare even though the
browser keeps raw QuickNES states.

`tests/media.test.js` checks the snapshot digest against the Rust
reference vectors.

The browser tests cover the swarm, heatmap, route selection, exact replay,
scrubbing across the Level 1 to Level 2 change, 1x and 4x playback, human play,
branching, the guided tour on desktop and phone, the level selector and the
credits. `tests/browser.mjs` runs one worker so its Level 1 checks finish
before the search reaches Level 2. `tests/room-cameras.mjs` checks one camera viewport per level on
desktop and phone, following, edge panning and the level strip,
rotation, and a retained-state hit near the far end of Level 1 using the
hash-verified `level-one` history. `tests/level-finish.mjs` checks that
the map moves to Level 2 within half a second of the first batch that clears
Level 1 in Twin Dragons and Tobu Tobu Girl, and that
the level strip moves the view in both. `tests/smb.mjs` also checks that
cleared levels collapse into the +N button and that nothing scrolls sideways
at 1,280 and 390 pixels wide. `tests/mega-man.mjs` boots every stage, builds three
maps, replays a Crash Man state exactly, compares the Rust and browser
decoders, checks that the stage map is drawn when the first rollouts appear,
and checks that the stage map and the top and bottom edge chevrons move the
view.
`tests/metroid.mjs` does the same for Metroid's five area maps.
`tests/famidash.mjs` boots all 109 levels and checks that each starts in the
first screen of its map, runs a 2,000 execution search from one random level
of each group with a random seed and replays its rightmost state exactly, checks the picker order and the grouped
level selector, starts a Stereo Madness search and waits for Level progress,
pauses, selects the root's cell, then plays from it and branches, and replays
the branch history to its endpoint checksum. It picks a random B-Sides level
from the selector, checks that its page searches the B-Sides ROM, and checks
that the credits list the music of every side. On Dash it checks that the
level strip and the top and bottom chevrons move the view up and down.
`tests/arrow-pan.mjs` checks that the arrow keys pan the view in Twin Dragons,
Famidash and Tobu Tobu Girl without taking control.
`tests/replay-pause.mjs` checks that the path
counter stays still while a visitor's replay plays and advances during the
automatic replay pick and after the replay pauses, ends or closes.
`CHROME_CHANNEL=chrome` uses an installed Chrome, and
`DEMO_URL` targets another server.

`node tests/core-oracle.mjs` checks that the browser QuickNES build emulates
exactly like the native core the workloads use. It builds the native core
into `.build/native-quicknes` unless `HARMONY_QUICKNES_CORE` names one, runs
`nes-core-oracle` from `workloads/nes`, and replays the same tape on
`public/engine/quicknes.wasm` one frame per call. It compares system RAM after
every frame, and at every action boundary also cartridge RAM (Famidash and
Metroid) and the
save state with the PPU padding bytes zeroed. It runs once with video and audio
off on both cores and once with both on. Each game runs its workload setup tape
and then `ORACLE_FRAMES` (default 8,000) frames of chords drawn with
`ORACLE_SEED` (default 1). Twin Dragons and Famidash always run. Super Mario Bros., Mega
Man 2 and Metroid run when `SMB_ROM`, `MM2_ROM` and `METROID_ROM` or the
default paths under `~/workspace/roms` name a ROM. The check needs the Rust
toolchain and a C++ compiler, so it runs locally and not in the browser lane.
A snapshot taken with audio on differs from one taken with audio off in the APU
block, so snapshot digests are comparable only between cores in the same audio
mode.

`Checks / Dissonance Workloads / Twin Dragons Browser` rebuilds the static app
once and uploads `twin-dragons-browser-dist`. Two browser jobs then run
`npm run test:browser:games` and `npm run test:browser:interface` against that
build and upload their evidence. `npm run test:browser` runs both. No
generated assets or ROM are checked in.

## Credits

Twin Dragons is by Garydos, with code under the zlib license. Some artwork is
by surt under CC BY 3.0, and the music is by Garydos under CC BY 4.0. This
build is altered: the sound-effect data is zeroed, and the sound effects by
Shiru are not included. [CREDITS.md](CREDITS.md) and the site's Credits dialog
hold the full notices. The site ships the ROM, the upstream zlib license, the
credits, the QuickNES source and build recipe, Harmony's source and the Rust
dependency sources in `licenses/`.

Famidash is by TFDSoft, copyright 2023-2024, under the MIT license. It is an
unofficial fan demake of Geometry Dash by RobTop Games. The site ships the
four official release ROMs unchanged with their license, music credits and
music metadata.
