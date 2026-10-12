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
maps and the sprite sheet. It gets the ROM from
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

The browser driver is a bounded single-worker adapter. Its archive place is
(levels cleared, level, loading, x / 32, y / 32), and its identity within a
place is (x / 16, y / 16). Within a place it prefers more cleared levels, then
more health. A state with zero health or on the title screen ends a rollout.
The 32-pixel heatmap projection is separate from archive identity.

Rollouts have one to eight controller actions. The seed determines every search
choice; wall time changes only animation, heat decay and presentation. The
first search uses seed 3, and Restart increments it. Seed 3 enters Level 2
after about 3,000 paths. A history is bounded to 10,000 actions and 200,000
frames. Runs stop after 100,000 paths, about 20,000 archive entries, or a
shared compressed snapshot budget. Phones, coarse-pointer devices and devices
reporting at most 4 GiB of RAM use 32 MiB of snapshots and 96 MiB of search
WASM buffers. Other devices use 128 MiB and 192 MiB. A Twin Dragons snapshot is
12,792 bytes before compression, and 60,000 paths retain about 10 MB of
compressed snapshots. Up to eight searches are retained in one worker, with
only one advancing at a time.

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

The segmented view control offers Movement, Heatmap and Both. Movement shows
the level maps with small player sprites replaying recorded search rollouts.
One recorder samples the search emulator every four game frames, without
changing inputs, random draws or captured endpoints. Loading frames and frames
with zero health are hidden. Heatmap warms 32-pixel cells from blue through red
with a six-second half-life measured while that search runs. Both draws the
sprites over the heatmap.

Clicking a cell with retained states opens History, listing up to twelve routes
to that cell. Selecting a route replays its controller history in a second
emulator and compares the endpoint snapshot byte for byte. Play from here hands
control to the visitor: arrows or WASD move, Z or Space jumps and X breathes
fire. Branch search from here verifies the controller prefix and starts a
separate search rooted at that state. The Searches pane shows the branch tree.
Long levels wrap into broad sections under a sticky whole-level navigator. A
guided tour explains the search, routes, replay, human play and branching.

Replays play the game's music. The sound effects are absent from this build.

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
requires seeds 3 and 4 to enter Level 2 within 6,000 paths and exactly replays
each arrival. It replays the `level-one`, `level-two` and `level-three`
fixtures against their endpoint checksums, and checks that the Level 1 fixture
plus the rest of the Level 2 controller history reproduces the Level 2
snapshot. It also forks mid-action and from manual play in Levels 1 and 2 and
checks the audio emulator against the search emulator. The fixtures came from
real seed 3 and seed 5 searches and contain only controller inputs.
`TWIN_DRAGONS_PROGRESS_PATHS=25000 npm run test:search` requires seed 5 to
enter Level 3, which it does at about 18,500 paths.

The browser tests cover the swarm, heatmap, route selection, exact replay,
scrubbing across the Level 1 to Level 2 change, 1x and 4x playback, human play,
branching, the guided tour on desktop and phone, the level selector, wrapped
maps and the credits. `CHROME_CHANNEL=chrome` uses an installed Chrome, and
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
