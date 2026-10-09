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

A six-step guided tour spotlights the live heatmap, a real populated cell and its
retained routes, replay controls, human takeover, search admission and the branch
selector. The rest of the interface dims without blurring the game. The tour
opens once after the initial search has populated a cell; Guided tour in the
header reopens it. A versioned localStorage marker remembers dismissal; unavailable
storage falls back to one offer per page load. Skip tour and Escape always exit.
Native modal focus, step announcements, compact phone callouts and stationary
spotlights support keyboard use and reduced motion. Callouts follow the actual
elements through scrolling and resizing, without covering the highlighted control.
Search continues during the heatmap introduction, then pauses while inspecting
an authentic route. Exit restores the prior search pause state and any pre-existing human history. The tour never
creates a search branch or records game inputs on its own; the optional final
Try playing button invokes normal takeover after an explicit click.

Search starts automatically. Recent activity warms cells from blue through green,
orange and red, with a six-second half-life measured only while that search runs.
Heat is a UI view of actual visits, not archive retention or parent-selection
priority. Pausing, human play and switching to another branch freeze its heat;
cooled cells remain blue to preserve the explored footprint. The play/pause icon controls search;
Restart Search resets it. Heat is always on. Clicking a cell with retained states
opens a history inspector beside the maps, or a collapsible bottom sheet on phones,
listing its latest twelve retained states. Clicking a cell with no retained states
clears the previous selection and closes the inspector, leaving the maps visible
and keyboard focus on the map. Arrow keys and Enter also select cells. Each level
shows all its connected areas as stacked live maps: Introduction, Garden and
Main Level are visible together. Area labels and the world atlas can focus any map
immediately, including unvisited areas. Opening a visited area selects its furthest retained
state. Each room has its own zoom button and independent view; dragging moves
that room’s zoomed view.

The level heading latches actual campaign clear bits seen
across alternate histories. Green atlas cards record these witnesses; Watch Level
Finish replays the first witness for a new clear. Reaching a door is not a clear:
Nova needs a fresh Up press while touching it. A newly discovered next campaign
level brings its maps into view even after inspecting the preceding level. Game
Complete requires all 40 bits in one history, rather than the union of separate
branches. A budget stop is visible and does not claim a win.

A silent preview follows frontier discoveries until the visitor interacts. It
never draws a replay marker while the history pane is closed; only an intentional
branch origin remains highlighted on the search map. Empty-cell selection also
clears the previous replay marker.
Selecting a state replays its controller history from the level-one root in a
second emulator. Endpoint snapshots are compared byte for byte after
canonicalizing QuickNES's three unused PPU bytes. Verification runs internally,
without an on-screen badge. Scrubbing keeps the last complete screenshot visible
while the slider and an interpolated trail marker follow the requested frame.
An Updating frame indicator appears only if reconstruction takes over 120 ms;
interpolation never crosses a reload gap or a room transition. Takeover waits
for the actual emulator frame. The map follows the requested room as soon as the selected trail identifies it, then confirms the room from
the reconstructed game state. Crossing rooms or campaign levels focuses that
room and brings it into view; zoomed rooms center on the replay position.
Keyboard/cell selection keeps its retained-state list during this navigation.
Route selection keeps replay checkpoints and sampled trail points up to the exact
shared controller prefix, including equivalent split actions. It discards the
divergent future and immediately marks the new route selected. Reopening a route
or selecting a sibling can resume near the endpoint instead of replaying from
frame zero. Checkpoints remain one bounded cache across selections, at regular intervals targeting 64 snapshots,
with a hard 2 MiB phone / 4 MiB desktop limit. Seeks resume from the closest
preceding checkpoint or the current emulator frame, yield after approximately
8 ms of work, and cancel when a newer request arrives. Movies follow elapsed
wall time, render only their final presented frame, and update details at most
about seven times per second. Maps refresh at 30 Hz. Playback defaults to 1×.
User-started histories and takeover play the original music and sound effects,
with one mute control. Faster movies accelerate audio by the selected playback
rate. Audio stops on pause, seeking, selection, restart, completion, or a hidden
tab; ending takeover also stops its sound. The AudioContext is unlocked in the
play/watch click before any asynchronous reset. Automatic attract replay starts
silently until the visitor chooses to watch. Save/open-history and screenshot
controls are omitted from the interface. Search controls sit at the top left,
with a compact counter strip above the rooms. Neutral charcoal surfaces, system
typography and restrained controls keep the unchanged cold-to-hot heat colors
prominent. Route rows and atlas levels avoid
nested card borders; the light Play from here button remains the primary action.
A separate, lazily loaded emulator generates PCM because switching QuickNES
from its silent buffer changes serialized APU bytes. Only the original silent
emulator records controller endpoints; the reusable audio emulator never enters
search or snapshot verification. The selected history is traced in gold across its rooms, with
matching numbered entrance and exit markers at transitions. The position marker
follows the replay scrubber. The selected trail and one fixed origin trail per
retained branch are sampled at a fixed interval, each with at most about 8,400 points for an admitted history.
At most eight search presentations are retained and Restart Search releases them.

