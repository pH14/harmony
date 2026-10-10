# Nova browser explorer

A static demonstration of state-space exploration. QuickNES executes Nova the
Squirrel's original pinned ROM in WebAssembly. A Rust worker uses the real
Dissonance `Archive` and parent selector. The source-derived map catalog contains all
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

The segmented view control offers Movement, Heatmap and Both. Movement is the default.
Movement shows the original level artwork with
Nova sprites replaying recorded search rollouts together. Maps retain their
original colors and the original light background shade. Each sprite fades in
as its rollout starts and eases from nearly opaque to about two-thirds opacity
as it finishes, followed by two faint dots at its position six and twelve game
frames earlier. Sprites are drawn at least 18 CSS pixels tall, up to 2.5 times
their native size, so whole-room views stay legible. Main uses the original
palette; each branch recolors its sprites with its own tint (gold, pink, cyan,
green, violet, orange, teal) by blending hue onto the original luminance. The
same tint marks that branch in Searches, its fork ripple and its admission toast. Heat, grid and
sparks disappear in Movement; selected route trails, Nova markers, fork origins
and cell inspection remain available. Inspecting a populated cell keeps the
selected view and draws the route and Nova marker in all three modes. Opening
the guided tour starts with Movement. Both overlays the original colored sprites on the normal
heatmap, underneath selected trails and state markers. Inspection keeps Both
selected. The view selection survives branch changes and Restart Search.

Each sprite follows one new rollout from its restored parent, rather than a
complete root-to-state history. Incoming two-rollout batches animate once at
60 game frames per second from their arrival, then disappear and release their
sample arrays. There are no loops, staggered historical phases or endpoint fade.
This presentation follows completed search batches; it neither slows search to
real-time gameplay nor adds an emulator. Movement uses the same branch activity
clock as heat: pausing, human play and inactive branches freeze it. Returning to
a branch resumes unfinished attempts, without restarting completed ones.
Reduced motion keeps each incoming sprite at its starting pose for the same
bounded lifetime. Invalid gameplay, deaths and room transitions never interpolate
across maps or menus. Both uses the same one-shot attempts; changing views does
not restart them. Expired attempts are also released as new batches arrive in
Heatmap mode. Restart Search releases every branch's samples.

One recorder samples the existing search emulator every four game frames. It
splits silent runs into equivalent chunks without changing inputs, random draws,
archive selection or captured endpoints; no extra emulator or snapshot is retained.
Transferred Uint16 arrays store relative frame, room, position and pose. Their
shared FIFO cache across all eight branches is capped at 2 MiB / 2,048 rollouts
on phones and 8 MiB / 8,192 on desktops. Finished attempts are released; the
newest recordings replace the oldest if either limit is reached first. Inactive
branches share this same allowance.
The worker keeps only the current two-rollout batch, whose encoded payload is
at most 4,820 bytes before transfer. Live RAM reads use the pinned game's position, direction, ground state,
velocity and retrace addresses; map identity uses the same bank/checkpoint rules
as exploration. The build extracts idle, four walking and jumping poses, in both
directions, from upstream's spnova.chr and player.s, with the original palette.
The sprite sheet carries the same NovaSquirrel CC BY-NC-SA attribution as maps.

The page opens with a one-line pitch and two live totals: timelines tried and
gameplay time executed, summed across every search branch. The per-search
counters (timelines tried, moments saved, map cells reached, game frames played
and snapshot memory) sit in a collapsed Search details disclosure below the
maps.

On a visitor's first desktop visit the camera opens at 2.6× on the boot room,
follows the search frontier (the 80th-percentile x of live sprites, smoothed)
for about four seconds, then eases back to the whole level over 1.6 seconds.
The tour waits for that opening to finish. Take the tour replays the same
opening on desktop before starting the tour, unless the search is paused. Returning visitors, reduced-motion
visitors and any manual pan, pinch, zoom or cell selection skip or end it.

