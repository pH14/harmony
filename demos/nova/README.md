# Nova browser explorer

A static demonstration of state-space exploration. QuickNES executes Nova the
Squirrel's original pinned ROM in WebAssembly. A Rust worker uses the real
Dissonance `Archive` and parent selector. The source-derived atlas contains all
40 campaign levels, four bonus levels and their connected rooms. All execution
happens on the visitor's computer; hosting requires only static files.

## Run

Prerequisites: the repository Rust toolchain, `wasm32-unknown-unknown`,
wasm-pack 0.14.0, Emscripten 4.0.15, Node 22.19 or later, GNU coreutils, and
cc65 V2.19 (`ca65` and `ld65`). Activate Emscripten before building. On macOS,
put GNU coreutils in `PATH` and set `CC65_BIN_DIR` if needed.

```sh
rustup target add wasm32-unknown-unknown
cd demos/nova
npm ci
./tools/build-runtime.sh
npm run build
npm run preview -- --port 4173
```

Open http://127.0.0.1:4173. The production `dist/` is self-contained; serve it at
any URL path with a static HTTP server. ES modules and WASM require HTTP.
No shared-memory, cross-origin isolation, server computation or credentials are
required. Corresponding sources and build recipes accompany the ROM and core.

## Interaction

Search starts automatically. Recent activity warms cells from blue through green,
orange and red, with a six-second half-life. The play/pause icon controls search;
Restart Search resets it. Heat is always on. Clicking a visited cell lists its
latest twelve retained states. Arrow keys and Enter also select cells. Area tabs
and the world atlas can open any map immediately, including unvisited areas. Opening a visited area selects its furthest retained
state. Zoom focuses the selected state; dragging moves the zoomed view.

The goal names the focused level and its exit. The completion counter and green
level cards latch actual campaign clear bits seen across alternate histories.
Visited area tabs gain a check mark. Watch Arrival replays the first retained
door transition; Watch Level Finish replays the first witness for a new clear. Game
Complete requires all 40 bits in one history, rather than the union of separate
branches. A budget stop says Search Limit Reached and does not claim a win.

The attract-mode film follows frontier discoveries until the visitor interacts.
Selecting a state replays its controller history from the level-one root in a
second emulator. Endpoint snapshots are compared byte for byte after
canonicalizing QuickNES's three unused PPU bytes. Verification runs internally,
without an on-screen badge. Scrubbing reconstructs any frame. Save a PNG, or
export and reopen a JSON controller history. Movies are rendered live and are
currently silent. Imports verify the ROM, core and endpoint checksum. PNG
metadata and JSON exports carry attribution and license references.

## Boundaries

The browser driver is a bounded single-worker adapter using Dissonance's archive
retention, parent selector and Nova's spatial/resource key and chord policy.
Pending reloads have their own place identity and remain restorable: a healthy
door transition is not a death. All ROM map IDs and selected levels are admitted,
so room transitions and level-select screens can continue. Dead states end a
rollout. The 32-pixel heatmap projection does not replace archive identity.

This driver does not run the complete native `Campaign` scheduler: rollouts have
one to eight controller actions rather than adaptive campaign coordination,
continuation banks and checkpoints. Its seed determines search choices; wall
time changes only animation, heat decay and presentation. Restart increments
the seed. A history is bounded to 10,000 actions and 200,000 frames, matching
import admission. Runs stop after 100,000 paths, approximately 20,000 historical
entries, or 128 MiB of compressed snapshot payload (at most seven additional
entries in the final rollout). Raw DEFLATE compression preserves every snapshot
byte. Stored boxed slices discard spare compressor capacity, so the payload
count also bounds their backing allocations. This includes snapshots whose selector entries have retired. Decompression is
bounded to one MiB. Total memory also includes archive structures, emulators,
decoded images and UI storage. Budgets do not guarantee game completion.

The original level-one root uses the native adapter's pinned menu/setup sequence,
including resetting persistent progression to the first available level. Search
and replay then use normal controller inputs without changing game positions or
resources. The compiled QuickNES core is unmodified upstream plus
`tools/frontend.cpp`; its identity differs from the native core with its
idle-loop optimization. Browser tapes are not native Harmony recordings.
This demo does not depend on Consonance.

`tools/build-panorama.mjs` reads the pinned master level table for IDs and source
portal links for connected rooms. Occupied editor pages determine map extents;
empty editor padding is trimmed. Packed horizontal runtime pages project back
into source rows for tall levels. An offline camera captures each original map,
with build-only RAM writes and button presses to skip dialogs. Those snapshots
never enter live search or replay. Only non-reloading main-loop states whose map and loaded checkpoint belong to
the selected level contribute heat or area visits. End screens and level-select
menus still contribute clear witnesses without claiming visits to the next map.
Heat is actual controller-action endpoints,
rather than interpolated or fabricated visits. The marker and film show a
historical endpoint, rather than one continuously moving search agent.

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

The search check requires three real seeds to reach the garden (map 49) and
main area (map 45), and exactly replays both first-arrival tapes against their
compressed snapshots. It exports these witnesses as browser evidence. The old
admission of only maps 0 and 40 rejected the garden door; map 40 is a bonus level.
The UI check covers cell selection, replay, frame zero, scrubbing, attributed
PNG/JSON downloads, valid and tampered imports, cancelling a long import, direct
main/garden selection, world browsing, tall-level layout, dragging, the simplified
toolbar, restart, mobile layout, credits and source bundles. A native and wasm32
selector fixture checks the same recorded choices with weights above 2^32,
including 2^56 tiers. `CHROME_CHANNEL=chrome` uses an installed Chrome;
`DEMO_URL` targets a deployed subdirectory.

`Checks / Dissonance Workloads / Nova Browser` rebuilds and tests the static app,
then uploads `nova-browser-dist` and browser evidence. The shared Pages publisher
can place the artifact under `/nova/`. No generated assets or ROM are checked in.

## Credits

Nova the Squirrel is by **NovaSquirrel**. Game code is GPL-3.0-or-later; original
graphics, sound and gameplay imagery are CC BY-NC-SA 4.0 with upstream character
restrictions. This is a noncommercial software demonstration.
Creator, source and Creative Commons license links appear directly under the
map, replay and atlas. Full attribution, restrictions, pinned source references
and corresponding-source links are in
[CREDITS.md](CREDITS.md) and the site's Credits dialog. The build distributes
original sources and build recipes alongside the ROM and emulator.