The inspector keeps the movie and a prominent 🎮 Play from here action above a
compact list of alternate routes to the selected spot. Route numbers are assigned
once in discovery order within each cell and search, and rows run oldest to newest.
The most recently retained choice is selected when opening a cell. The latest twelve choices keep
their original numbers when older choices leave the window; an inspected route
stays listed while selected, for at most thirteen rows. Repeat visits, reopening
a cell and branch switches do not renumber histories. Each retained state adds
one small ordinal entry to its cell, without retaining another snapshot. Rows
show replay durations and health/chips; the selected route has a checkmark.
Internal IDs and frame counts remain in tooltips and collapsed Game state
fields. The collapsed phone pane occupies at most 44% of the viewport, with a
small movie beside replay and branching controls, tight header spacing, and
visible attribution. Selecting a route or collapsing the pane brings its map
above the pane, including another state in the same room. Playing expands the
game automatically. Play from here pauses exploration and starts a new
history at the displayed frame, including frames in the middle of a recorded
action. Move with arrows or WASD, jump with Z or Space, and use an ability with
X; touch controls work on phones.
Stop playing, Escape, loss of focus or hiding the tab releases held inputs and
ends takeover. Branch search from here verifies
that the complete controller prefix reproduces the rendered endpoint, then
starts a separate explorer rooted there.
It preserves health, items, level progression and emulator state through replay,
without teleporting Nova or changing game RAM. After successful admission, the
pane closes and the mapped branch origin ripples for 1.2 seconds; reduced-motion visitors get a stationary ring. The map reveals
that room, including after expanded phone gameplay. The prefix trail to the
fork remains visible while the pane is closed; inspecting a history shows its
own complete trail. Each retained search has its own heat, cell history IDs,
room discoveries, progress and origin trail. Switching restores that search's
presentation, closes the history pane and resumes exploration. A search already
at its memory or completion limit stays stopped. Inactive heat stays frozen, and
returning does not add a synthetic root visit. The always-visible Search branch
selector shows Original search before any forks, becomes selectable when another
branch is admitted, and focuses the new branch after a fork. It returns to any
earlier search, preserving its archive and random generator. Taking control
or forking at an earlier scrubber position discards only that new history’s
future. Descendant histories include the original inputs, human intervention
and subsequent search inputs, and remain exactly replayable from the original
game root. The internal fork admission verifies the ROM, core and endpoint
checksum. Generated map PNGs carry attribution and license metadata.

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
the seed from an initial seed of 2, which has a recorded Level 2 witness under
the desktop budget. The smaller phone budget pauses this default run before
Level 2; phones can inspect and replay the retained exploration but are not
promised that transition within their budget.
Seed 1 is retained in regression diagnostics as a censored run, not described
as a successful clear. A history is bounded to 10,000 actions and 200,000 frames, matching
controller-history admission. Runs stop after 100,000 paths, approximately 20,000 historical
entries per search, or a shared compressed snapshot budget. Up to eight searches
are retained in one worker, with only one advancing at a time. Earlier searches
remain inspectable; their snapshots count toward the same global budget. The
final two-rollout batch can retain up to fourteen additional entries. Phones/coarse-pointer devices and devices reporting at most
4 GiB of RAM use 32 MiB of snapshots and 96 MiB of search WASM buffers; other
devices use 128 MiB and 192 MiB. The buffer check includes both worker-side Rust
and QuickNES memories, happens after each two-rollout batch, and can overshoot
by that batch's allocations. It excludes the separate 16 MiB replay emulator, the bounded checkpoint cache,
and a further 16 MiB audio emulator allocated after the first audible replay or takeover. Web Audio
PCM capture is capped at 9,600 stereo frames (200 ms at 48 kHz). Audio advances
in chunks of at most 12 game frames and drains after each chunk, including
12× movies on 30 Hz displays; queued playback
is bounded, flushed on release, and is not a recording of past audio.
In recorded desktop seeds, 11,400 snapshots occupied about 80 MiB compressed and
89 MiB of Rust linear memory. The 32 MiB phone snapshot budget therefore retains
roughly 4,000--5,000 similar states, not a guaranteed count.