A five-step guided tour follows the story many, one, you, fork and tree:
Thousands of Novas on the live Movement map; Follow one timeline, which spotlights a populated cell, draws its selected route in gold
over 1.8 seconds with a moving head, and lights the History film, replay
controls and route list together; Your turn for takeover; Hand it back to
Harmony for search admission; and A tree of searches for the tree.
Each step has one or two short sentences and a row of progress dots. Every step enables its real highlighted controls: cell and route selection,
replay and scrubbing, keyboard/touch takeover, branching and switching searches.
The rest of the interface dims without blurring the game. The tour opens once
after the opening camera finishes and the search has populated a cell; Take the
tour beside the live totals reopens it. A versioned localStorage marker remembers dismissal; unavailable storage
falls back to one offer per page load. Skip tour and Escape always exit.
Nonmodal callouts use native inert to isolate unrelated controls, contain keyboard
focus within the enabled elements, announce steps and follow targets through
scrolling/resizing. Hovering a retained route previews its verified history as a stronger gold map
trail and an endpoint screenshot in the existing History screen, without
changing the selected emulator or adding a floating preview. Leaving the route
restores the selected frame; clicking commits that route. Route names, durations
and resources share one compact row, without a separate route count. The
route step spotlights those maps as well as the existing History screen and
route list. Selecting any cell or route outside the tour draws its trail the
same way. Preview tracing reuses exact shared-prefix checkpoints and keeps one
bounded sampled trail, rather than a trail per cached screenshot. The route
step keeps the route maps and gameplay fully lit. Phone replay places audio beside
Replay in one highlighted action row; gameplay keeps audio in the pane header. On phones, the
callout reserves its own space above the interactive stage (alongside it in
landscape). The gameplay screen scales to leave the controller and branch actions
uncovered; search admission pins Searches above the game. Spotlights clip to
visible content and exclude portions covered by other panes. Replay room changes
bring the active route map into the unobstructed stage.
Longer tour narration scrolls separately from persistent navigation, leaving
the real game and controls visible on small screens. Search continues during
the opening step, then pauses while inspecting a
route. Dismissal always resumes the selected search while preserving explicit
branch changes and any existing human history. Leaving gameplay by discarding,
collapsing History or admitting a search visits the branch-decision step before
Searches, even if the visitor acts early on the gameplay step. The branch-decision
step highlights Branch/Discard and the game screen while a draft is open; the
controller remains usable. After the draft closes,
the spotlight moves to Searches. The pinned phone Searches pane keeps its list
in a 44px scroll viewport, rather than letting a multi-branch list overflow the
56px pane. Its spotlight follows the actual rounded pane boundary without
illuminating the map below. `npm run test:tour-webkit` is an optional
Safari-engine regression check after installing Playwright WebKit; it compares
the action pixels with and without the shade in dark mode, verifies real tap
targets, and resizes through phone toolbar heights and landscape. Admission advances
from the step where it began; pressing Next while a branch is pending cannot
advance the tour twice. Clicking Branch
search from here performs normal verified admission. The tour itself never creates branches or inputs;
Play from here and all real controls require an explicit visitor action. Moving
from gameplay to search admission preserves the human draft and controller, even
if Nova has died; a dead endpoint can be discarded but cannot root a search. The
callout keeps Skip tour beside its title and finishes with Let’s go explore!

Search starts automatically. Recent activity warms cells from blue through green,
orange and red, with a six-second half-life measured only while that search runs.
Activity uses the real four-frame rollout samples, including the restored start
and cells crossed between action endpoints. Each attempt warms a cell once,
regardless of how many samples dwell there. No interpolated bridge is painted
across restores, room transitions or invalid/menu samples. Heatmap, Movement and
Both use the same recorded attempts; colors and the 32-pixel position grid are
unchanged. Retained endpoints attach their actual histories separately, without
warming the cell twice or inventing snapshots for transit cells.
Heat is a UI view of actual visits, not archive retention or parent-selection
priority. Pausing, human play and switching to another branch freeze its heat;
cooled cells remain blue to preserve the explored footprint. The play/pause icon controls search;
Restart Search resets it. Heatmap and Both show the same activity overlay. Clicking a cell with retained states
opens a history inspector beside the maps, or a collapsible bottom sheet on phones,
listing its latest twelve retained states. Mouse hover outlines and the pointer cursor identify only cells with retained histories; empty ground and transit-only cells keep the ordinary cursor. Keyboard selection can still inspect any grid cell. Clicking a cell with no retained states
clears the previous selection and closes the inspector, leaving the maps visible
and keyboard focus on the map. Arrow keys and Enter also select cells. Each level
shows all its connected areas as stacked live maps: Introduction, Garden and
Main Level are visible together. Area labels focus rooms within the current level. Opening a visited area selects its furthest retained
state. Each room has its own zoom button and independent view; dragging moves
that room’s zoomed view.

