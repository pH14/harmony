# Nova browser explorer

A static, interactive demonstration of state-space exploration. QuickNES executes
Nova the Squirrel’s original pinned ROM in WebAssembly. A Rust worker uses the
real Dissonance `Archive` and parent selector; the view projects visited
endpoints onto the two rooms of level one. All execution happens on the visitor’s
computer. Hosting requires only static files, including GitHub Pages.

## Run

Prerequisites: the repository Rust toolchain, `wasm32-unknown-unknown`,
wasm-pack 0.14.0, Emscripten 4.0.15, Node 22.19 or later, GNU coreutils, and
cc65 V2.19 (`ca65` and `ld65`). Activate Emscripten before building. On macOS,
put GNU coreutils in `PATH` and set `CC65_BIN_DIR` if the assemblers are outside it.

```sh
rustup target add wasm32-unknown-unknown
cd demos/nova
npm ci
./tools/build-runtime.sh
npm run build
npm run preview -- --port 4173
```

Open http://127.0.0.1:4173. The production `dist/` directory is self-contained;
serve it at any URL path with a static HTTP server. ES modules and WebAssembly
require HTTP, rather than opening `index.html` as a file. No shared-memory,
cross-origin isolation, server computation or credentials are required. The
build includes both the original ROM and its corresponding source archives.

## Interaction

Search starts automatically. Recent activity warms cells from blue through green,
orange and red, with a six-second half-life. Pausing search lets heat cool while
histories stay accessible. Clicking a visited cell lists its latest twelve
retained states; different resources or controller histories can reach the same
place. Arrow keys and Enter also select cells. Room, zoom, pan and fit controls
navigate the map.

The attract-mode film follows frontier discoveries until the visitor interacts.
Selecting a state replays its controller history from the level-one root in a
second emulator. Endpoint snapshots are compared byte for byte after
canonicalizing QuickNES’s three unused PPU bytes. Scrubbing reconstructs any
frame. Save a PNG of the current frame, or export and reopen a JSON controller
history. Movies are rendered live and are currently silent. Saved histories
name the ROM and core revisions, contain actions rather than executable code,
and carry an endpoint checksum verified on import. PNG metadata and JSON exports
carry the author credit, CC license and upstream source references.

## Boundaries

The browser driver is a bounded, single-worker adapter. It uses Dissonance’s
archive retention and parent selector, with Nova’s spatial/resource key and
controller chord policy. It does not run the complete native `Campaign` scheduler:
rollouts contain one to eight controller actions rather than its adaptive
campaign coordination, continuation bank and checkpoints. Its seed determines
search choices; wall time changes only animation, heat decay and presentation.
Starting fresh increments the seed and resets the archive. Runs stop after
20,000 paths or approximately 4,000 historical entries (at most seven additional
entries in the last rollout). Snapshots are retained separately for every browser
history, even when the selector retires its endpoint; the bound keeps this near
84 MiB plus emulator, archive and UI storage.

The initial level-one root uses the native adapter’s pinned menu/setup sequence,
including resetting persistent progression to the first available level. Search
executes normal controller inputs afterward. No game positions or resources are
changed during search or replay. Dead/reloading states and departures from the
first level end a rollout. The heatmap’s 32-pixel display cells do not replace
archive identity or resource preferences.

`tools/build-panorama.mjs` captures the static backdrop from the original emulator
by moving an offline camera through the level. Those build-only camera snapshots
never become live search states. Heat measures observed controller-action
endpoints, rather than interpolated or fabricated visits. The marker and film
show a selected historical endpoint, not one continuously moving search agent.

The QuickNES browser build is unmodified upstream plus `tools/frontend.cpp`.
Its runtime identity differs from the native Harmony core with its idle-loop
optimization. Browser tapes are not native Harmony recordings. This demo does
not depend on Consonance; the Linux demonstration is a separate application.

## Verification and publishing

```sh
npm test
cargo test --locked --manifest-path rust/Cargo.toml
cargo clippy --locked --manifest-path rust/Cargo.toml --all-targets -- -D warnings
npx playwright install chromium
# While the preview server is running:
npm run test:browser
```

The browser check executes real search, selects a nonempty history, checks an
exact endpoint replay, scrubs back and forward, downloads a PNG and history,
reopens that history, checks credits, room controls, restart and a mobile layout.
`CHROME_CHANNEL=chrome` can use an already installed Chrome. `DEMO_URL` can point
the check at a deployed subdirectory. It verifies liveness and replay, without
requiring a specific discovery or completion outcome from a chosen seed.

`Checks / Dissonance Workloads / Nova Browser` rebuilds and tests the static
application, then uploads `nova-browser-dist` and browser screenshots. The shared
Pages publisher can place that artifact under `/nova/`; no build outputs or ROM
binaries are checked into Git.

## Credits

Nova the Squirrel is by **NovaSquirrel**. Game code is GPL-3.0-or-later; original
graphics, sound and gameplay imagery are CC BY-NC-SA 4.0 with upstream character
restrictions. This is a noncommercial software demonstration. Full attribution,
license restrictions, pinned source references and corresponding-source links
are in [CREDITS.md](CREDITS.md) and the site’s Credits dialog. The build distributes
the original sources and build recipe alongside the ROM and emulator.