Raw DEFLATE preserves every snapshot byte. Boxed slices discard spare compressor
capacity; retired selector entries still retain their snapshots. Decompression
is bounded to one MiB. The UI caches at most 32 fetched histories and 2 MiB of
estimated storage (64 bytes per action plus snapshot and record overhead);
current replay, the bounded human controller prefix, selected trail and in-flight
messages are separate. Branch ancestry metadata is sanitized on fork admission.
Evicted histories remain in the worker archive and can be fetched again. Full-resolution panoramas are held
only for the focused level's connected rooms. Atlas images are decoded one at a
time into 320 by 96 thumbnails, at most 6.7 MiB for all 57 maps; discarded full
images can take time to be reclaimed by the browser. Browsing every level no
longer retains about 156 MiB of decoded panoramas.

These limits do not measure total browser memory or guarantee immunity to mobile
OOM termination. Archive structures, JS objects, in-flight histories, image
decoding, canvases, rendering and the browser itself also consume memory. A limit
pauses exploration while retained states stay inspectable; Restart Search ends
the worker and releases its archive. Budgets do not guarantee game completion.

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
compressed snapshots within 6,000 paths per seed. Recorded whole-game Level 2
witnesses from seeds 2 and 3 reproduce their authoritative endpoint checksums.
A native archive check admits the real before/after observations and preserves
this transition's recorded input suffix. Restoring the Main checkpoint and
executing its original suffix also reproduces the same Level 2 snapshot.
The fixtures contain controller inputs,
not edited game state, and never affect live initialization. A small snapshot-budget stop preserves a late exact replay.

`NOVA_PROGRESS_PATHS=22000 npm run test:search` runs the longer three-seed
power-on diagnostic. Seeds 2 and 3 clear Level 1 and enter Level 2; seed 1 is
censored without a clear at that budget. This extended run was verified locally;
the routine CI check uses the cheaper whole-game chain plus recorded transitions and archive admission
to fit the registered 15-minute job limit. Checks export live witnesses as browser
evidence. The old
admission of only maps 0 and 40 rejected the garden door; map 40 is a bonus level.
Rooted search checks fork mid-action and from Main and Level 2, then verify both
the root snapshot and actual descendants against complete controller histories.
The UI check covers audible wall-clock 1×/4× playback, cached late scrubbing,
room following across Introduction/Garden/Main and into Level 2, audio
scheduling and mute, the drawer, independent room zoom, real keyboard and touch
inputs, human-prefix branching, resuming an earlier search without reopening
the pane, hidden-preview marker suppression, cell
selection, exact replay, frame zero, cancellation of long reconstruction,
stacked live-map selection, world browsing, tall-level layout, dragging, the
simplified top-left toolbar/counters, restart, mobile layout, credits and source
bundles. Browser-only worker interception supplies source-matched, replayed
fixture states through the normal selected-history response boundary; no fixture
or test API enters production. Actual fork messages and actual archive responses
verify human and descendant prefixes after removal of file controls. A native
and wasm32 selector fixture checks the same recorded choices with weights above 2^32,
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