The level heading latches actual campaign clear bits seen
across alternate histories. Watch Level Finish replays the first witness for a new clear. Reaching a door is not a clear:
Nova needs a fresh Up press while touching it. A newly discovered next campaign
level brings its maps into view even after inspecting the preceding level. Game
Complete requires all 40 bits in one history, rather than the union of separate
branches. A budget stop is visible and does not claim a win.

A silent preview follows frontier discoveries until the visitor interacts. It
never draws a replay marker while the history pane is closed; only an intentional
branch origin remains highlighted on the search map. Searches identifies each
branch with a fork arrow, its immutable starting gameplay time (to a tenth of a
second) and room. These come from the verified native branch root, rather than
the selected replay or moving search frontier. Main keeps its name; exact frames
and stable IDs remain in accessible labels/tooltips, with parent-time context
when the tree exceeds three nested levels. Hovering a different search keeps
the chosen visualization mode: Movement and Both show its frozen last movement
frame, while heat and the branch-origin trail come from that same search.
Frozen poses are independent of the expiring rollout recordings and capped at
256 per phone branch / 1,024 per desktop branch (eight searches maximum).
Previewing never advances or switches a search. Empty-cell selection also
clears the previous replay marker.
Selecting a state replays its controller history from the selected starting-level root in a
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
controls are omitted from the interface. Search controls sit in the Searches pane; the
live totals sit beside the pitch above the maps. Warm charcoal surfaces, softer corners, system
typography and restrained controls keep the unchanged cold-to-hot heat colors
prominent. Route rows avoid nested card borders; the light Play from here button remains the primary action.
A separate, lazily loaded emulator generates PCM because switching QuickNES
from its silent buffer changes serialized APU bytes. Only the original silent
emulator records controller endpoints; the reusable audio emulator never enters
search or snapshot verification. The selected history is traced in gold across its rooms, with
matching numbered entrance and exit markers at transitions. The position marker
follows the replay scrubber. The selected trail and one fixed origin trail per
retained branch are sampled at a fixed interval, each with at most about 8,400 points for an admitted history.
At most eight search presentations are retained and Restart Search releases them.

