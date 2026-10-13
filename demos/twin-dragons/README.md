# Twin Dragons browser explorer

A static demonstration of state-space exploration. QuickNES runs the NES game
Twin Dragons in WebAssembly. A Rust worker uses the Dissonance `Archive` and
parent selector to search the game. The site has one map for each of the game's
three levels. All execution happens on the visitor's computer, so hosting needs
only static files.

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
worker holds its own QuickNES instance. The archive place is (level, loading,
x / 32, y / 32), its progress is the number of levels cleared, and its identity
within a place is (x / 16, y / 16). The place rows match the map's cell rows.
The parent selector weighs each progress tier 8 times the tier below, so new
work concentrates on the furthest level without a direction preference. Within
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
10,000 actions and 200,000 frames. Runs stop when a path finishes the game,
after 100,000 paths or 400,000 archive history entries, or when the search
WASM buffers reach their limit.

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
96 MiB of coordinator WASM buffers; other devices use 128 MiB and 192 MiB. A
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
Heatmap warms 32-pixel cells from blue through red with a six-second half-life
measured while that search runs. Cell rows start 8 pixels below a 32-pixel
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
glides about half a screen. A thin track along the bottom shows which part of
the level is on screen, with a gold dot at the point the camera is chasing.

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
desktop and phone, Full view and Auto view, edge panning and the level track,
rotation, and a retained-state hit near the far end of Level 1 using the
hash-verified `level-one` history. `CHROME_CHANNEL=chrome` uses an installed Chrome, and
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