Replay replays recorded inputs; 🎮 Play from here hands control to the
visitor at the displayed moment. The inspector keeps the movie and this primary
action above a compact list of alternate routes to the selected location. Routes are named Timeline 1, Timeline 2 and so on, numbered
once in discovery order within each cell and search, and rows run oldest to newest.
The most recently retained choice is selected when opening a cell. The latest twelve choices keep
their original numbers when older choices leave the window; an inspected route
stays listed while selected, for at most thirteen rows. Repeat visits, reopening
a cell and branch switches do not renumber histories. Each retained state adds
one small ordinal entry to its cell, without retaining another snapshot. Rows
show replay durations and health/chips; the selected route has a checkmark.
Internal IDs and frame counts remain in tooltips and collapsed Game state
fields. The collapsed phone pane occupies at most 44% of the viewport, with a
small movie beside replay and takeover controls, tight header spacing, and
a single artwork attribution below the main game maps. Selecting a route or collapsing the pane brings its map
above the pane, including another state in the same room. Playing expands the
game automatically. Play from here pauses exploration and starts a new
history at the displayed frame, including frames in the middle of a recorded
action. Move with arrows or WASD, jump with Z or Space, and use an ability with
X; touch controls work on phones.
Escape, loss of focus or hiding the tab releases held inputs and ends takeover;
closing or discarding the draft resumes exploration. Branch search from here
verifies
that the complete controller prefix reproduces the rendered endpoint, then
starts a separate explorer rooted there. This also applies when branching from
another branch: the new archive contains only the exact selected root, with a
fresh random generator and zero search work. Prior controller inputs remain in
the replay prefix, while the parent's archive and exploration stay separate.
Paths explored, states retained and game frames executed describe the active
search; the snapshot-memory tooltip describes the shared allowance.
It preserves health, items, level progression and emulator state through replay,
without teleporting Nova or changing game RAM. After successful admission, the
pane closes and the mapped branch origin ripples three times in the branch tint for 2.4 seconds; reduced-motion visitors get a stationary ring.
The camera centers the fork, holding a 2.2× close-up on desktop for 2.6 seconds
before easing back to the whole level, and a toast reports that the branch is
live. The new row slides into Searches with its tint. The final tour step keeps
that map and toast lit. The map reveals
that room, including after expanded phone gameplay. The prefix trail to the
fork remains visible while the pane is closed; inspecting a history shows its
own complete trail. Each retained search has its own heat, cell history IDs,
room discoveries, progress and origin trail. Switching restores that search's
presentation, closes the history pane and resumes exploration. A search already
at its memory or completion limit stays stopped. Inactive heat stays frozen, and
returning does not add a synthetic root visit. The Searches pane shows Main and its actual nested descendants in a
branch tree. Parent identity comes from the worker's active search at admission,
rather than guessing from a route name or manual-history ID. Clicking resumes
that search, including a single tap on phones. Tree indentation stops growing
after three forks; deeper rows name their parent without reducing the map width.
Desktop maps use the available window width. Mouse hover or keyboard focus on an
inactive branch temporarily previews
its saved heat and fork-prefix trail, including its room, without switching the
worker, advancing that branch, or changing the selected replay. Leaving restores
the active map. Phones show a compact scrollable tree below the maps. The lower
world/level atlas is replaced by a modal selector that establishes a real playable
search root; discovery and replay still follow later levels.
Archived states offer Play from here first; search admission is available only
after explicit takeover. While playing, Branch search from here and Discard
branch share one action row and replace the takeover button. Discarding or
closing a gameplay draft releases inputs and resumes the existing search without
admitting the draft. Keyboard hints use labeled keycaps; touch controls suppress
selection/callouts across the whole controller and release captured pointers on
cancellation. Taking control
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
Evicted histories remain in the worker archive and can be fetched again.
Full-resolution panoramas are held only for the focused level's connected rooms; discarded full images can take time to be reclaimed by the browser.
Hovering a route, or focusing it with the keyboard on hover-capable devices,
shows its endpoint screenshot without altering replay, audio or controller state.
An isolated lazy 16 MiB preview emulator reconstructs its original inputs and
compares the final snapshot exactly. It yields after approximately 8 ms and cancels
older requests. A 90 ms dwell avoids replaying fleeting pointer movements. Current
verified endpoints enter the image cache directly. The cache holds at most eight
256×224 RGBA images on phones (1.75 MiB) / sixteen on desktops (3.5 MiB), plus
one shared 1/2 MiB checkpoint cache. One preview emulator is reused. Restart
Search clears images and checkpoints. First hover on an unrelated route can still
require reconstruction; a pending label indicates that work. These allowances are
separate from search, replay and audio memory.

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
with build-only RAM writes and button presses to skip dialogs. The pinned
PlayerInvincible symbol is checked against the ROM debug symbols, and its
normal flicker phase hides the offline camera's player while capturing artwork.
Two frames warm the sprite pipeline before copying pixels. This prevents a
stationary Nova being baked into the background of the swarm. Panorama URLs
carry a capture-recipe cache revision so existing browsers request the corrected
artwork. These visibility writes and snapshots
never enter live search or replay. Only non-reloading main-loop states whose map and loaded checkpoint belong to
the selected level contribute heat or area visits. End screens and level-select
menus still contribute clear witnesses without claiming visits to the next map.
Heat uses actual sampled rollout positions and retained action endpoints,
without interpolated or fabricated visits. The marker and film show a
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

The swarm check compares 128 real recorded and unrecorded rollouts, including
every archived snapshot byte, and samples authentic door/campaign witness tapes.
Browser checks cover moving sprites while search is paused, clean overlay and
sampled-path heat without false restore bridges, heat restoration, one-shot
lifetimes, pause/resume, separate branch recordings,
restart, tour handoff and phones. Interaction checks add real two-finger touch
capture, single-tap branch switching, gameplay-only admission, draft
discard/close resumption and stable layout across four nested branches. Mobile
tour checks perform real route selection, replay/scrubbing, held touch inputs,
rotation, discard, search admission and branch switching at 320px, 390px and
landscape. They check every spotlight against the callout and hit-test controls
to detect occlusion by sticky headers and other panes.

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
stacked live-map selection, dragging, the
inline search controls and memory counters, restart, mobile layout, one attribution below the maps and source bundles. Hover checks compare
a preview screenshot against exact selected replay, preserve the active frame,
verify nested tree ancestry and restore unchanged heat after branch preview. Browser-only worker interception supplies source-matched, replayed
fixture states through the normal selected-history response boundary; no fixture
or test API enters production. Nested forks verify their selected frame, fresh root/counters, first restored
position and descendant prefix against real worker messages. Actual fork messages and actual archive responses
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
Creator, game source and Creative Commons license links appear once below the
main game maps; the footer’s Credits & source dialog contains full notices. Full attribution, restrictions, pinned source references
and corresponding-source links are in
[CREDITS.md](CREDITS.md) and the site's Credits dialog. The build distributes
original sources and build recipes alongside the ROM and emulator.

The Searches and History panes use matching sliding panels. Each search has inline play/pause controls. Main has a restart control that releases the whole search tree; other branches have delete controls that free their native snapshot archive, heat view and movement recordings. Deleting an active branch resumes its parent. Independent child branches keep their own complete input histories and move up the tree. Branch IDs are never reused within a search session. Movement, Heatmap and Both are visible segmented controls, and hovering a search branch preserves that choice. Search details report snapshot archive memory shared across branches. Light and dark UI palettes follow ph14.dev; game pixels and heat colors are unchanged. The theme follows the OS until a visitor chooses one.

Phones show one room at a time through a 2:1 camera viewport instead of stacked, wrapped sections. Room tabs above the map switch rooms and mark rooms the search has reached. The camera holds one room height on screen (about 0.8 CSS pixels per game pixel) and follows the search frontier; when the current room empties for 2.5 seconds and another room in the level has at least six live sprites, it moves there unless the visitor chose a room. Whole room fits the entire room in the viewport and stops following; Follow the search resumes. Any drag, pinch or cell selection stops following. Switching search branches and finishing the tour resume it. Phone maps keep vertical page scrolling available even when zoomed: one finger pans horizontally, two fingers pinch and pan the map, and each room retains its own view. Tour spotlights are clipped to scroll containers and the visual viewport so scrolling the history sheet cannot illuminate hidden controls or the map underneath. All highlighted controls remain usable.

## Starting at another level

Click the World / Level title to choose from all 40 campaign and four bonus levels. Choosing one replaces the current search tree and starts a fresh Main search. This is a level-start experiment, not a campaign save: the game initializes its own health, ability and level-specific items. No cleared-level or collectible flags are fabricated. The UI, worker and hover emulator all use the same `boot_level` identity.

`boot(level)` restores a power-on snapshot, sets only native level availability in SRAM before the title-screen menu selects its highest available world/level, then runs the pinned controller bootstrap. The default level-zero bootstrap stays identical. Later levels use `Explorer.from_history(seed, "[]")` at this authenticated root. Replay and forks include the starting level plus all subsequent inputs; a worker rejects tapes from a different root. Repeated warps reset archive, snapshots, map views, progress and cached replay/hover checkpoints.

`tests/levels.mjs` verifies all 44 native level starts, advancing a fresh archive, exact replay, repeated bootstrap and rooted forks. `tests/levels-browser.mjs` exercises the selector on desktop and phone through four repeated warps and gameplay forks, then checks real pinch zoom and vertical scrolling of zoomed maps.

Level cards use 256×96 thumbnails built from the original panorama pixels, with the same embedded Creative Commons attribution. The 44 thumbnails together contain about 4.1 MiB of decoded RGBA, rather than decoding full panoramas for gallery cards. Live map panoramas remain unchanged and are retained only for the current level's rooms.

The panes are labeled Searches, Exploration and History. History keeps its heading; route names appear only in the retained-history list. The artwork attribution below the main game maps uses larger text; the footer links to Credits & source.

Overview route strokes stay bright gold at a constant screen width, with a dark edge for contrast at small map scales. Retained histories collapse into a reopen control; dismissing human play still discards its draft and resumes the existing search. On phones, Searches places its heading and short scrollable tree in one row. During human play, History places audio in the header, then the screen, a spaced controller and Branch/Discard actions; the retained-route list stays hidden.

On desktop, long rooms wrap into contiguous sections under one room heading. The section width adapts to the Exploration pane, aligning seams to original tile boundaries; short rooms keep their overview. Small numbered continuation links connect the sections of the same room. Tall rooms use an ordered grid of vertical sections. Artwork, heat, movement and selected trails use the same original world coordinates in every section; no visits or connecting routes are invented at a wrap. Replay follows its actual section, and selection, keyboard navigation, dragging and pinch zoom use that section's transform. A room's zoom button applies to all its sections, while gestures can adjust a single section. `tests/wrapped-maps.mjs` exercises later horizontal and vertical rooms, original-art crops, tile hits beyond the first section, section navigation, zoom and responsive reflow on desktop and phone. The nested-branch interaction check verifies native archive memory release, preserved child branches, active-parent resumption and non-reused branch IDs.
