// SPDX-License-Identifier: AGPL-3.0-or-later
import "./style.css";
import { ViewRecording, composeView } from "./recording.js";
import { GuidedTour, tourSeen } from "./tour.js";
import { ReplayTimeline, levelEntryFrame, prefixTrail, trailPoint } from "./replay.js";
import { RoutePreviews } from "./preview.js";
import { GameAudio } from "./audio.js";
import { createEngine, GAMES, sha256Hex } from "./emulator.js";
import { load, save, loadRom, saveRom } from "./rom-store.js";
import { Heatmap, cellTop, routeIds } from "./heat.js";
import { Swarm, ghosts, trailRoom, MOTION_STRIDE } from "./swarm.js";
import { resumeFollowing, resetRetreat, trackingGoal, followView } from "./camera.js";
import { SmoothCounter } from "./counter.js";
import { snapshotHash, snapshotDigest } from "./media.js";
import { roomPanels, panelContains, panelCenter, CAMERA_VIEWPORT, GRID_CAMERA_VIEWPORT, TALL_CAMERA_VIEWPORT } from "./view.js";
import {
  prefixAt,
  appendInput,
  Controller,
  keyButtons,
  trailSegments,
  branchInfo,
} from "./branch.js";
import { StateCache, memoryBudget } from "./memory.js";
import { liveSearch } from "./live-source.js";
import { detectLauncher, nativePanel, segments as segmentButtons } from "./native-run.js";
import {
  project,
  completedLevels,
  mergeProgress,
  isMapEvidence,
  finishedLevel,
  followExit,
  busierDeeperLevel,
} from "./world.js";
const params = new URLSearchParams(location.search);
const liveView = params.has("live");
const bundledRoms = new URL("roms/", new URL(import.meta.env.BASE_URL, location.href));
const manifest = await fetch(new URL("manifest.json", bundledRoms))
  .then((response) => (response.ok ? response.json() : {}))
  .catch(() => ({}));
const bundled = new Set(manifest.games || []);
const requested = GAMES.get(params.get("game")) || GAMES.get(manifest.default) || GAMES.get("twin-dragons");
async function bundledRom(choice) {
  if (!bundled.has(choice.id)) return null;
  const response = await fetch(new URL(`${choice.id}.nes`, bundledRoms)).catch(() => null);
  const bytes = response?.ok ? new Uint8Array(await response.arrayBuffer()) : null;
  return bytes?.length === choice.rom.bytes && (await sha256Hex(bytes)) === choice.rom.sha256 ? bytes : null;
}
const suppliedRom = requested.rom.upload ? (await loadRom(requested.id)) || (await bundledRom(requested)) : null;
const missingRom = requested.rom.upload && !suppliedRom ? requested : null;
const game = missingRom ? GAMES.get("twin-dragons") : requested;
const engineInputs = suppliedRom && !missingRom ? { rom: suppliedRom } : {};
let askedForRom = false;
const SPRITE = game.sprite;
const base = new URL(import.meta.env.BASE_URL, location.href),
  assets = new URL(game.assets, base),
  $ = (id) => document.getElementById(id);
const compactReplay = matchMedia("(max-width: 800px), (pointer: coarse) and (max-width: 1000px) and (max-height: 500px)");
document.title = "Harmony Arcade";
document.documentElement.dataset.game = game.id;
document.documentElement.classList.toggle("vertical-game", !!game.vertical);
document.documentElement.classList.toggle("grid-game", !!game.grid);
document.documentElement.style.setProperty("--screen-ratio", game.screen.width / game.screen.height);
const catalog = game.catalog || (await (await fetch(new URL("maps.json", assets))).json());
const maps = new Map(catalog.maps.map((map) => [map.id, map]));
document.querySelector("#app").innerHTML = `
<main><section class="hero" aria-labelledby="hero-title"><div class="hero-copy"><h1 id="hero-title">Harmony Arcade</h1><p class="hero-lede">Watch and join Harmony in playing games through multiverse state exploration.</p></div><div class="hero-side"><div class="hero-stats" aria-label="Search progress"><div><b id="hero-timelines">0</b><span>timelines tried</span></div></div><button id="tour-open" disabled>Take the tour</button></div></section><div class="workspace" id="workspace"><aside id="branches" class="branches" aria-label="Search branches"><div class="pane-heading"><h2>Searches</h2><button id="timeline-toggle" aria-expanded="true" aria-label="Collapse Searches">‹</button></div><nav aria-label="Search branches"><ol id="branch-tree"></ol></nav><details class="search-details" open><summary>Details</summary><label class="search-speed"><span>Timelines per second <output id="search-speed-value"></output></span><input id="search-speed" type="range" min="0" step="1"><span class="search-speed-ends" aria-hidden="true"><span>Slower</span><span>Faster</span></span></label><div class="metrics"><div><b id="attempts">0</b><span>timelines tried</span></div><div><b id="states">0</b><span>moments saved</span></div><div><b id="cells">0</b><span>map cells reached</span></div><div><b id="work">0</b><span>game frames played</span></div><div><b id="memory">0 MB</b><span>snapshot memory</span></div><div><b id="workers">0</b><span>search workers</span></div></div></details></aside><section class="exploration" aria-label="Exploration"><h2 class="exploration-heading visually-hidden">Exploration</h2><div class="controls" hidden><span id="status" hidden>Loading ${game.title}…</span><i id="status-dot" hidden></i><button id="pause" class="icon-button" aria-label="Pause Search" title="Pause Search" disabled></button><button id="reset" class="icon-button" aria-label="Restart Search" title="Restart Search" disabled><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 10a8 8 0 1 1 1 8M4 4v6h6" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/></svg></button></div><span id="branch-feedback" class="visually-hidden" role="status"></span>
<div class="goal"><div class="level-nav"><button id="new-search" class="new-search" aria-haspopup="dialog" aria-controls="level-picker" title="Start a fresh search from any level" aria-label="New search" disabled><span>New search</span></button><nav id="level-strip" class="level-strip" aria-label="Levels in this search" data-key=""><button id="level-more" class="level-more" aria-controls="level-trail" aria-expanded="false" hidden></button><div id="level-trail" class="level-trail"></div></nav><span id="goal-status" hidden></span></div><div class="view-tools"><div id="visualization" class="segments toggles" role="group" aria-label="Show on map" data-value="movement"><button data-viz="movement" aria-pressed="true">Movement</button><button data-viz="heat" aria-pressed="false">Heatmap</button></div><button id="completion" hidden>Watch completion</button></div></div>
<div class="recording-controls" role="group" aria-label="Record exploration"><button id="record-to-file" hidden>Record to file</button><button id="record-view" aria-pressed="false">Record clip</button><a id="download-recording" hidden>Download video</a></div>
<div class="map-wrap" data-rooms="1"><div class="room-bar"><nav id="room-tabs" aria-label="Maps in this level"></nav></div><div id="map-rows"></div><canvas id="map" width="1280" height="320" tabindex="0" aria-label="Game area heatmap. Drag to move when zoomed. Arrow keys pan; Enter inspects the cell under the pointer or at the center."></canvas><span class="map-label" id="map-label" hidden>LEVEL 1</span><div id="hover" hidden></div></div>
<div class="map-footer"><span id="memory-limit" hidden></span><span id="recording-status" role="status" aria-live="polite"></span></div>
${game.attribution ? `<p class="game-attribution">${game.attribution}</p>` : ""}
</section>
<section class="inspect inspector" id="inspector" aria-label="History inspector" hidden><div class="drawer-top"><div class="history-heading"><h2 id="film-title">History</h2></div><div class="history-controls"><button id="sound" class="icon-button" aria-label="Mute game audio" title="Mute game audio" aria-pressed="false"></button><button id="expand-inspector" aria-expanded="false">Expand</button><button id="close-inspector" aria-controls="inspector" aria-label="Collapse History" aria-expanded="true" title="Collapse History">›</button></div></div><div class="film"><span id="verification" hidden>Starting emulator</span><div class="screen"><canvas id="film" tabindex="0" width="${game.screen.width}" height="${game.screen.height}" aria-label="${game.title} gameplay replay"></canvas><span id="frame-label">FRAME 0</span></div><div class="transport"><div class="replay-actions"><button id="play" disabled>▶ Replay</button></div><input id="scrub" aria-label="Replay frame" type="range" min="0" max="0" value="0" disabled><select id="speed" aria-label="Playback speed"><option value="1" selected>1×</option><option value="4">4×</option><option value="12">12×</option></select></div><div id="game-controls" hidden><div class="keyboard-guide" aria-label="Keyboard controls"><span><kbd>↑ ← ↓ →</kbd><kbd>WASD</kbd><span>Move</span></span><span><kbd>Z</kbd><kbd>Space</kbd><span>${game.controls.a}</span></span><span><kbd>X</kbd><span>${game.controls.b}</span></span></div><div class="touch-controls" aria-label="Game controller"><div class="dpad"><button data-button="16" aria-label="Move up">↑</button><button data-button="64" aria-label="Move left">←</button><button data-button="32" aria-label="Move down">↓</button><button data-button="128" aria-label="Move right">→</button></div><div class="action-buttons"><button data-button="2" aria-label="${game.controls.b}">B</button><button data-button="1" aria-label="${game.controls.a}">A</button></div></div></div><div class="branch-actions"><button id="take-control" disabled>🎮 Play from here</button><button id="search-here" hidden disabled>↗ Branch search from here</button><button id="discard-branch" hidden><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 6h16M9 6V3h6v3M6 6l1 15h10l1-15M10 10v7m4-7v7"/></svg>Discard branch</button></div><div id="branch-message" role="status" hidden></div></div>
<aside class="state-picker" aria-label="Retained histories"><div class="section-title"><h2 id="cell-title">Saved paths</h2></div><p id="selection-hint" hidden></p><div id="state-list"></div><details class="state-disclosure"><summary>Game state</summary><div id="details" class="details"></div></details></aside></section><aside id="history-rail" class="history-rail" hidden><button id="history-reopen" aria-controls="inspector" aria-label="Expand History" aria-expanded="false" title="Expand History">‹<span>History</span></button></aside></div>
<footer><a class="brand" href="https://github.com/pH14/harmony"><b>harmony</b></a><button id="credits">Credits & source</button><button id="theme" class="theme-button" aria-label="Switch color theme">Dark mode</button></footer>
<div id="error" role="alert" hidden></div>
<dialog id="level-picker" aria-labelledby="level-picker-title"><div class="level-picker-heading"><button id="close-level-picker" class="close" aria-label="Close level selector">×</button><h2 id="level-picker-title">Pick a game</h2></div><div id="game-choice" class="game-choice"><section id="run-target" class="game-group" hidden></section><section class="game-group" aria-labelledby="homebrew-title"><h3 id="homebrew-title">Homebrew</h3><div id="homebrew-games" class="segments" role="group" aria-labelledby="homebrew-title"></div></section><section class="game-group" aria-labelledby="byo-rom-title"><h3 id="byo-rom-title">Bring Your Own ROM</h3><div id="rom-games" class="segments" role="group" aria-labelledby="byo-rom-title"></div></section></div><section id="rom-panel" class="rom-panel" hidden><h3 id="rom-title"></h3><button id="rom-drop" class="rom-drop" type="button">Drop the ROM file here, or click to choose it</button><p id="rom-message" class="rom-message" role="status" hidden></p><div id="playthrough" class="playthrough" hidden><p>If you don’t have a ROM handy, you can watch an end-to-end playthrough</p><p id="playthrough-note" class="playthrough-note" hidden></p><div class="playthrough-frame"><div id="playthrough-video"></div></div></div></section><section id="native-panel" class="native-panel" hidden></section><div id="level-grid" class="level-grid"></div></dialog>
<dialog id="info"><button id="close-info" class="close" aria-label="Close">×</button><div id="info-content"></div></dialog></main>`;
const recordingCanvas = document.createElement("canvas");
recordingCanvas.width = 1920;
recordingCanvas.height = 1080;
let recordingURL = null, choosingRecordingFile = false;
const recording = new ViewRecording({
  onChange(state) {
    $("record-view").textContent = state === "recording" ? "Stop recording" : state === "stopping" ? "Saving video…" : "Record clip";
    $("record-view").disabled = choosingRecordingFile || state === "stopping" || !recording.supported(recordingCanvas);
    $("record-to-file").disabled = choosingRecordingFile || state !== "idle" || !recording.supported(recordingCanvas);
    $("record-view").setAttribute("aria-pressed", state === "recording");
    if (state === "recording" && recordingURL) $("download-recording").textContent = "Download previous clip";
    if (state === "recording" && recording.writable) $("recording-status").textContent = recording.backpressured
      ? "Disk is busy. Capture is paused while video writes drain; the search continues."
      : "Recording directly to file with no duration limit. Keep this tab visible. Stop recording to finish and save the video.";
  },
  onComplete(blob, reason, result) {
    if (result?.file) {
      $("recording-status").textContent = `Video saved to file (${(result.bytes / 1048576).toFixed(1)} MB).`;
      return;
    }
    if (recordingURL) URL.revokeObjectURL(recordingURL);
    recordingURL = URL.createObjectURL(blob);
    const link = $("download-recording");
    link.href = recordingURL;
    link.textContent = "Download video";
    link.download = `harmony-${game.id}-${new Date().toISOString().replace(/[:.]/g, "-")}.${blob.type.includes("mp4") ? "mp4" : "webm"}`;
    link.hidden = false;
    $("recording-status").textContent = `${reason ? reason + ". " : ""}Video ready (${(blob.size / 1048576).toFixed(1)} MB). Download it before leaving this page.`;
  },
  onError(error) { $("recording-status").textContent = `Recording failed: ${error.message}`; },
});
$("record-to-file").hidden = typeof window.showSaveFilePicker !== "function";
$("record-to-file").disabled = $("record-view").disabled = !recording.supported(recordingCanvas);
if ($("record-view").disabled) $("recording-status").textContent = "Video recording is unavailable in this browser. Try Chrome or Firefox.";
$("record-view").onclick = () => {
  if (recording.state === "recording") return recording.stop();
  try {
    paintRecording();
    recording.start(recordingCanvas);
    $("recording-status").textContent = "Recording view and mini map. Keep this tab visible. Stops after 10 minutes or 256 MB.";
  } catch (error) { $("recording-status").textContent = `Recording failed: ${error.message}`; }
};
$("record-to-file").onclick = async () => {
  choosingRecordingFile = true;
  recording.onChange(recording.state);
  let writable;
  try {
    const type = recording.mimeType.split(";")[0], extension = type.includes("mp4") ? ".mp4" : ".webm";
    const handle = await window.showSaveFilePicker({
      suggestedName: `harmony-${game.id}-${new Date().toISOString().replace(/[:.]/g, "-")}${extension}`,
      types: [{ description: "Search video", accept: { [type]: [extension] } }],
    });
    writable = await handle.createWritable();
    paintRecording();
    recording.start(recordingCanvas, { writable });
  } catch (error) {
    if (writable && recording.writable !== writable) {
      try { await writable.abort(); } catch { /* The recording may already have aborted the writer. */ }
    }
    $("recording-status").textContent = error.name === "AbortError" ? "File recording canceled." : `Recording failed: ${error.message}`;
  } finally {
    choosingRecordingFile = false;
    recording.onChange(recording.state);
  }
};
function paintRecording() {
  const map = document.querySelector(".area-map");
  if (!map) throw new Error("Wait for the game map to load.");
  composeView(recordingCanvas, {
    map, overview: document.querySelector(".level-overview"),
    title: `Harmony Plays ${game.title} | ${$("workers").textContent} ${Number($("workers").textContent) === 1 ? "Worker" : "Workers"}`,
    detail: maps.get(Number(map.dataset.map))?.label || "",
    progress: `${$("hero-timelines").textContent} timelines tried · ${$("work").textContent} game frames`,
    attribution: `${document.querySelector(".game-attribution")?.textContent || game.title} / github.com/pH14/harmony`,
  });
  recording.frame();
}
window.addEventListener("beforeunload", (event) => {
  if (recording.state !== "idle") { event.preventDefault(); event.returnValue = ""; }
});
window.addEventListener("pagehide", () => {
  const activeRecording = recording.state !== "idle";
  recording.cleanup();
  if (activeRecording) $("recording-status").textContent = "Recording canceled when leaving this page. Start a new recording to capture more.";
  if (recordingURL) {
    URL.revokeObjectURL(recordingURL);
    recordingURL = null;
    $("download-recording").hidden = true;
    $("download-recording").removeAttribute("href");
    $("recording-status").textContent = "Video cleared when leaving this page. Start a new recording to capture more.";
  }
});
let heat = new Heatmap(),
  worker,
  rolloutWorkers = [],
  engine,
  origin,
  originPixels,
  bootLevel = 0,
  gameWon = false,
  paused = false,
  terminalDrain = null,
  replayPaused = false,
  replayWatching = false,
  viewerReplay = false,
  ready = false,
  seed = 27,
  request = 0,
  userSelected = false,
  current = null,
  currentFrame = 0,
  playing = false,
  seeking = false,
  replayEpoch = 0,
  lastTime = 0,
  frameCredit = 0,
  latestBest = null,
  lastAuto = 0,
  hoverCell = null,
  selectedCell = null,
  mapLevel = 0,
  mapWidth = 1280,
  mapHeight = 224,
  focusedLevel = 0,
  watchRequested = false,
  pointerStart = null,
  dragged = false;
let stateCache = new StateCache(),
  bestByMap = new Map(),
  bestStates = new Map(),
  arrivals = new Map(),
  seenMaps = new Set(),
  cleared = new Set(),
  wins = new Map(),
  path = { level: 0, levels: [0], since: 0, idle: 0 },
  stats = {};
const counters = { executions: new SmoothCounter(), states: new SmoothCounter(), frames: new SmoothCounter() };
const views = new Map(),
  controller = new Controller();
let playSession = null,
  controlMode = false,
  branchBusy = false,
  manualId = 0,
  activeSearch = 0,
  branchOrigin = null,
  pendingOrigin = null,
  originPulse = null,
  trace = [],
  segments = [],
  traceMaxFrame = -1,
  traceStride = 24,
  frameObservation,
  inspectorReturnFocus,
  revealReplayRoom = false,
  historyVerified = false,
  requestedFrame = 0,
  renderedFrame = 0,
  seekStarted = 0,
  lastMapPaint = 0,
  statsDirty = false,
  lastDetailPaint = 0;
const searchViews = new Map(),
  reducedMotion = matchMedia("(prefers-reduced-motion: reduce)");
function invalidateStateRequest() {
  heat.releasePending();
  return ++request;
}
function clearRouteSelection() {
  heat.clearPins();
  invalidateStateRequest();
}
function requestStates(ids, cell = selectedCell, ordinal) {
  const token = ++request;
  heat.pending(cell, ids[0], token, ordinal);
  worker.postMessage({ type: "states", ids, request: token });
}
function saveSearchView() {
  terminalDrain = null;
  clearRouteSelection();
  heat.setRunning(false, performance.now());
  searchViews.set(activeSearch, {
    heat,
    movement: swarm.frame(activeSearch, heat.clock(performance.now()), reducedMotion.matches, swarm.maxTrails / 8),
    bestByMap,
    bestStates,
    arrivals,
    seenMaps,
    cleared,
    wins,
    path,
    gameWon,
    latestBest,
    stats,
    branchOrigin,
    room: mapLevel,
  });
}
function activateSearchView(id) {
  const fresh = !searchViews.has(id);
  const view = searchViews.get(id) || {
    heat: new Heatmap(),
    bestByMap: new Map(),
    bestStates: new Map(),
    arrivals: new Map(),
    seenMaps: new Set(),
    cleared: new Set(),
    wins: new Map(),
    path: { level: null, levels: [], since: 0, idle: 0 },
    gameWon: false,
    latestBest: null,
    stats: { executions: 0, states: 1, frames: 0 },
    branchOrigin: null,
  };
  ({
    heat,
    bestByMap,
    bestStates,
    arrivals,
    seenMaps,
    cleared,
    wins,
    path,
    gameWon,
    latestBest,
    stats,
    branchOrigin,
  } = view);
  searchViews.set(id, view);
  swarmRooms = swarm.frame(id, heat.clock(performance.now()), reducedMotion.matches);
  hoverCell = selectedCell = null;
  clearRouteSelection();
  lastAuto = 0;
  return fresh;
}
function revealBranchOrigin() {
  const point = branchOrigin?.point;
  if (!point) return;
  setRoom(point.level, true);
  const c = mapCanvas(point.level, point);
  if (!c) return;
  const view = roomView(point.level, c);
  Object.assign(view, panelCenter(canvasPanel(c), view.zoom, { x: point.x, y: point.y - 8 }, c));
  const row = c.closest(".room-part");
  const bounds = row.getBoundingClientRect();
  if (bounds.top < 20 || bounds.bottom > innerHeight - 20)
    window.scrollBy({ top: bounds.top - 24, behavior: "instant" });
}
function canvasPanel(c) {
  return { x: Number(c.dataset.panelX || 0), y: Number(c.dataset.panelY || 0),
    width: Number(c.dataset.mapWidth), height: Number(c.dataset.mapHeight) };
}
function mapCanvas(id, point) {
  const panels = [...document.querySelectorAll(`.map-row[data-map="${id}"] .area-map`)];
  return (point && panels.find(c => panelContains(canvasPanel(c), { x: point.x, y: point.y - 8 }))) || panels[0];
}
function roomView(id, c) {
  const key = c ? `${id}:${c.dataset.panel}` : id;
  if (!views.has(key)) {
    const map = c ? canvasPanel(c) : { x: 0, y: 0, ...maps.get(id) };
    const fresh = c && !views.has(id);
    const view = { zoom: c ? roomView(id).zoom : 1, x: (map.x || 0) + map.width / 2, y: (map.y || 0) + map.height / 2 };
    views.set(key, view);
    if (fresh) {
      view.zoom = roomView(id).zoom = closeUpZoom(c);
      const point = frontier(id) || reachedFrontier(id);
      Object.assign(view, panelCenter(map, view.zoom, { x: point.x, y: point.y - 8 }, c));
    }
  }
  return views.get(key);
}
const budget = memoryBudget(
  navigator.deviceMemory,
  matchMedia("(pointer: coarse)").matches,
  navigator.hardwareConcurrency,
  Number(params.get("workers")),
);
let pace = params.has("pace") ? Math.max(0, Number(params.get("pace")) || 0) : game.pace;
const paceStops = [...new Set([10, 30, game.pace, 300, 1000, Infinity, pace || Infinity])].sort((a, b) => a - b);
function showPace() {
  $("search-speed").value = paceStops.indexOf(pace || Infinity);
  $("search-speed-value").textContent = pace ? fmt(pace) : "Unlimited";
  $("search-speed").setAttribute("aria-valuetext", pace ? `${fmt(pace)} timelines per second` : "Unlimited");
  $("search-speed").title = pace ? `At most ${fmt(pace)} timelines per second` : "As fast as this device can run";
}
const FINISH_HOLD = 500;
const swarm = new Swarm(
  (budget.snapshotsMiB === 32 ? 2 : 8) * 1048576,
  budget.snapshotsMiB === 32 ? 2048 : 8192,
  FINISH_HOLD,
);
let visualization = "movement", liveVisualization = null, swarmRooms = new Map();
const pointLevels = new Map();
let trailsSeen = false, traillessBatches = 0;
const sprites = new Image();
if (!game.buildSprites) sprites.src = new URL("sprites.png", assets).href;
function setVisualization(mode, draw = true) {
  visualization = mode;
  $("visualization").dataset.value = mode;
  for (const button of $("visualization").children) button.setAttribute("aria-pressed", mode === "both" || button.dataset.viz === mode);
  $("hover").hidden = true;
  if (draw) drawMap(performance.now());
}
for (const button of $("visualization").children) button.onclick = () => {
  userSelected = true;
  liveVisualization = null;
  const on = new Set(visualization === "both" ? ["movement", "heat"] : [visualization]);
  if (on.has(button.dataset.viz)) on.delete(button.dataset.viz);
  else on.add(button.dataset.viz);
  if (on.size) setVisualization(on.size === 2 ? "both" : [...on][0]);
};
let theme;
try { theme = localStorage.getItem("harmony.twin-dragons.theme"); } catch {}
function applyTheme(value) {
  document.documentElement.dataset.theme = value;
  $("theme").setAttribute("aria-label", `Switch to ${value === "dark" ? "light" : "dark"} mode`);
  $("theme").textContent = value === "dark" ? "Light mode" : "Dark mode";
}
const themeMedia = matchMedia("(prefers-color-scheme: dark)");
applyTheme(theme === "light" || theme === "dark" ? theme : themeMedia.matches ? "dark" : "light");
themeMedia.addEventListener("change", () => { if (!theme) applyTheme(themeMedia.matches ? "dark" : "light"); });
$("theme").onclick = () => {
  theme = document.documentElement.dataset.theme === "dark" ? "light" : "dark";
  applyTheme(theme);
  try { localStorage.setItem("harmony.twin-dragons.theme", theme); } catch {}
};
$("timeline-toggle").onclick = () => {
  const collapsed = $("workspace").classList.toggle("timeline-collapsed");
  $("timeline-toggle").setAttribute("aria-expanded", !collapsed);
  $("timeline-toggle").setAttribute("aria-label", `${collapsed ? "Expand" : "Collapse"} Searches`);
  $("timeline-toggle").textContent = collapsed ? "›" : "‹";
};
function drawSwarmBackground(ctx, width, height) {
  ctx.fillStyle = "rgba(12,17,20,.24)";
  ctx.fillRect(0, 0, width, height);
}
const branchIcons = new Map();
function branchIcon() {
  if (!sprites.complete || !sprites.naturalWidth) return null;
  if (!branchIcons.has("player")) {
    const icon = document.createElement("canvas");
    const [x, y, width, height] = SPRITE.icon;
    icon.width = width;
    icon.height = height;
    icon.getContext("2d").drawImage(sprites, x, y, width, height, 0, 0, width, height);
    branchIcons.set("player", icon.toDataURL());
  }
  return branchIcons.get("player");
}
sprites.addEventListener("load", () => renderBranches());
const ghostDot = document.createElement("canvas");
ghostDot.width = ghostDot.height = 32;
{
  const dot = ghostDot.getContext("2d");
  dot.fillStyle = "#eaf6ff";
  dot.beginPath();
  dot.arc(16, 16, 16, 0, Math.PI * 2);
  dot.fill();
}
function drawPlayers(ctx, level, screenScale, rooms = swarmRooms, panel = null, visible = null) {
  if (!sprites.complete || !sprites.naturalWidth) return 0;
  const map = maps.get(level), points = rooms?.get(level) || [];
  const size = Math.max(1, 18 / (SPRITE.height * screenScale));
  const radius = 1.7 * size, margin = 32 * size;
  const shown = [];
  let count = 0;
  for (const raw of points) {
    const p = project(raw, map);
    if (p.x < 0 || p.x >= map.width || p.y < 0 || p.y >= map.height) continue;
    if (panel && (p.x < panel.x - 48 || p.x >= panel.x + panel.width + 48 || p.y < panel.y - 48 || p.y >= panel.y + panel.height + 48)) continue;
    if (!panel || panelContains(panel, { x: p.x, y: p.y - 8 })) count++;
    if (visible && (p.x < visible.left - margin || p.x > visible.right + margin || p.y < visible.top - margin || p.y > visible.bottom + margin)) continue;
    shown.push(p, raw);
  }
  ctx.save();
  ctx.imageSmoothingEnabled = true;
  for (let i = 1; i < shown.length; i += 2)
    ghosts(shown[i]).forEach((ghost, index) => {
      const g = project(ghost, map);
      ctx.globalAlpha = index ? 0.12 : 0.22;
      ctx.drawImage(ghostDot, g.x - radius, g.y - 8 - radius, radius * 2, radius * 2);
    });
  ctx.imageSmoothingEnabled = false;
  for (let i = 0; i < shown.length; i += 2) {
    const p = shown[i], raw = shown[i + 1];
    ctx.globalAlpha = raw.life === undefined ? 0.8 : Math.min(1, raw.life / 0.05 + 0.2) * (0.97 - 0.32 * raw.life);
    ctx.drawImage(sprites, (p.pose >> 1) * SPRITE.width, (p.pose & 1) * SPRITE.height, SPRITE.width, SPRITE.height,
      p.x - (SPRITE.originX + 8) * size, p.y - 8 - SPRITE.originY - 8 + SPRITE.height * (1 - size), SPRITE.width * size, SPRITE.height * size);
  }
  ctx.restore();
  return count;
}
const phoneLayout = matchMedia("(max-width: 800px)");
const INTRO_SCALE = 1.3,
  PATH_STALL_MS = 8000,
  ROOM_DWELL_MS = 10000,
  CAMERA_RETREAT_MS = 1500;
const camera = { mode: "follow", follow: false, overview: false, manual: 0, pinned: null, introStart: 0, introDone: false, focus: null, last: 0, busierSince: 0, roomSince: 0, leader: null, jumped: 0, targets: new Map() };
const ease = (t) => (t < 0.5 ? 4 * t * t * t : 1 - (-2 * t + 2) ** 3 / 2);
function closeUpZoom(c) {
  const panel = canvasPanel(c), fit = Math.min(c.width / panel.width, c.height / panel.height);
  const view = game.closeUp || game.screen;
  return Math.max(1, Math.min(c.width / view.width, c.height / view.height) / fit);
}
function frameRoute() {
  for (const room of cameraRooms()) {
    const c = mapCanvas(room), points = trace.filter((p) => !p.gap && p.level === room);
    if (!c || !points.length) {
      overview(room);
      continue;
    }
    const xs = points.map((p) => p.x), ys = points.map((p) => p.y);
    const left = Math.min(...xs) - 48, right = Math.max(...xs) + 48, top = Math.min(...ys) - 56, bottom = Math.max(...ys) + 40;
    const panel = canvasPanel(c), fit = Math.min(c.width / panel.width, c.height / panel.height);
    const zoom = Math.min(closeUpZoom(c), Math.max(1, Math.min(c.width / (fit * (right - left)), c.height / (fit * (bottom - top)))));
    const view = roomView(room, c);
    roomView(room).zoom = view.zoom = zoom;
    Object.assign(view, panelCenter(panel, zoom, { x: (left + right) / 2, y: (top + bottom) / 2 }, c));
  }
}
function maxZoom(c) {
  return c?.dataset.viewport === "true" ? Math.max(6, closeUpZoom(c) * 2.5) : 6;
}
function frontier(level) {
  const map = maps.get(level), raw = swarmRooms.get(level) || [];
  const left = (p) => (p.samples ? p.samples.at(-MOTION_STRIDE) - p.frame : 0);
  const order = (p) => game.route(level, p.x + 8, p.y + 8);
  const finisher = raw.filter((p) => p.finish && p.samples && levelRooms(level).includes(trailRoom(p.samples))).sort((a, b) => left(a) - left(b) || (game.grid ? order(b) - order(a) : game.vertical ? a.y - b.y : b.x - a.x))[0];
  if (finisher) return { x: finisher.x + 8, y: finisher.y };
  if (game.grid) {
    const ranked = raw.map((p) => [order(p), p]).filter(([rank]) => rank >= 0).sort((a, b) => a[0] - b[0]);
    if (ranked.length < 4) return null;
    const p = ranked[Math.floor(ranked.length * 0.8)][1];
    return { x: p.x + 8, y: p.y };
  }
  const px = new Float64Array(raw.length), py = new Float64Array(raw.length);
  let n = 0;
  for (const p of raw) {
    const x = p.x + 8, y = p.y + 8;
    if (x < 0 || x >= map.width || y < 0 || y >= map.height) continue;
    px[n] = x;
    py[n++] = y;
  }
  if (n < 4) return null;
  if (game.vertical) {
    const y = py.slice(0, n).sort()[Math.floor(n * 0.2)];
    const xs = [];
    for (let i = 0; i < n; i++) if (Math.abs(py[i] - y) < 160) xs.push(px[i]);
    xs.sort((a, b) => a - b);
    return { x: xs[xs.length >> 1] ?? map.width / 2, y: y - 8 };
  }
  const x = px.slice(0, n).sort()[Math.floor(n * 0.8)];
  const ys = [];
  for (let i = 0; i < n; i++) if (Math.abs(px[i] - x) < 160) ys.push(py[i]);
  ys.sort((a, b) => a - b);
  return { x, y: (ys[ys.length >> 1] ?? map.height / 2) - 8 };
}
function reachedFrontier(level) {
  const map = maps.get(level);
  const center = (cell) => ({ x: cell.x * 32 + 16, y: cellTop(cell.y) + 16 });
  const rank = (cell) => game.route(level, center(cell).x, center(cell).y);
  let best = null;
  for (const cell of heat.cells.values())
    if (cell.level === level && (!best || (game.grid ? rank(cell) > rank(best) : game.vertical ? cell.y < best.y : cell.x > best.x))) best = cell;
  return best ? center(best) : game.grid ? game.entry(level) : { x: 0, y: map.height };
}
function cameraProgress(level, point) {
  return game.grid ? game.route(level, point.x, point.y + 8) : game.vertical ? -point.y : point.x;
}
function steerCamera(level, c, target, zoom, zoomRate, panRate) {
  const view = roomView(level, c);
  view.zoom += (zoom - view.zoom) * zoomRate;
  if (Math.abs(view.zoom - zoom) < 0.005) view.zoom = zoom;
  roomView(level).zoom = view.zoom;
  const center = panelCenter(canvasPanel(c), view.zoom, target, c);
  view.x += (center.x - view.x) * panRate;
  view.y += (center.y - view.y) * panRate;
  Object.assign(view, panelCenter(canvasPanel(c), view.zoom, view, c));
}
function overview(level) {
  camera.overview = true;
  roomView(level).zoom = 1;
  for (const c of document.querySelectorAll(`.map-row[data-map="${level}"] .area-map`)) {
    const view = roomView(level, c), zoom = game.vertical || game.grid ? closeUpZoom(c) : 1;
    view.zoom = roomView(level).zoom = zoom;
    Object.assign(view, panelCenter(canvasPanel(c), zoom, view, c));
  }
}
function scrollRoom(id, point) {
  for (const c of document.querySelectorAll(`.map-row[data-map="${id}"] .area-map`)) {
    const view = roomView(id, c);
    Object.assign(view, panelCenter(canvasPanel(c), view.zoom, { x: point.x ?? view.x, y: point.y ?? view.y }, c));
  }
}
function scrollAlong(id, c, delta) {
  return game.vertical ? scrollView(id, c, 0, delta) : scrollView(id, c, delta, 0);
}
function scrollView(id, c, dx, dy) {
  const view = roomView(id, c), panel = canvasPanel(c);
  const scale = Math.min(c.width / panel.width, c.height / panel.height) * view.zoom;
  const next = panelCenter(panel, view.zoom, { x: view.x + dx / scale, y: view.y + dy / scale }, c);
  if (Math.abs(next.x - view.x) + Math.abs(next.y - view.y) < 0.5) return false;
  moveCameraByHand();
  scrollRoom(id, next);
  drawMap(performance.now());
  return true;
}
function wheelUnit(e, size) {
  return e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? size : 1;
}
function wheelDelta(e, size) {
  const unit = wheelUnit(e, size);
  return (game.vertical ? e.deltaY : Math.abs(e.deltaX) > Math.abs(e.deltaY) ? e.deltaX : 0) * unit;
}
function levelOverview(id) {
  const map = maps.get(id), strip = document.createElement("canvas");
  strip.className = "level-overview";
  strip.dataset.map = id;
  if (game.vertical) {
    strip.height = TALL_CAMERA_VIEWPORT.desktop.height;
    strip.width = Math.max(8, Math.round(map.width * strip.height / map.height));
  } else if (game.grid) {
    const box = { width: 240, height: GRID_CAMERA_VIEWPORT.desktop.height }, fit = Math.min(box.width / map.width, box.height / map.height);
    strip.width = Math.max(8, Math.round(map.width * fit * 2));
    strip.height = Math.max(8, Math.round(map.height * fit * 2));
  } else {
    strip.width = 1600;
    strip.height = Math.max(8, Math.round(map.height * strip.width / map.width));
    strip.style.setProperty("--aspect", map.width / map.height);
  }
  strip.title = "The whole level. Click or drag to move the view.";
  strip.setAttribute("aria-hidden", "true");
  const seek = (e) => {
    const r = strip.getBoundingClientRect();
    moveCameraByHand();
    const c = mapCanvas(id);
    if (c) {
      const view = roomView(id, c);
      roomView(id).zoom = view.zoom = Math.max(view.zoom, closeUpZoom(c));
    }
    const x = ((e.clientX - r.left) / r.width) * map.width, y = ((e.clientY - r.top) / r.height) * map.height;
    scrollRoom(id, game.vertical ? { y } : { x, y });
    drawMap(performance.now());
  };
  strip.addEventListener("pointerdown", (e) => {
    seek(e);
    strip.setPointerCapture(e.pointerId);
  });
  strip.addEventListener("pointermove", (e) => {
    if (strip.hasPointerCapture(e.pointerId)) seek(e);
  });
  strip.addEventListener("wheel", (e) => {
    const c = mapCanvas(id), r = strip.getBoundingClientRect();
    if (!c) return;
    const view = roomView(id, c), panel = canvasPanel(c), scale = Math.min(c.width / panel.width, c.height / panel.height) * view.zoom;
    if (game.grid) {
      const unit = wheelUnit(e, r.height);
      if (scrollView(id, c, e.deltaX * unit * scale * map.width / r.width, e.deltaY * unit * scale * map.height / r.height)) e.preventDefault();
      return;
    }
    const delta = wheelDelta(e, game.vertical ? r.height : r.width) * (game.vertical ? map.height / r.height : map.width / r.width);
    if (delta && scrollAlong(id, c, delta * scale)) e.preventDefault();
  }, { passive: false });
  return strip;
}
function drawOverview(strip, now) {
  const id = Number(strip.dataset.map), map = maps.get(id), ctx = strip.getContext("2d");
  const kx = strip.width / map.width, ky = strip.height / map.height;
  ctx.imageSmoothingEnabled = true;
  ctx.fillStyle = "#122b42";
  ctx.fillRect(0, 0, strip.width, strip.height);
  const image = panorama(id);
  if (image) ctx.drawImage(image.small || image, 0, 0, strip.width, strip.height);
  ctx.fillStyle = "rgba(7,24,43,.4)";
  ctx.fillRect(0, 0, strip.width, strip.height);
  if (visualization !== "movement")
    for (const cell of heat.cells.values()) {
      if (cell.level !== id) continue;
      const color = heat.color(cell, now);
      if (!color) continue;
      ctx.fillStyle = `rgba(${color},.75)`;
      ctx.fillRect(cell.x * 32 * kx, cellTop(cell.y) * ky, 32 * kx, 32 * ky);
    }
  if (visualization !== "heat") {
    const dot = game.vertical ? 3 : 4;
    ctx.fillStyle = "#ffd96a";
    for (const p of swarmRooms.get(id) || [])
      ctx.fillRect(Math.round((p.x + 8) * kx - dot / 2), Math.round((p.y + 8) * ky - dot / 2), dot, dot);
  }
  const c = mapCanvas(id);
  if (!c) return;
  const view = roomView(id, c), panel = canvasPanel(c);
  const scale = Math.min(c.width / panel.width, c.height / panel.height) * view.zoom;
  const halfW = c.width / 2 / scale, halfH = c.height / 2 / scale, line = game.vertical ? 3 : 4;
  const x0 = Math.max(0, (view.x - halfW) * kx), x1 = Math.min(strip.width, (view.x + halfW) * kx);
  const y0 = Math.max(0, (view.y - halfH) * ky), y1 = Math.min(strip.height, (view.y + halfH) * ky);
  ctx.strokeStyle = "rgba(255,255,255,.95)";
  ctx.lineWidth = line;
  ctx.strokeRect(x0 + line / 2, y0 + line / 2, x1 - x0 - line, y1 - y0 - line);
}
function stopCamera() {
  camera.overview = false;
  camera.only = null;
  camera.follow = false;
  camera.focus = null;
  camera.introDone = true;
}
function moveCameraByHand() {
  camera.overview = false;
  camera.introDone = true;
  camera.focus = null;
  if (camera.follow) camera.manual = performance.now() + 10000;
}
function followSearch() {
  resumeFollowing(camera);
}
function levelIndex(room) {
  return catalog.levels.findIndex((level) => level.rooms.includes(room));
}
function levelRooms(room = mapLevel) {
  return catalog.levels.find((level) => level.rooms.includes(room))?.rooms || [room];
}
function cameraRooms() {
  return [mapLevel];
}
function drivenRooms() {
  return cameraRooms().filter((room) => camera.only == null || room === camera.only);
}
function applyCameraMode() {
  camera.pinned = null;
  followSearch();
  const room = followRoom(performance.now(), true);
  if (room != null) setRoom(room, true);
  drawMap(performance.now());
}
function roomLevel(room) {
  return catalog.levels.find((level) => level.rooms.includes(room))?.id ?? game.areaMap?.(room)?.level ?? null;
}
function leadingFinisherRoom(rooms) {
  let room = null, left = Infinity, trail = null;
  for (const id of rooms)
    for (const p of swarmRooms.get(id) || []) {
      if (!p.finish || !p.samples || !rooms.includes(trailRoom(p.samples))) continue;
      if (p.samples === camera.leader) return id;
      const remaining = p.samples.at(-MOTION_STRIDE) - p.frame;
      if (remaining < left) [room, left, trail] = [id, remaining, p.samples];
    }
  camera.leader = trail;
  return room;
}
function followRoom(now, immediate = false) {
  const lead = leadingFinisherRoom(levelRooms());
  if (lead != null) {
    camera.busierSince = 0;
    return lead === mapLevel ? null : lead;
  }
  const live = (id) => swarmRooms.get(id)?.length || 0;
  const occupied = live(mapLevel) > 0;
  const busiest = levelRooms().reduce((best, id) => (live(id) > live(best) * 1.5 ? id : best), mapLevel);
  if (busiest === mapLevel || (!immediate && occupied && now - camera.roomSince < ROOM_DWELL_MS)) {
    camera.busierSince = 0;
    return null;
  }
  camera.busierSince ||= now;
  return immediate || now - camera.busierSince > (occupied ? 2500 : 250) ? busiest : null;
}
function showPathLevel(level) {
  const room = catalog.levels.find((entry) => entry.id === level)?.rooms[0];
  if (room == null || !maps.has(room)) return;
  if (branchPreview) branchPreview.room = room;
  else setRoom(room);
}
function followPathExit(from, to, now) {
  const shown = roomLevel(branchPreview?.room ?? mapLevel);
  const following = shown === path.level || path.show?.from === shown;
  if (!followExit(path, from, to, catalog.levels)) return;
  path.since = now;
  path.idle = 0;
  if (following) path.show = { level: path.level, from: shown };
}
function followStalledPath(now) {
  if (!ready || path.level == null || paused) return;
  const canFollow = camera.follow && !camera.focus && camera.pinned == null && camera.manual <= now && camera.held <= now;
  if (path.show && canFollow) {
    const rooms = catalog.levels.find((entry) => entry.id === path.show.level)?.rooms || [];
    if (roomLevel(branchPreview?.room ?? mapLevel) !== path.show.from) path.show = null;
    else if (rooms.reduce((n, id) => n + (swarmRooms.get(id)?.length || 0), 0) >= 3) {
      showPathLevel(path.show.level);
      path.show = null;
    }
  }
  const counts = new Map();
  for (const [room, points] of swarmRooms) {
    const level = roomLevel(room);
    if (points.length && level != null) counts.set(level, (counts.get(level) || 0) + points.length);
  }
  const deepest = busierDeeperLevel(trailsSeen ? counts : pointLevels, path.level);
  const occupied = (counts.get(path.level) || 0) > 0;
  if (deepest == null || (occupied && !path.leftRoot && now - path.since < PATH_STALL_MS)) {
    path.idle = 0;
    return;
  }
  path.idle ||= now;
  if (!path.leftRoot && now - path.idle < (occupied ? PATH_STALL_MS : 250)) return;
  path.leftRoot = false;
  const shown = roomLevel(branchPreview?.room ?? mapLevel);
  const onPath = shown === path.level || path.show?.from === shown;
  path.level = deepest;
  if (!path.levels.includes(deepest)) path.levels.push(deepest);
  path.since = now;
  path.idle = 0;
  renderLevelStrip();
  if (onPath) {
    if (canFollow) {
      showPathLevel(deepest);
      path.show = null;
    } else path.show = { level: deepest, from: shown };
  }
}
function beginCamera(intro = !reducedMotion.matches && !tourSeen(), only = null) {
  resumeFollowing(camera);
  const introRoom = intro ? mapLevel : null;
  Object.assign(camera, { introStart: 0, last: 0, busierSince: 0, roomSince: 0, leader: null, jumped: 0, targets: new Map(), focus: null, follow: true, introDone: !intro, introRoom, only });
  for (const room of drivenRooms()) {
    const c = mapCanvas(room);
    if (c) steerCamera(room, c, game.grid ? game.entry(room) : { x: 0, y: maps.get(room).height }, closeUpZoom(c) * (room === introRoom ? INTRO_SCALE : 1), 1, 1);
  }
}
function updateCamera(now) {
  const dt = Math.min(0.1, camera.last ? (now - camera.last) / 1000 : 0);
  camera.last = now;
  document.querySelector(".map-wrap").dataset.camera = camera.focus ? "focus" : camera.follow ? "follow" : "idle";
  resetRetreat(camera, mapLevel);
  if (!ready || branchPreview || camera.focus || !camera.follow || paused || camera.held > now || camera.manual > now) resetRetreat(camera);
  if (!ready || branchPreview) return;
  const smooth = (seconds) => (reducedMotion.matches ? 1 : 1 - Math.exp(-dt / seconds));
  if (camera.focus) {
    const { point, start } = camera.focus, c = mapCanvas(point.level, point);
    if (!c) {
      camera.focus = null;
      return;
    }
    steerCamera(point.level, c, { x: point.x, y: point.y - 8 }, closeUpZoom(c), smooth(0.45), smooth(0.4));
    if (now - start >= 2600) {
      camera.focus = null;
      applyCameraMode();
    }
    return;
  }
  if (!camera.follow || paused || camera.held > now || camera.manual > now) return;
  camera.introStart ||= now;
  const t = now - camera.introStart;
  const scale = camera.introDone ? 1 : t < 1500 ? INTRO_SCALE : INTRO_SCALE + (1 - INTRO_SCALE) * ease(Math.min(1, (t - 1500) / 800));
  if (!camera.introDone && t >= 2300) camera.introDone = true;
  const steer = !reducedMotion.matches || now - camera.jumped >= 2000;
  if (steer) camera.jumped = now;
  if (steer && camera.pinned == null) {
    const room = followRoom(now);
    if (room != null) {
      setRoom(room, true);
      camera.busierSince = 0;
      camera.roomSince = now;
      return;
    }
  }
  for (const room of drivenRooms()) {
    const held = camera.targets.get(room);
    const point = trackingGoal(held, frontier(room), swarmRooms.get(room) || [], now,
      (point) => cameraProgress(room, point), CAMERA_RETREAT_MS);
    if (!point || !steer) continue;
    const c = mapCanvas(room, point);
    if (!c) continue;
    const view = roomView(room, c);
    let target = held;
    if (target?.canvas !== c) camera.targets.set(room, (target = { canvas: c, behind: held?.behind ?? 0 }));
    target.goal = point;
    const zoom = closeUpZoom(c) * (room === camera.introRoom ? scale : 1);
    if (reducedMotion.matches) steerCamera(room, c, point, zoom, 1, 1);
    else {
      Object.assign(view, followView(view, point, canvasPanel(c), c, zoom, dt, liveView));
      roomView(room).zoom = view.zoom;
    }
  }
}
let searchChoices = [{ id: 0, parent: null }],
  knownSearches = new Set([0]),
  traceReveal = 0,
  revealNext = false,
  transportPending = false,
  branchPreview = null,
  routeHover = null,
  previewTimer;
const routePreviews = new RoutePreviews(() => createEngine(base, game, engineInputs),
  budget.snapshotsMiB === 32 ? 8 : 16,
  (budget.snapshotsMiB === 32 ? 1 : 2) * 1048576,
  (previewEngine, frame) => {
    const observation = previewEngine.observation(), map = maps.get(observation.level);
    return map && isMapEvidence(observation, catalog.levels)
      ? { ...project(observation, map), frame }
      : { gap: true, frame };
  });
function hideRoutePreview() {
  clearTimeout(previewTimer);
  const painted = !!routeHover?.pixels;
  routeHover = null;
  routePreviews.cancel();
  if (painted) paintFilm();
}
function previewRoute(id, button) {
  if (controlMode || branchBusy || matchMedia("(hover: none)").matches) return;
  hideRoutePreview();
  const target = routeHover = { id, button };
  previewTimer = setTimeout(() => {
    if (routeHover !== target || !button.isConnected) return;
    const state = current?.id === id ? current : stateCache.get(id);
    if (state) showRoutePreview(state, target);
    else worker.postMessage({ type: "states", ids: [id], request: -2001 });
  }, 90);
}
async function showRoutePreview(state, target) {
  if (!state || target !== routeHover || !target.button.isConnected) return;
  try {
    const pixels = await routePreviews.get(state);
    if (!pixels || target !== routeHover || !target.button.isConnected) return;
    target.segments = trailSegments(routePreviews.timeline.trail);
    target.point = isMapEvidence(state.observation, catalog.levels)
      ? project(state.observation, maps.get(state.observation.level)) : null;
    target.frames = state.frames;
    drawMap(performance.now());
    target.pixels = pixels;
    paintFilm();
  } catch {
    if (target === routeHover) hideRoutePreview();
  }
}
function clearBranchPreview(restore = true) {
  if (!branchPreview) return;
  const room = branchPreview.room;
  branchPreview = null;
  $("branches").removeAttribute("data-preview");
  if (restore && mapLevel !== room) setRoom(room, true);
  drawMap(performance.now());
}
function previewBranch(id) {
  if (matchMedia("(hover: none)").matches) return;
  if (id === activeSearch || branchBusy || controlMode) return;
  clearBranchPreview();
  const view = searchViews.get(id);
  if (!view) return;
  branchPreview = { id, room: mapLevel };
  $("branches").dataset.preview = id;
  const room = view.room ?? view.branchOrigin?.point?.level ?? [...view.seenMaps][0];
  if (room !== undefined && room !== mapLevel) setRoom(room, true);
  drawMap(performance.now());
}
function searchTime(search) {
  return `${replayTime(search.origin.frames)}.${Math.floor(search.origin.frames % 60 / 6)}`;
}
function searchLabel(search) {
  return search.id === 0 ? "Main" : `${maps.get(search.origin.level)?.label ?? "Gameplay"} at ${searchTime(search)}`;
}
function renderBranches() {
  const pauseControl = $("pause"), resetControl = $("reset");
  const list = (parent, depth = 0) => {
    const ol = document.createElement("ol");
    for (const search of searchChoices.filter((s) => s.parent === parent)) {
      const li = document.createElement("li"), button = document.createElement("button");
      li.dataset.searchNode = search.id;
      button.dataset.search = search.id;
      const label = searchLabel(search);
      button.textContent = "";
      const iconURL = branchIcon();
      if (iconURL) {
        const icon = document.createElement("img");
        icon.className = "branch-icon";
        icon.src = iconURL;
        icon.alt = "";
        button.append(icon);
      }
      if (search.id === 0) button.append("Main");
      if (search.id !== 0) {
        const time = document.createElement("span");
        time.className = "search-time";
        time.textContent = searchTime(search);
        button.append(time);
        const room = document.createElement("small");
        room.className = "search-origin";
        room.textContent = maps.get(search.origin.level)?.label ?? "Gameplay";
        button.append(room);
        button.setAttribute("aria-label", `Search from ${label}, branch ${search.id}`);
      }
      if (depth > 3) {
        const parentSearch = searchChoices.find((s) => s.id === search.parent);
        const ancestry = document.createElement("small");
        ancestry.className = "search-parent";
        ancestry.textContent = `from ${parentSearch?.id ? searchTime(parentSearch) : "Main"}`;
        button.append(ancestry);
      }
      button.setAttribute("aria-pressed", search.id === activeSearch);
      if (!knownSearches.has(search.id)) {
        knownSearches.add(search.id);
        li.classList.add("fresh");
      }
      button.title = `${search.origin ? `Search from ${label} · frame ${fmt(search.origin.frames)} · branch ${search.id}` : "Original search"}`;
      button.disabled = branchBusy || controlMode;
      button.onclick = () => switchSearch(search.id);
      button.onpointerenter = (e) => { if (e.pointerType === "mouse") previewBranch(search.id); };
      button.onfocus = () => { if (button.matches(":focus-visible")) previewBranch(search.id); };
      button.onpointerleave = button.onblur = () => clearBranchPreview();
      const row = document.createElement("div");
      row.className = "timeline-row";
      row.append(button);
      const actions = document.createElement("div");
      actions.className = "timeline-controls";
      const play = search.id === activeSearch ? pauseControl : document.createElement("button");
      if (search.id !== activeSearch) {
        play.className = "icon-button";
        play.dataset.resumeSearch = search.id;
        play.setAttribute("aria-label", `Resume ${label}`);
        play.title = `Resume ${label}`;
        play.innerHTML = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M7 4l14 8-14 8z"/></svg>';
        play.onclick = () => switchSearch(search.id);
      }
      const action = search.id === 0 ? resetControl : document.createElement("button");
      if (search.id !== 0) {
        action.className = "icon-button";
        action.dataset.deleteSearch = search.id;
        action.setAttribute("aria-label", `Delete ${label}`);
        action.title = `Delete ${label}`;
        action.innerHTML = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 6h16M9 6V3h6v3M6 6l1 15h10l1-15M10 10v7m4-7v7" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/></svg>';
        action.onclick = () => deleteSearch(search.id);
      }
      play.disabled = action.disabled = branchBusy || controlMode || !ready;
      actions.append(play, action);
      row.append(actions);
      li.append(row);
      if (searchChoices.some((s) => s.parent === search.id)) li.append(list(search.id, depth + 1));
      ol.append(li);
    }
    return ol;
  };
  $("branch-tree").replaceChildren(...list(null).children);
  const button = $("branch-tree").querySelector('[aria-pressed="true"]');
  if (button) {
    const nav = button.closest("nav"), row = button.getBoundingClientRect(), bounds = nav.getBoundingClientRect();
    if (row.bottom > bounds.bottom) nav.scrollTop += row.bottom - bounds.bottom;
    else if (row.top < bounds.top) nav.scrollTop -= bounds.top - row.top;
  }
}
const timeline = new ReplayTimeline(
  (budget.snapshotsMiB === 32 ? 2 : 4) * 1048576,
);
const music = new GameAudio(() => createEngine(base, game, engineInputs));
const panoramas = new Map();
function mapURL(level) {
  const url = new URL(maps.get(level).file, assets);
  url.searchParams.set("v", "level-data-1");
  return url.href;
}
const builtMaps = { workers: [], ready: false, queue: [], wanted: null, waiting: new Map() };
function imageCanvas({ width, height, data }) {
  const image = document.createElement("canvas");
  image.width = width;
  image.height = height;
  image.getContext("2d").putImageData(new ImageData(new Uint8ClampedArray(data.buffer || data), width, height), 0, 0);
  return image;
}
function nextBuiltMap() {
  if (!builtMaps.ready) return;
  for (const slot of builtMaps.workers) {
    if (!slot.ready || slot.busy || !builtMaps.queue.length) continue;
    const at = Math.max(0, builtMaps.queue.indexOf(builtMaps.wanted));
    const [level] = builtMaps.queue.splice(at, 1);
    slot.busy = true;
    slot.worker.postMessage({ type: "map", level });
  }
}
function builtMapKey(level) {
  return `${game.id}:${game.rom.sha256}:map-v2:${level}`;
}
function tiledMap({ width, height, tiles }) {
  const fit = Math.min(1, 1600 / Math.max(width, height)), small = document.createElement("canvas");
  small.width = Math.round(width * fit);
  small.height = Math.round(height * fit);
  const ctx = small.getContext("2d");
  return Promise.all(tiles.map(async (tile) => {
    const bitmap = await createImageBitmap(new ImageData(new Uint8ClampedArray(tile.data.buffer || tile.data), tile.width, tile.height));
    ctx.drawImage(bitmap, tile.x * fit, tile.y * fit, tile.width * fit, tile.height * fit);
    return { x: tile.x, y: tile.y, width: tile.width, height: tile.height, bitmap };
  })).then((list) => ({ width, height, tiles: list, small }));
}
function showBuiltMap(level, image) {
  (image.tiles ? tiledMap(image) : createImageBitmap(image)).then((bitmap) => {
    panoramas.set(level, bitmap);
    for (const resolve of builtMaps.waiting.get(level) || []) resolve();
    builtMaps.waiting.delete(level);
    addAreaMap(level, bitmap);
    for (const preview of document.querySelectorAll(`canvas.level-preview[data-level="${level}"]`)) drawLevelPreview(preview, bitmap);
  });
}
function startMapWorker() {
  if (builtMaps.workers.length) return;
  const count = Math.max(1, Math.min(3, (navigator.hardwareConcurrency || 4) >> 2));
  for (let i = 0; i < count; i++) {
    const slot = { worker: new Worker(new URL("./map-worker.js", import.meta.url), { type: "module" }), ready: false, busy: false };
    slot.worker.onmessage = ({ data }) => {
      if (data.type === "error" && data.level === undefined) return showError(data.message);
      if (data.type === "error") {
        console.warn(`Map ${data.level} did not build: ${data.message}`);
        slot.busy = false;
      } else if (data.type === "sprites") {
        imageCanvas(data).toBlob((blob) => (sprites.src = URL.createObjectURL(blob)));
        builtMaps.ready = slot.ready = true;
      } else if (data.type === "ready") {
        slot.ready = true;
      } else if (data.type === "map" && data.tiles) {
        showBuiltMap(data.level, data);
        slot.busy = false;
      } else if (data.type === "map") {
        const image = imageCanvas(data);
        showBuiltMap(data.level, image);
        image.toBlob((blob) => blob && save(builtMapKey(data.level), blob));
        slot.busy = false;
      }
      nextBuiltMap();
    };
    slot.worker.postMessage({ type: "init", base: base.href, game: game.id, rom: engineInputs.rom, sprites: i === 0 });
    builtMaps.workers.push(slot);
  }
}
async function buildMapInBrowser(level) {
  const cached = await load(builtMapKey(level));
  if (cached instanceof Blob) return showBuiltMap(level, cached);
  startMapWorker();
  builtMaps.queue.push(level);
  nextBuiltMap();
}
function mapBuilt(level) {
  if (panorama(level)) return Promise.resolve();
  return new Promise((resolve) => builtMaps.waiting.set(level, [...(builtMaps.waiting.get(level) || []), resolve]));
}
function prefetchNextMap(level) {
  const next = catalog.levels[levelIndex(level) + 1]?.rooms[0];
  if (!game.buildMap || next == null || panoramas.has(next)) return;
  panoramas.set(next, null);
  buildMapInBrowser(next);
}
function discoverArea(id) {
  if (maps.has(id) || panoramas.has(id) || !game.areaMap?.(id)) return;
  panorama(id);
}
function addAreaMap(id, bitmap) {
  const info = game.areaMap?.(id);
  if (!info || maps.has(id)) return;
  maps.set(id, { ...info, width: bitmap.width, height: bitmap.height });
  const owner = catalog.levels.find((level) => level.id === info.level);
  owner.rooms.push(id);
  if (owner.rooms.includes(mapLevel)) {
    renderMapRows(owner);
    drawMap(performance.now());
  }
}
function drawLevelPreview(preview, image) {
  const bitmap = image.small || image;
  const height = bitmap.height, width = Math.min(bitmap.width, Math.round((height * preview.width) / preview.height));
  preview.getContext("2d").drawImage(bitmap, 0, 0, width, height, 0, 0, preview.width, preview.height);
}
function panorama(level) {
  if (game.buildMap) {
    builtMaps.wanted = level;
    if (!panoramas.has(level)) {
      panoramas.set(level, null);
      buildMapInBrowser(level);
    }
    return panoramas.get(level);
  }
  if (!panoramas.has(level)) {
    panoramas.set(level, null);
    fetch(mapURL(level))
      .then((response) => response.blob())
      .then(createImageBitmap)
      .then((bitmap) => panoramas.set(level, bitmap))
      .catch(() => {});
  }
  return panoramas.get(level);
}
if (game.buildMap) {
  startMapWorker();
  panorama(bootLevel);
  prefetchNextMap(bootLevel);
}
const canvas = $("map"),
  film = $("film").getContext("2d");
const numberFormat = new Intl.NumberFormat(),
  fmt = (n) => numberFormat.format(Math.round(n || 0));
function showError(error) {
  $("error").hidden = false;
  $("error").textContent = String(error?.message || error);
}
function replayError(error) {
  fail(error);
}
function fail(error) {
  playing = false;
  music.stop();
  worker?.postMessage({ type: "pause" });
  $("error").hidden = false;
  $("error").textContent = String(error?.message || error);
  $("status").textContent = "Search paused";
  paused = true;
  updateSearchControl();
  $("status-dot").className = "paused";
}
function paintFilm() {
  if (!engine) return;
  const preview = routeHover?.pixels,
    p = preview || (currentFrame === 0 && originPixels ? originPixels : engine.pixels());
  film.putImageData(new ImageData(p.data, p.width, p.height), 0, 0);
  $("film").dataset.previewRoute = preview ? routeHover.id : "";
  $("frame-label").textContent = preview
    ? `${routeName(routeHover.id)} · ${replayTime(routeHover.frames)}`
    : `FRAME ${fmt(currentFrame)} / ${fmt(current?.frames)}`;
}
function drawFilm() {
  frameObservation = engine?.observation();
  if (
    frameObservation &&
    ((!playing && !controlMode) || performance.now() - lastDetailPaint > 150)
  ) {
    renderDetails(frameObservation);
    lastDetailPaint = performance.now();
    updateBranchControls();
  }
  if (!playing && !controlMode) updateBranchControls();
  paintFilm();
  renderedFrame = currentFrame;
  followReplayRoom(
    frameObservation && isMapEvidence(frameObservation, catalog.levels)
      ? project(frameObservation, maps.get(frameObservation.level))
      : null,
  );
  if (!seeking) $("scrub").value = currentFrame;
}
function advanceFilm(target, paint = true, renderAt = target) {
  while (currentFrame < target) {
    const action = timeline.actionAt(currentFrame);
    if (!action) break;
    const n = Math.min(
      action.frames,
      target - currentFrame,
      traceStride - (currentFrame % traceStride),
      timeline.interval - (currentFrame % timeline.interval),
    );
    engine.run(action.buttons, n, currentFrame + n === renderAt);
    if (playing && !seeking)
      music.advance(action.buttons, n, Number($("speed").value));
    currentFrame += n;
    if (currentFrame % timeline.interval === 0)
      timeline.put(currentFrame, engine.capture());
    if (currentFrame % traceStride === 0 && currentFrame > traceMaxFrame)
      recordTrail();
  }
  if (currentFrame === current.frames && currentFrame > traceMaxFrame)
    recordTrail();
  if (paint) drawFilm();
}
function markerPoint() {
  if (seeking)
    return (
      trailPoint(trace, requestedFrame) ||
      (requestedFrame === current?.frames &&
      isMapEvidence(current.observation, catalog.levels)
        ? project(current.observation, maps.get(current.observation.level))
        : null)
    );
  return frameObservation && isMapEvidence(frameObservation, catalog.levels)
    ? project(frameObservation, maps.get(frameObservation.level))
    : null;
}
function followReplayRoom(point) {
  if (branchPreview) return;
  if (!point || !userSelected || $("inspector").hidden) return;
  const changed = point.level !== mapLevel;
  if (changed) setRoom(point.level, true);
  const c = mapCanvas(point.level, point);
  if (!c) return;
  const view = roomView(point.level, c), row = c.closest(".room-part");
  Object.assign(view, panelCenter(canvasPanel(c), view.zoom, { x: point.x, y: point.y - 8 }, c));
  const changedPart = row !== followReplayRoom.part;
  followReplayRoom.part = row;
  const phone = matchMedia("(max-width: 800px)").matches;
  if (document.body.classList.contains("tour-phone")) return;
  if (changed || changedPart || (phone && revealReplayRoom)) {
    revealReplayRoom = false;
    if (phone && $("inspector").classList.contains("expanded")) return;
    const bounds = row.getBoundingClientRect(),
      sheet = $("inspector").getBoundingClientRect(),
      bottom = phone ? sheet.top - 12 : innerHeight - 20;
    if (bounds.top < 20 || bounds.bottom > bottom)
      window.scrollBy({ top: bounds.top - 24, behavior: "instant" });
  }
}
function recordTrail() {
  timeline.trail = trace;
  if (trace.at(-1)?.frame === currentFrame) return;
  const o = engine.observation(),
    map = maps.get(o.level);
  const point =
    map && isMapEvidence(o, catalog.levels)
      ? { ...project(o, map), frame: currentFrame }
      : { gap: true, frame: currentFrame };
  const last = trace.at(-1);
  if (!point.gap || !last?.gap) trace.push(point);
  traceMaxFrame = currentFrame;
}
async function verify() {
  if (!current?.snapshot && !current?.digest) {
    $("verification").textContent = current?.frames
      ? "Controller history"
      : "Original game";
    return;
  }
  const endpoint = engine.capture();
  $("verification").textContent = (current.snapshot ? equal(endpoint, current.snapshot) : snapshotDigest(endpoint) === current.digest)
    ? "Exact replay ✓"
    : "Replay differs";
  if ($("verification").textContent === "Exact replay ✓") {
    historyVerified = true;
    if (!current.frames) $("verification").textContent = "Original game";
  }
  if ($("verification").textContent === "Replay differs")
    throw new Error(
      "Replay verification failed. This history does not match its archived snapshot.",
    );
}
function equal(a, b) {
  return a.length === b.length && a.every((v, i) => v === b[i]);
}
const yieldChannel = new MessageChannel(), yieldQueue = [];
yieldChannel.port1.onmessage = () => yieldQueue.shift()?.();
function yieldToBrowser() {
  return new Promise((resolve) => {
    yieldQueue.push(resolve);
    yieldChannel.port2.postMessage(0);
  });
}
async function seek(target) {
  if (!current || !engine) return;
  stopControl();
  music.stop();
  const epoch = ++replayEpoch;
  playing = false;
  seeking = true;
  updateBranchControls();
  seekStarted = performance.now();
  $("verification").textContent = "Replaying";
  target = Math.max(0, Math.min(current.frames, Math.floor(target)));
  requestedFrame = target;
  $("scrub").value = target;
  followReplayRoom(markerPoint());
  const checkpoint = timeline.before(target);
  if (
    currentFrame > target ||
    currentFrame < checkpoint.frame ||
    (currentFrame === target && renderedFrame !== target)
  ) {
    engine.restore(checkpoint.snapshot || origin);
    currentFrame = checkpoint.frame;
  }
  const startedAt = currentFrame;
  $("film").dataset.seekStart = startedAt;
  try {
    while (currentFrame < target && epoch === replayEpoch) {
      const deadline = performance.now() + 12;
      do {
        advanceFilm(Math.min(target, currentFrame + 120), false, target);
      } while (currentFrame < target && performance.now() < deadline);
      if (currentFrame < target) {
        segments = trailSegments(trace);
        await yieldToBrowser();
      }
    }
    if (epoch !== replayEpoch) return false;
    drawFilm();
    if (currentFrame === current.frames) await verify();
    else $("verification").textContent = "Frame " + fmt(currentFrame);
    segments = trailSegments(trace);
    return true;
  } catch (e) {
    replayError(e);
    return false;
  } finally {
    if (epoch === replayEpoch) {
      seeking = false;
      $("film").closest(".screen").classList.remove("updating");
      updateBranchControls();
      $("play").textContent = "▶ Replay";
      $("scrub").value = currentFrame;
    }
  }
}
async function selectState(state, autoplay = false) {
  if (!engine) return;
  heat.select(selectedCell, state.id);
  stopControl();
  current = state;
  const shared = timeline.reset(state.actions, state.frames);
  requestedFrame = 0;
  historyVerified = false;
  traceStride = Math.max(24, Math.ceil(state.frames / 6000));
  trace = prefixTrail(timeline.trail, shared, traceStride);
  timeline.trail = trace;
  segments = trailSegments(trace);
  traceMaxFrame = trace.at(-1)?.frame ?? -1;
  $("inspector").dataset.empty = "false";
  $("selection-hint").hidden = true;
  recordTrailAtRoot();
  frameCredit = 0;
  $("film-title").textContent = "History";
  $("film-title").dataset.stateId = state.id;
  $("film-title").title = `State #${state.id} · ${fmt(state.frames)} frames`;
  $("scrub").max = state.frames;
  for (const id of ["play", "scrub"]) $(id).disabled = false;
  if (
    isMapEvidence(state.observation, catalog.levels) &&
    state.observation.level !== mapLevel
  )
    setRoom(state.observation.level);
  renderStates();
  const selectionStarted = performance.now();
  const selection = seek(autoplay ? 0 : state.frames),
    epoch = replayEpoch;
  const success = await selection;
  if (!success || current !== state || epoch !== replayEpoch) return false;
  if (autoplay) {
    if (userSelected && music.context) startAudio();
    playing = true;
    viewerReplay = userSelected;
    $("play").textContent = "Ⅱ Pause";
  }
  if (!autoplay) routePreviews.remember(state.id, state.frames === 0 ? originPixels : engine.pixels());
  if (!autoplay && revealNext) startTraceReveal(performance.now() - selectionStarted < 350);
  revealNext = false;
  renderStates();
  return true;
}
function renderDetails(observation) {
  $("details").replaceChildren();
  const fields = [
    ["Position", `${observation.x}, ${observation.y}`],
    ...(game.maxHealth ? [["Health", `${observation.health} / ${game.maxHealth}`]] : []),
    ...(observation.percent === undefined ? [] : [["Level progress", `${observation.percent}%`]]),
    ["Levels cleared", levelProgress(observation).cleared - bootLevel],
    [
      "Level",
      isMapEvidence(observation, catalog.levels)
        ? maps.get(observation.level)?.label
        : "Level transition",
    ],
    ["Controller actions", current?.actions.length || 0],
    ["State ID", current?.id ?? "—"],
    ["Frame", fmt(currentFrame)],
  ];
  for (const [name, value] of fields) {
    const div = document.createElement("div");
    const label = document.createElement("span");
    label.textContent = name;
    const b = document.createElement("b");
    b.textContent = value;
    div.append(label, b);
    $("details").append(div);
  }
}
function recordTrailAtRoot() {
  engine.restore(origin);
  currentFrame = 0;
  if (!trace.length) recordTrail();
}
function replayTime(frames) {
  const seconds = Math.floor(frames / 60);
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}
function routeName(id) {
  if (String(id).startsWith("manual-")) return "Your branch";
  const number = selectedCell?.routes?.get(id);
  return number !== undefined ? `Path ${number}` : "History";
}
function renderStates() {
  if (current) {
    $("film-title").textContent = "History";
    $("film-title").dataset.stateId = current.id;
    $("film-title").title =
      `State #${current.id} · ${fmt(current.frames)} frames`;
  }
  const ids = selectedCell
    ? routeIds(selectedCell, current?.id)
    : [current?.id].filter((x) => x !== undefined);
  $("cell-title").textContent = selectedCell
    ? "Saved paths to this spot"
    : "History";
  $("cell-title").title = selectedCell
    ? `Cell ${selectedCell.x}, ${selectedCell.y}`
    : "";
  $("state-list").replaceChildren(
    ...ids.map((id) => {
      const renderedHeat = heat, renderedCell = selectedCell;
      const state = current?.id === id ? current : stateCache.get(id),
        ordinal = renderedCell?.routes.get(id),
        selected = current?.id === id,
        button = document.createElement("button"),
        marker = document.createElement("span"),
        summary = document.createElement("span"),
        name = document.createElement("span"),
        duration = document.createElement("span"),
        resources = document.createElement("span");
      button.className = "state" + (selected ? " selected" : "");
      button.dataset.stateId = id;
      button.title = state
        ? `State #${id} · ${fmt(state.frames)} frames`
        : `State #${id}`;
      marker.className = "state-marker";
      marker.setAttribute("aria-hidden", "true");
      marker.textContent = selected ? "✓" : "○";
      summary.className = "state-summary";
      name.className = "state-name";
      name.textContent = routeName(id);
      duration.textContent = state
        ? replayTime(state.frames)
        : "Loading…";
      summary.append(name, duration);
      resources.className = "state-resources";
      resources.textContent = state && game.maxHealth
        ? `♥ ${state.observation.health}/${game.maxHealth}`
        : "";
      button.append(marker, summary, resources);
      button.setAttribute("aria-pressed", selected);
      if (state)
        button.setAttribute(
          "aria-label",
          `${routeName(id)}, ${Math.floor(state.frames / 60)} seconds of gameplay${game.maxHealth ? `, health ${state.observation.health} of ${game.maxHealth}` : ""}`,
        );
      button.disabled = controlMode || branchBusy;
      button.onpointerenter = () => previewRoute(id, button);
      button.onfocus = () => previewRoute(id, button);
      button.onpointerleave = button.onblur = hideRoutePreview;
      button.onclick = () => {
        if (branchBusy || heat !== renderedHeat || selectedCell !== renderedCell) return;
        hideRoutePreview();
        if (tourSession) tourSession.interacted = true;
        userSelected = true;
        revealReplayRoom = true;
        revealNext = true;
        if (state) {
          heat.select(renderedCell, state.id, ordinal);
          invalidateStateRequest();
          selectState(state).then(revealTransport).catch(fail);
        } else {
          transportPending = true;
          requestStates([id], renderedCell, ordinal);
        }
      };
      return button;
    }),
  );
}
function revealTransport(ok) {
  transportPending = false;
  if (ok && compactReplay.matches && !$("inspector").hidden)
    document.querySelector(".transport").scrollIntoView({ block: "nearest", behavior: "instant" });
}
function inspect(cell) {
  hideRoutePreview();
  if (!cell || branchBusy) return;
  if (cell.ids.length) stopCamera();
  revealNext = true;
  stopControl();
  music.stop();
  playing = false;
  invalidateStateRequest();
  $("inspector").dataset.empty = cell.ids.length ? "false" : "true";
  $("selection-hint").hidden = !!cell.ids.length;
  userSelected = true;
  clearRouteSelection();
  selectedCell = cell.ids.length ? cell : null;
  revealReplayRoom = true;
  ++replayEpoch;
  seeking = false;
  current = null;
  historyVerified = false;
  segments = [];
  trace = [];
  $("verification").textContent = "Loading selected history";
  $("film-title").textContent = "History";
  updateBranchControls();
  $("cell-title").textContent = `Cell ${cell.x}, ${cell.y}`;
  $("selection-hint").textContent = "";
  renderStates();
  if (cell.ids.length) {
    openInspector();
    requestStates(cell.ids, cell);
  } else {
    closeInspector({ restoreFocus: false });
    applyCameraMode();
  }
}
function startSearch(level = bootLevel) {
  terminalDrain = null;
  if (!catalog.levels.some((entry) => entry.id === level)) throw new Error("Unknown level");
  bootLevel = level;
  if (engine) {
    origin = engine.boot(bootLevel);
    originPixels = engine.pixels();
  }
  stopControl();
  playSession = null;
  music.stop();
  branchBusy = false;
  activeSearch = 0;
  searchViews.clear();
  branchOrigin = pendingOrigin = originPulse = null;
  $("branch-feedback").textContent = "";
  timeline.clear();
  trace = [];
  segments = [];
  views.clear();
  closeInspector();
  searchChoices = [{ id: 0, parent: null }];
  branchPreview = null;
  hideRoutePreview();
  routePreviews.clear();
  renderBranches();
  $("branch-message").hidden = true;
  if (worker) worker.terminate();
  for (const rollout of rolloutWorkers) rollout.terminate();
  heat = new Heatmap();
  swarm.clear();
  swarmRooms = new Map();
  stateCache = new StateCache();
  selectedCell = null;
  clearRouteSelection();
  current = null;
  replayEpoch++;
  playing = false;
  userSelected = false;
  if (liveVisualization !== null) setVisualization(liveVisualization, false);
  liveVisualization = null;
  camera.pinned = null;
  gameWon = false;
  paused = false;
  replayPaused = replayWatching = false;
  ready = false;
  seeking = false;

  bestByMap.clear();
  bestStates.clear();
  arrivals.clear();
  seenMaps.clear();
  cleared.clear();
  wins.clear();
  path = { level: bootLevel, levels: [bootLevel], since: 0, idle: 0 };
  latestBest = null;
  lastAuto = 0;
  stats = {};
  for (const counter of Object.values(counters)) counter.reset();
  for (const id of ["play", "scrub"]) $(id).disabled = true;
  $("details").replaceChildren();
  $("state-list").replaceChildren();
  updateStats();
  knownSearches = new Set([0]);
  setRoom(bootLevel);
  beginCamera();

  $("error").hidden = true;
  $("memory-limit").hidden = true;
  $("status").textContent = `Loading ${game.title}…`;
  $("pause").disabled = true;
  updateSearchControl();

  $("status-dot").className = "";
  $("cell-title").textContent = "Saved paths";
  const ports = [];
  rolloutWorkers = liveView ? [] : Array.from({ length: budget.workers }, () => {
    const rollout = new Worker(new URL("./rollout-worker.js", import.meta.url), { type: "module" });
    const channel = new MessageChannel();
    rollout.onerror = (e) => fail(e.message || "Rollout worker failed");
    rollout.postMessage({ port: channel.port1 }, [channel.port1]);
    ports.push(channel.port2);
    return rollout;
  });
  worker = liveView
    ? liveSearch(game.id)
    : new Worker(new URL("./search-worker.js", import.meta.url), { type: "module" });
  worker.onerror = (e) => fail(e.message);
  worker.onmessage = ({ data }) => {
    try {
      if (data.type === "error") {
        fail(data.message);
        return;
      }
      if (data.type === "branch-error") {
        pendingOrigin = null;
        branchBusy = false;
        $("branch-message").hidden = false;
        $("branch-message").textContent = data.message;
        updateBranchControls();
        return;
      }
      if (data.type === "ready") {
        const initial = !ready;
        if (!initial) saveSearchView();
        if (data.deleted !== undefined) {
          searchViews.delete(data.deleted);
          swarm.remove(data.deleted);
          stateCache = new StateCache();
          routePreviews.clear();
        }
        ready = true;
        if (activeSearch !== data.active) camera.targets = new Map();
        activeSearch = data.active;
        const fresh = activateSearchView(activeSearch),
          handedOff = fresh && !!pendingOrigin;
        originPulse = null;
        if (handedOff) {
          branchOrigin = pendingOrigin;
          originPulse = { point: branchOrigin.point, start: performance.now() };
          saveSearchView();
        }
        pendingOrigin = null;
        invalidateStateRequest();
        selectedCell = hoverCell = null;
        clearRouteSelection();
        paused = data.paused;
        branchBusy = false;
        $("branch-message").hidden = true;
        searchChoices = data.searches;
        const focusBranch = !!document.activeElement.closest("#branch-tree");
        renderBranches();
        if (focusBranch) $("branch-tree").querySelector(`[data-search="${activeSearch}"]`).focus({ preventScroll: true });
        $("memory-limit").hidden = true;
        updateStats();
        $("status").textContent = "Exploring";
        updateSearchControl();
        $("pause").disabled = false;
        $("reset").disabled = false;
        const state = data.state;
        stateCache.set(state.id, state);
        if (fresh) {
          mergeProgress(cleared, levelProgress(state.observation), bootLevel);
          for (const level of completedLevels(levelProgress(state.observation), bootLevel))
            wins.set(level, state.id);
          const start = roomLevel(state.observation.level) ?? bootLevel;
          const leftRoot = finishedLevel(levelProgress(state.observation), start) != null;
          Object.assign(path, { level: start, levels: [start], since: performance.now(), idle: 0, leftRoot });
        }
        if (isMapEvidence(state.observation, catalog.levels)) {
          seenMaps.add(state.observation.level);
            if (fresh) {
            bestByMap.set(state.observation.level, state.observation.x);
            bestStates.set(state.observation.level, state.id);
            heat.visit(
              {
                observation: project(
                  state.observation,
                  maps.get(state.observation.level),
                ),
                retained: state.id,
              },
              performance.now(),
            );
          }
        }
        if (handedOff) {
          closeInspector();
          revealBranchOrigin();
          if (branchOrigin.point) {
            camera.follow = false;
            camera.focus = { point: branchOrigin.point, start: performance.now() };
          }
          $("branch-feedback").textContent =
            `${searchLabel(searchChoices.find((s) => s.id === activeSearch))} is searching from this frame.`;
          updateBranchControls();
          $("branch-tree").querySelector(`[data-search="${activeSearch}"]`).focus({ preventScroll: true });

        }
        if (tapeUrl) startTape();
        else if (engine) selectState(state, initial).catch(fail);
        updateStats();
        drawMap(performance.now());
      } else if (data.type === "batch") {
        if (data.active !== activeSearch) return;
        const { motion, finishes, exits, ...batchStats } = data;
        swarm.add(activeSearch, motion || [], heat.clock(performance.now()), finishes, liveView && !data.stopped ? 100 : 0);
        if (motion?.length) trailsSeen = true;
        else if (liveView && !trailsSeen) {
          for (const [level, count] of pointLevels) pointLevels.set(level, count * 0.9);
          for (const point of data.points) {
            const level = roomLevel(point.observation.level);
            if (level != null) pointLevels.set(level, (pointLevels.get(level) || 0) + 1);
          }
          if (++traillessBatches === 20 && visualization === "movement") setVisualization("heat");
        }
        stats = batchStats;
        if (liveView) for (const [key, counter] of Object.entries(counters)) counter.push(batchStats[key] || 0, performance.now());
        gameWon ||= data.won;
        const now = performance.now();
        heat.visitMotion(data.motion || [], maps, now);
        for (const point of data.points) {
          const o = point.observation,
            map = maps.get(o.level),
            finished = finishedLevel(levelProgress(o), roomLevel(o.level));
          if (finished != null) {
            cleared.add(finished);
            if (!game.levelsCleared) followPathExit(finished, levelProgress(o).cleared, now);
            if (!wins.has(finished) && point.retained !== null) wins.set(finished, point.retained);
          }
          if (!map && !o.loading && o.health) discoverArea(o.level);
          if (!map || !isMapEvidence(o, catalog.levels)) continue;
          const first = !seenMaps.has(o.level);
          seenMaps.add(o.level);
          if (point.retained !== null && !arrivals.has(o.level))
            arrivals.set(o.level, point.retained);
          const best = bestByMap.get(o.level) || 0;
          if (o.x > best && point.retained !== null) {
            bestByMap.set(o.level, o.x);
            bestStates.set(o.level, point.retained);
            if (o.level === mapLevel || first) latestBest = point.retained;
          }
          heat.retain({ ...point, observation: project(o, map) }, now);
        }
        (finishes || []).forEach((index, i) => {
          const from = roomLevel(trailRoom(motion[index]));
          if (from == null || game.levelsCleared) return;
          cleared.add(from);
          followPathExit(from, exits?.[i], now);
        });
        if (
          latestBest !== null &&
          !userSelected &&
          now - lastAuto > 8000 &&
          !seeking
        ) {
          lastAuto = now;
          worker.postMessage({ type: "states", ids: [latestBest], request: 0 });
        }
        statsDirty = true;
        offerTour();
      } else if (data.type === "states") {
        for (const state of data.states) stateCache.set(state.id, state);
        if (data.missing?.length) replaceDropped(heat.forget(data.missing, data.request));
        if (data.request === -2001) {
          if (routeHover) showRoutePreview(stateCache.get(routeHover.id), routeHover);
        } else if (data.request === 0) {
          const state = data.states[0];
          if (!state) lastAuto = 0;
          if (!userSelected && state && (!isMapEvidence(state.observation, catalog.levels) || levelRooms().includes(state.observation.level)))
            selectState(state, true).catch(fail);
        } else if (data.request === request) {
          renderStates();
          if (data.states[0])
            selectState(data.states[0], watchRequested).then((ok) => transportPending && revealTransport(ok)).catch(fail);
          else $("verification").textContent = "The search no longer keeps a path to this spot";
          watchRequested = false;
          heat.releasePending(data.request);
        }
      } else if (data.type === "limit") {
        const clock = heat.clock(performance.now()), end = swarm.endTime(activeSearch, clock);
        terminalDrain = !paused && !reducedMotion.matches && end > clock ? { branch: activeSearch, end } : null;
        paused = !terminalDrain;
        replayPaused = false;
        const ended = data.finished ? "Search finished" : "Memory limit reached";
        $("status").textContent = data.won ? "Game won" : ended;
        updateSearchControl();
        $("goal-status").textContent = data.won ? "Game complete" : ended;
        $("pause").disabled = true;
        $("memory-limit").hidden = data.won || data.finished;
        $("memory-limit").textContent = `The search used its ${fmt(budget.searchMiB)} MiB memory limit. Delete a branch or restart the search to free memory.`;
        $("status-dot").className = "paused";
      }
    } catch (e) {
      fail(e);
    }
  };
  const init = { type: "init", base: base.href, game: game.id, rom: engineInputs.rom, seed, budget, pace, ports, boot_level: bootLevel };
  if (!game.buildMap) return worker.postMessage(init, ports);
  const starting = worker;
  mapBuilt(bootLevel).then(() => starting.postMessage(init, ports));
}
function renderPauseButton() {
  $("pause").innerHTML = paused
    ? '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M7 4l14 8-14 8z"/></svg>'
    : '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M6 4h4v16H6zm8 0h4v16h-4z"/></svg>';
  const label = paused ? "Resume Search" : "Pause Search";
  $("pause").setAttribute("aria-label", label);
  $("pause").title = label;
}
function updateSearchControl() {
  if (paused || terminalDrain?.branch !== activeSearch) terminalDrain = null;
  heat.setRunning(ready && !paused && (!stats.stopped || !!terminalDrain), performance.now());
  renderPauseButton();
  $("goal-status").textContent = !ready
    ? "Loading…"
    : paused
      ? "Paused"
      : "Searching";
}
function setRoom(level, preserveSelection = false) {
  const map = maps.get(level);
  if (!map) return;
  if (!levelRooms().includes(level)) camera.pinned = null;
  mapLevel = level;
  mapWidth = map.width;
  mapHeight = map.height;
  canvas.dataset.map = level;
  canvas.dataset.mapWidth = mapWidth;
  canvas.dataset.mapHeight = mapHeight;
  if (!preserveSelection) {
    selectedCell = null;
    clearRouteSelection();
  }
  hoverCell = null;
  const owner = catalog.levels.find((l) => l.rooms.includes(level));
  focusedLevel = owner?.id ?? 0;
  $("map-label").textContent = map.label.toUpperCase();
  renderMapRows(owner);
  updateStats();
  prefetchNextMap(level);
}
function browseRoom(level) {
  stopControl();
  music.stop();
  playing = false;
  $("play").textContent = "▶ Replay";
  setRoom(level);
  const id = bestStates.get(level);
  if (id !== undefined) {
    watchRequested = false;
    requestStates([id]);
  }
}
function replaceDropped(replacements) {
  for (const [dropped, next] of replacements) {
    if (latestBest === dropped) latestBest = next;
    for (const states of [bestStates, arrivals, wins])
      for (const [level, id] of states)
        if (id === dropped) next === null ? states.delete(level) : states.set(level, next);
  }
}
function levelProgress(observation) {
  return game.levelsCleared ? { ...observation, cleared: game.levelsCleared(observation) } : observation;
}
function levelLabel(id) {
  return catalog.levels.find((level) => level.id === id)?.label ?? `Level ${id + 1}`;
}
function completionWitness() {
  if (wins.has(focusedLevel))
    return {
      id: wins.get(focusedLevel),
      label: `Watch ${levelLabel(focusedLevel)} finish`,
    };
  return null;
}
$("completion").onclick = () => {
  music.unlock().catch(audioError);
  const witness = completionWitness();
  if (!witness) return;
  userSelected = true;
  openInspector();
  selectedCell = null;
  clearRouteSelection();
  watchRequested = true;
  requestStates([witness.id]);
};
function setText(id, text) {
  const node = $(id);
  if (node.textContent !== text) node.textContent = text;
}
function setTitle(id, text) {
  const node = $(id);
  if (node.title !== text) node.title = text;
}
function shown(key) {
  return liveView && !paused ? counters[key].read(performance.now()) : stats[key];
}
function updateCounters() {
  const others = [...searchViews].filter(([id]) => id !== activeSearch).map(([, view]) => view.stats || {});
  const total = (key) => others.reduce((sum, other) => sum + (other[key] || 0), shown(key) || 0);
  setText("hero-timelines", fmt(total("executions")));
  setText("attempts", fmt(shown("executions")));
  setText("states", fmt(shown("states")));
  setText("work", fmt(shown("frames")));
}
function updateStats() {
  updateCounters();
  const anyVisited = !!$("room-tabs").querySelector(".visited");
  for (const tab of $("room-tabs").children) {
    const seen = seenMaps.has(Number(tab.dataset.map));
    if (seen && anyVisited && !tab.classList.contains("visited")) {
      tab.classList.add("fresh");
      tab.addEventListener("animationend", () => tab.classList.remove("fresh"), { once: true });
      tab.addEventListener("click", () => tab.classList.remove("fresh"), { once: true });
    }
    tab.classList.toggle("visited", seen);
  }
  setText("cells", fmt(heat.cells.size));
  renderLevelStrip();
  const witness = completionWitness();
  $("completion").hidden = !witness;
  if (witness) setText("completion", witness.label);
  setTitle("states", `Shared snapshot memory: ${fmt(stats.snapshot_bytes / 1048576)} MiB; ${budget.snapshotsMiB} MiB limit. Search memory: ${fmt(stats.wasm_bytes / 1048576)} MiB.`);
  setText("memory", `${((stats.snapshot_bytes || 0) / 1000000).toFixed(1)} MB`);
  setTitle("memory", "Retained snapshots across all search branches");
  setText("workers", fmt(stats.workers ?? budget.workers));
  setTitle("workers", "Web Workers running game rollouts in parallel");
}
function liveLevel() {
  return path.level ?? bootLevel;
}
let levelsExpanded = false;
function renderLevelStrip() {
  const live = liveLevel(),
    after = path.levels.findIndex((id) => id > focusedLevel),
    ids = path.levels.includes(focusedLevel) ? path.levels : path.levels.toSpliced(after < 0 ? path.levels.length : after, 0, focusedLevel),
    levels = ids.map((id) => catalog.levels.find((level) => level.id === id)).filter(Boolean),
    recent = new Set(levels.slice(-2).map((level) => level.id).concat(focusedLevel)),
    hidden = levels.filter((level) => !recent.has(level.id)),
    collapse = hidden.length > 1,
    shown = collapse && !levelsExpanded ? levels.filter((level) => recent.has(level.id)) : levels,
    locked = !ready || branchBusy || controlMode,
    key = `${levels.map((level) => `${level.id}${cleared.has(level.id) ? "c" : ""}`)}|${focusedLevel}|${live}|${gameWon}|${locked}|${collapse && levelsExpanded}`;
  const strip = $("level-strip"),
    trail = $("level-trail");
  if (strip.dataset.key === key) return;
  const expanding = collapse && levelsExpanded && !strip.dataset.key.endsWith("|true");
  strip.dataset.key = key;
  const buttons = shown.map((level) => {
    const button = document.createElement("button");
    button.dataset.level = level.id;
    button.disabled = locked;
    button.setAttribute("aria-pressed", level.id === focusedLevel);
    button.textContent = level.label;
    if (cleared.has(level.id)) {
      const done = document.createElement("span");
      done.className = "level-cleared";
      done.textContent = " ✓";
      button.append(done);
    }
    if (level.id === live && !gameWon) {
      const dot = document.createElement("i");
      dot.className = "level-live";
      button.prepend(dot);
      button.title = "The search is exploring this level";
    }
    button.onclick = () => showLevel(level, level.id === live);
    return button;
  });
  const more = $("level-more");
  more.hidden = !collapse;
  if (collapse) {
    more.setAttribute("aria-expanded", levelsExpanded);
    more.setAttribute("aria-label", levelsExpanded ? "Hide earlier levels" : `Show ${hidden.length} earlier levels`);
    more.title = levelsExpanded ? "Hide earlier levels" : hidden.map((level) => level.label).join(", ");
    more.innerHTML = levelsExpanded ? "Hide<span> earlier</span>" : `+${hidden.length}<span> earlier</span>`;
  }
  trail.replaceChildren(...buttons);
  if (expanding) trail.scrollLeft = 0;
  else revealLevel(trail.querySelector('[aria-pressed="true"]'));
  fadeTrail();
}
function revealLevel(button) {
  const trail = $("level-trail");
  if (!button) return;
  const pad = 24,
    start = button.offsetLeft - pad,
    end = button.offsetLeft + button.offsetWidth + pad - trail.clientWidth;
  if (trail.scrollLeft > start) trail.scrollLeft = start;
  else if (trail.scrollLeft < end) trail.scrollLeft = end;
}
function fadeTrail() {
  const trail = $("level-trail"),
    max = trail.scrollWidth - trail.clientWidth;
  trail.dataset.fade = `${trail.scrollLeft > 1 ? "start" : ""}${trail.scrollLeft < max - 1 ? "end" : ""}`;
}
$("level-trail").addEventListener("scroll", fadeTrail, { passive: true });
new ResizeObserver(() => {
  revealLevel($("level-trail").querySelector('[aria-pressed="true"]'));
  fadeTrail();
}).observe($("level-trail"));
$("level-more").onclick = () => {
  levelsExpanded = !levelsExpanded;
  renderLevelStrip();
};
function showLevel(level, live) {
  userSelected = true;
  if (!live && cleared.has(level.id) && liveVisualization === null) {
    liveVisualization = visualization;
    setVisualization("heat");
  } else if (live && liveVisualization !== null) {
    setVisualization(liveVisualization);
    liveVisualization = null;
  }
  const active = live ? level.rooms.findLast((id) => swarmRooms.get(id)?.length) : undefined;
  const room = active ?? level.rooms.findLast((id) => seenMaps.has(id)) ?? level.rooms[0];
  if (!level.rooms.includes(mapLevel)) browseRoom(room);
  followSearch();
  camera.pinned = live ? null : room;
}
function renderMapRows(owner) {
  const focusedMap = document.activeElement?.closest(".map-row")?.dataset.map;
  const rooms = owner?.rooms || [mapLevel];
  $("room-tabs").replaceChildren(...rooms.map((id) => {
    const tab = document.createElement("button");
    tab.dataset.map = id;
    const label = maps.get(id).label, same = rooms.filter((room) => maps.get(room).label === label);
    tab.textContent = same.length > 1 ? `${label} ${same.indexOf(id) + 1}` : label;
    tab.setAttribute("aria-pressed", id === mapLevel);
    tab.classList.toggle("visited", seenMaps.has(id));
    tab.onclick = () => {
      userSelected = true;
      if (id !== mapLevel) browseRoom(id);
      followSearch();
      camera.pinned = id;
    };
    return tab;
  }));
  document.querySelector(".map-wrap").dataset.rooms = rooms.length;
  $("map-rows").replaceChildren(
    ...[mapLevel].map((id) => {
      const row = document.createElement("section");
      row.className = "map-row";
      row.dataset.map = id;
      row.setAttribute("aria-label", maps.get(id).label);
      const parts = document.createElement("div");
      parts.className = "room-parts";
      const map = maps.get(id), panels = roomPanels(map.width, map.height);
      row.dataset.current = id === mapLevel;
      row.dataset.parts = panels.length;
      panels.forEach((panel, index) => {
        const part = document.createElement("div");
        part.className = "room-part";
        const c = id === mapLevel && index === 0 ? canvas : document.createElement("canvas");
        c.className = "area-map";
        const viewports = game.vertical ? TALL_CAMERA_VIEWPORT : game.grid ? GRID_CAMERA_VIEWPORT : CAMERA_VIEWPORT;
        const viewport = phoneLayout.matches ? viewports.phone : viewports.desktop;
        c.width = viewport.width;
        c.height = viewport.height;
        c.dataset.viewport = true;
        Object.assign(c.dataset, { map: id, panel: index, panelX: panel.x, panelY: panel.y,
          mapWidth: panel.width, mapHeight: panel.height, roomWidth: map.width, roomHeight: map.height });
        c.style.touchAction = game.vertical || game.grid ? "none" : "pan-y";
        c.tabIndex = 0;
        if (c !== canvas) bindMap(c);
        part.append(c, panEdge(c, -1), panEdge(c, 1));
        if (!game.vertical) part.append(panEdge(c, -1, "y"), panEdge(c, 1, "y"));
        if (game.grid) {
          const pair = document.createElement("div");
          pair.className = "room-pair";
          pair.append(part, levelOverview(id));
          parts.append(pair);
        } else if (game.vertical) {
          part.append(levelOverview(id));
          parts.append(part);
        } else parts.append(part, levelOverview(id));
      });
      row.append(parts);
      return row;
    }),
  );
  if (focusedMap !== undefined) {
    const target =
      document.querySelector(`.map-row[data-map="${focusedMap}"] .area-map`) ||
      canvas;
    target.focus({ preventScroll: true });
  }
}
let panHold = null, panLast = 0;
function holdingPan(c, dir, axis) {
  return panHold?.canvas === c && panHold.dir === dir && panHold.axis === axis;
}
function holdPan(c, dir, axis) {
  if (!holdingPan(c, dir, axis)) panHold = { canvas: c, dir, axis, since: performance.now() };
  panHold.until = 0;
}
function releasePan(c, dir, axis) {
  if (!holdingPan(c, dir, axis)) return;
  if (performance.now() - panHold.since < 300) panHold.until = performance.now() + 450;
  else panHold = null;
}
function panRoom(c) {
  const id = Number(c.dataset.map), map = canvasPanel(c), view = roomView(id, c);
  const scale = Math.min(c.width / map.width, c.height / map.height) * view.zoom, half = c.width / 2 / scale, halfY = c.height / 2 / scale;
  return {
    left: view.x - half - map.x > 1,
    right: map.x + map.width - view.x - half > 1,
    up: view.y - halfY - map.y > 1,
    down: map.y + map.height - view.y - halfY > 1,
  };
}
function panEdge(c, dir, axis = "x") {
  const edge = document.createElement("button");
  edge.className = "pan-edge";
  edge.dataset.dir = dir;
  if (axis === "y") edge.dataset.axis = axis;
  edge.tabIndex = -1;
  edge.setAttribute("aria-label", axis === "y" ? (dir < 0 ? "Look up" : "Look down") : dir < 0 ? "Look left" : "Look right");
  const path = axis === "y" ? (dir < 0 ? "M5.5 14.5 12 8l6.5 6.5" : "M5.5 9.5 12 16l6.5-6.5") : dir < 0 ? "M14.5 5.5 8 12l6.5 6.5" : "M9.5 5.5 16 12l-6.5 6.5";
  edge.innerHTML = `<svg viewBox="0 0 24 24" aria-hidden="true"><path d="${path}"/></svg>`;
  const mine = () => holdingPan(c, dir, axis);
  const start = () => holdPan(c, dir, axis);
  const end = () => releasePan(c, dir, axis);
  edge.addEventListener("pointerleave", (e) => { if (e.pointerType === "mouse" && mine()) panHold = null; });
  edge.addEventListener("pointerdown", (e) => { e.preventDefault(); if (e.pointerType !== "mouse") start(); });
  edge.addEventListener("pointerup", (e) => { if (e.pointerType !== "mouse") end(); });
  edge.addEventListener("pointercancel", () => { if (panHold?.canvas === c) panHold = null; });
  const forward = (type, e) => c.dispatchEvent(new PointerEvent(type, { clientX: e.clientX, clientY: e.clientY, pointerType: "mouse", bubbles: true }));
  edge.addEventListener("pointermove", (e) => {
    if (e.pointerType !== "mouse") return;
    if (!mine() && (e.movementX || e.movementY)) start();
    forward("pointermove", e);
  });
  edge.addEventListener("click", (e) => {
    if (e.pointerType === "mouse") return forward("click", e);
    if (e.pointerType) return;
    start();
    panHold.since -= 300;
    panHold.until = performance.now() + 450;
  });
  return edge;
}
function updatePan(now) {
  const dt = Math.min(0.05, panLast ? (now - panLast) / 1000 : 0);
  panLast = now;
  if (!panHold) return;
  const c = panHold.canvas;
  if (!c.isConnected || (panHold.until && now > panHold.until)) return (panHold = null);
  const id = Number(c.dataset.map), map = canvasPanel(c), view = roomView(id, c);
  const scale = Math.min(c.width / map.width, c.height / map.height) * view.zoom;
  const ramp = ((now - panHold.since) / 1000 - 0.12) / 0.45;
  if (ramp <= 0) return;
  if (!panHold.moved) {
    panHold.moved = true;
    userSelected = true;
    moveCameraByHand();
  }
  if (camera.follow) camera.manual = now + 10000;
  const step = (panHold.axis === "y" ? c.height : c.width) / scale * 0.85 * ease(Math.min(1, ramp)) * panHold.dir * dt;
  Object.assign(view, panelCenter(map, view.zoom, panHold.axis === "y" ? { x: view.x, y: view.y + step } : { x: view.x + step, y: view.y }, c));
}
function updateRoomChrome(c) {
  const part = c.parentElement;
  if (!part) return;
  const pan = panRoom(c), above = !game.vertical && pan.up, below = !game.vertical && pan.down;
  const key = `${pan.left},${pan.right},${above},${below}`;
  if (part.dataset.chrome === key) return;
  part.dataset.chrome = key;
  c.dataset.tall = pan.up || pan.down;
  part.dataset.panLeft = pan.left;
  part.dataset.panRight = pan.right;
  part.dataset.panUp = above;
  part.dataset.panDown = below;
}
let spriteBudget = swarm.maxTrails, drawCost = 0, frameGap = 1000 / 60;
function drawMap(now) {
  const start = performance.now();
  swarmRooms = swarm.frame(activeSearch, heat.clock(now), reducedMotion.matches, Math.round(spriteBudget), true);
  updatePan(now);
  followStalledPath(now);
  updateCamera(now);
  for (const c of document.querySelectorAll(".area-map")) {
    drawArea(c, now);
    updateRoomChrome(c);
  }
  for (const strip of document.querySelectorAll(".level-overview")) drawOverview(strip, now);
  if (recording.state === "recording") {
    try { paintRecording(); } catch (error) { recording.fail(error); }
  }
  drawCost = drawCost * 0.9 + (performance.now() - start) * 0.1;
  const behind = drawCost > 6 && frameGap > 20, ahead = drawCost < 4 || frameGap < 18;
  spriteBudget = Math.min(swarm.maxTrails, Math.max(256, spriteBudget * (behind ? 0.9 : ahead ? 1.02 : 1)));
}
function drawArea(canvas, now) {
  const preview = branchPreview && searchViews.get(branchPreview.id);
  const areaHeat = preview?.heat || heat;
  const areaOrigin = preview ? preview.branchOrigin : branchOrigin;
  const mode = visualization;
  canvas.dataset.previewSearch = preview ? branchPreview.id : "";
  const ctx = canvas.getContext("2d"),
    mapLevel = Number(canvas.dataset.map),
    map = maps.get(mapLevel),
    mapWidth = map.width,
    mapHeight = map.height,
    view = roomView(mapLevel, canvas),
    areaZoom = view.zoom,
    areaCenter = view.x,
    areaCenterY = view.y;
  ctx.imageSmoothingEnabled = false;
  ctx.fillStyle = "#122b42";
  ctx.fillRect(0, 0, canvas.width, canvas.height);
  ctx.save();
  const scale =
    Math.min(canvas.width / Number(canvas.dataset.mapWidth), canvas.height / Number(canvas.dataset.mapHeight)) * areaZoom;
  ctx.translate(Math.round(canvas.width / 2 - areaCenter * scale), Math.round(canvas.height / 2 - areaCenterY * scale));
  ctx.scale(scale, scale);
  const image = panorama(mapLevel);
  if (image?.tiles) {
    const left = areaCenter - canvas.width / 2 / scale, right = areaCenter + canvas.width / 2 / scale;
    const top = areaCenterY - canvas.height / 2 / scale, bottom = areaCenterY + canvas.height / 2 / scale;
    for (const t of image.tiles)
      if (t.x < right && t.x + t.width > left && t.y < bottom && t.y + t.height > top) ctx.drawImage(t.bitmap, t.x, t.y);
  } else if (image) ctx.drawImage(image, 0, 0);
  canvas.dataset.previewRoute = !preview && routeHover?.segments ? routeHover.id : "";
  canvas.dataset.zoom = areaZoom;
  canvas.dataset.centerX = areaCenter;
  canvas.dataset.centerY = areaCenterY;
  canvas.dataset.overlay = mode;
  const label = `${map.label} ${mode === "movement" ? "player movement" : mode === "both" ? "heatmap and player movement" : "heatmap"}. Click a cell to watch its history. Drag to move when zoomed. Arrow keys pan; Enter inspects the cell under the pointer or at the center.`;
  if (canvas.getAttribute("aria-label") !== label)
    canvas.setAttribute("aria-label", label);
  canvas.dataset.swarmCount = 0;
  if (mode === "movement") {
    drawSwarmBackground(ctx, mapWidth, mapHeight);
  } else {
    ctx.fillStyle = "rgba(7,24,43,.42)";
    ctx.fillRect(0, 0, mapWidth, mapHeight);
    {
      ctx.fillStyle = "rgba(65,113,162,.11)";
      ctx.fillRect(0, 0, mapWidth, mapHeight);
      for (const cell of areaHeat.cells.values()) {
        if (cell.level !== mapLevel) continue;
        const color = areaHeat.color(cell, now);
        if (!color) continue;
        const x = cell.x * 32,
          y = cellTop(cell.y);
        ctx.fillStyle = `rgba(${color},.56)`;
        ctx.fillRect(x + 1, y + 1, 30, 30);
        ctx.strokeStyle = `rgba(${color},.8)`;
        ctx.lineWidth = 0.7 / areaZoom;
        ctx.strokeRect(x + 1, y + 1, 30, 30);
      }
    }
    ctx.strokeStyle = "rgba(166,205,241,.07)";
    ctx.lineWidth = 0.5 / areaZoom;
    for (let x = 0; x <= mapWidth; x += 32) {
      ctx.beginPath();
      ctx.moveTo(x, 0);
      ctx.lineTo(x, mapHeight);
      ctx.stroke();
    }
    for (let y = 8; y < mapHeight; y += 32) {
      ctx.beginPath();
      ctx.moveTo(0, y);
      ctx.lineTo(mapWidth, y);
      ctx.stroke();
    }
  }
  if (mode !== "heat")
    canvas.dataset.swarmCount = drawPlayers(ctx, mapLevel, scale * (canvas.clientWidth / canvas.width), preview ? preview.movement ?? null : swarmRooms, canvasPanel(canvas), {
      left: areaCenter - canvas.width / 2 / scale,
      right: areaCenter + canvas.width / 2 / scale,
      top: areaCenterY - canvas.height / 2 / scale,
      bottom: areaCenterY + canvas.height / 2 / scale,
    });
  canvas.dataset.tracePoints = drawTrail(ctx, mapLevel, scale, preview ? areaOrigin?.segments || [] : undefined);
  canvas.dataset.zoom = areaZoom;
  canvas.dataset.centerX = areaCenter;
  canvas.dataset.centerY = areaCenterY;
  const focus = preview ? null : hoverCell || selectedCell;
  if (focus?.level === mapLevel) {
    ctx.strokeStyle = "#fff0bd";
    ctx.lineWidth = 1.5 / areaZoom;
    ctx.strokeRect(focus.x * 32 + 1, cellTop(focus.y) + 1, 30, 30);
  }
  const o = preview ? areaOrigin?.point : routeHover ? routeHover.point : $("inspector").hidden
    ? areaOrigin?.point
    : current
      ? markerPoint()
      : null;
  const mapOrigin =
    areaOrigin?.point?.level === mapLevel && panelContains(canvasPanel(canvas), { x: areaOrigin.point.x, y: areaOrigin.point.y - 8 }) ? areaOrigin : null;
  canvas.dataset.originFrame = mapOrigin?.frame ?? "";
  canvas.dataset.originPulse = String(
    !preview && !!mapOrigin && drawOriginPulse(ctx, mapLevel, scale, now),
  );
  canvas.dataset.markerFrame =
    o?.level !== mapLevel || !panelContains(canvasPanel(canvas), { x: o.x, y: o.y - 8 })
      ? ""
      : routeHover && !preview ? routeHover.frames : preview || $("inspector").hidden
        ? areaOrigin.frame
        : seeking
          ? requestedFrame
          : currentFrame;
  if (o?.level === mapLevel) {
    ctx.strokeStyle = "#fff5cc";
    ctx.lineWidth = 1.5 / areaZoom;
    ctx.beginPath();
    ctx.arc(o.x, o.y - 8, 9, 0, Math.PI * 2);
    ctx.stroke();
    ctx.fillStyle = "#fff5cc";
    ctx.fillRect(o.x - 2, o.y - 10, 4, 4);
  }
  ctx.restore();
}
function mapCoordinates(e) {
  const c = e.currentTarget,
    id = Number(c.dataset.map),
    rect = c.getBoundingClientRect(),
    px = ((e.clientX - rect.left) / rect.width) * c.width,
    py = ((e.clientY - rect.top) / rect.height) * c.height,
    view = roomView(id, c),
    scale = Math.min(c.width / Number(c.dataset.mapWidth), c.height / Number(c.dataset.mapHeight)) * view.zoom;
  return {
    x: Math.floor(((px - c.width / 2) / scale + view.x) / 32),
    y: Math.floor(((py - c.height / 2) / scale + view.y - 8) / 32),
    level: id,
  };
}
function bindMap(c) {
  let pinch;
  c.addEventListener("wheel", (e) => {
    const r = c.getBoundingClientRect(), size = game.vertical ? r.height : r.width;
    if (game.grid) {
      const unit = wheelUnit(e, r.height);
      if (scrollView(Number(c.dataset.map), c, (e.deltaX * unit * c.width) / r.width, (e.deltaY * unit * c.height) / r.height)) e.preventDefault();
      return;
    }
    const delta = wheelDelta(e, size);
    if (delta && scrollAlong(Number(c.dataset.map), c, (delta * (game.vertical ? c.height : c.width)) / size)) e.preventDefault();
  }, { passive: false });
  c.addEventListener("pointermove", (e) => {
    if (e.pointerType === "mouse" && (e.movementX || e.movementY)) camera.held = performance.now() + 3000;
  });
  c.addEventListener("pointerleave", (e) => {
    if (e.pointerType === "mouse") camera.held = Math.min(camera.held, performance.now() + 600);
  });
  c.addEventListener("pointerdown", (e) => {
    if (e.pointerType !== "mouse") camera.held = performance.now() + 2500;
  });
  const pair = (touches) => ({
    x: (touches[0].clientX + touches[1].clientX) / 2,
    y: (touches[0].clientY + touches[1].clientY) / 2,
    distance: Math.hypot(touches[0].clientX - touches[1].clientX, touches[0].clientY - touches[1].clientY),
  });
  c.addEventListener("touchstart", (event) => {
    if (event.touches.length !== 2) return;
    event.preventDefault();
    const room = roomView(Number(c.dataset.map), c), map = canvasPanel(c);
    const point = pair(event.touches), r = c.getBoundingClientRect();
    const scale = Math.min(c.width / map.width, c.height / map.height) * room.zoom;
    pinch = { ...point, zoom: room.zoom,
      worldX: room.x + (point.x - r.left - r.width / 2) * c.width / r.width / scale,
      worldY: room.y + (point.y - r.top - r.height / 2) * c.height / r.height / scale };
    dragged = true;
    pointerStart = null;
    hoverCell = null;
    $("hover").hidden = true;
    moveCameraByHand();
  }, { passive: false });
  c.addEventListener("touchmove", (event) => {
    if (!pinch || event.touches.length !== 2) return;
    event.preventDefault();
    const id = Number(c.dataset.map), room = roomView(id, c), map = canvasPanel(c);
    const point = pair(event.touches), r = c.getBoundingClientRect();
    room.zoom = Math.max(1, Math.min(maxZoom(c), pinch.zoom * point.distance / Math.max(1, pinch.distance)));
    const scale = Math.min(c.width / map.width, c.height / map.height) * room.zoom;
    const center = panelCenter(map, room.zoom, {
      x: pinch.worldX - (point.x - r.left - r.width / 2) * c.width / r.width / scale,
      y: pinch.worldY - (point.y - r.top - r.height / 2) * c.height / r.height / scale,
    }, c);
    room.x = center.x; room.y = center.y;
    userSelected = dragged = true;
    if (camera.follow) camera.manual = performance.now() + 10000;
    roomView(id).zoom = room.zoom;
    drawMap(performance.now());
  }, { passive: false });
  c.addEventListener("touchend", () => {
    if (pinch) drawMap(performance.now());
    pinch = null; pointerStart = null;
  });
  c.addEventListener("touchcancel", () => { pinch = null; pointerStart = null; });
  c.addEventListener("pointerdown", (e) => {
    const view = roomView(Number(c.dataset.map), c);
    pointerStart = {
      x: e.clientX,
      y: e.clientY,
      center: view.x,
      centerY: view.y,
      canvas: c,
    };
    dragged = false;
    if (view.zoom > 1) c.setPointerCapture(e.pointerId);
  });
  for (const event of ["pointerup", "pointercancel"])
    c.addEventListener(event, () => {
      pointerStart = null;
    });
  c.addEventListener("pointermove", (e) => {
    const id = Number(c.dataset.map),
      map = canvasPanel(c),
      room = roomView(id, c);
    if (!pinch && pointerStart?.canvas === c && e.buttons && room.zoom > 1) {
      const dx = e.clientX - pointerStart.x,
        dy = e.pointerType === "touch" && c.dataset.tall !== "true" ? 0 : e.clientY - pointerStart.y;
      if (Math.hypot(dx, dy) > 4 && !dragged) {
        dragged = true;
        moveCameraByHand();
      }
      if (dragged) {
        userSelected = true;
        if (camera.follow) camera.manual = performance.now() + 10000;
        const rect = c.getBoundingClientRect(),
          scale =
            Math.min(c.width / map.width, c.height / map.height) * room.zoom;
        const view = panelCenter(
          map,
          room.zoom,
          {
            x: pointerStart.center - (dx * c.width) / rect.width / scale,
            y: pointerStart.centerY - (dy * c.height) / rect.height / scale,
          },
          { width: c.width, height: c.height },
        );
        room.x = view.x;
        room.y = view.y;
        return;
      }
    }
    if (pinch || e.pointerType === "touch") return;
    const point = mapCoordinates(e),
      cell = heat.cells.get(`${point.level}:${point.x}:${point.y}`);
    hoverCell = cell?.ids.length ? point : null;
    c.style.cursor = hoverCell ? "pointer" : "default";
    $("hover").hidden = !cell?.visits && !cell?.ids.length;
    $("hover").textContent =
      `${cell?.visits || 0} visits · ${cell?.ids.length || 0} states`;
  });
  c.addEventListener("pointerleave", (e) => {
    if (c.hasPointerCapture(e.pointerId)) return;
    pointerStart = null;
    hoverCell = null;
    c.style.cursor = "default";
    $("hover").hidden = true;
  });
  c.addEventListener("click", (e) => {
    if (branchBusy) return;
    if (dragged) {
      dragged = false;
      return;
    }
    const point = mapCoordinates(e),
      cell = heat.cells.get(`${point.level}:${point.x}:${point.y}`);
    if (point.level !== mapLevel) setRoom(point.level);
    if (tourSession) { tourSession.interacted = true; if (cell?.ids.length) tourSession.cell = cell; }
    inspect(cell || { ...point, visits: 0, ids: [] });
  });
  c.addEventListener("keydown", (e) => {
    const level = Number(c.dataset.map),
      view = roomView(level, c);
    const point = (hoverCell?.level === level ? hoverCell : null) ||
      (selectedCell?.level === level ? selectedCell : null) || {
        level,
        x: Math.floor(view.x / 32),
        y: Math.floor((view.y - 8) / 32),
      };
    if (e.key === "Enter") {
      e.preventDefault();
      if (level !== mapLevel) setRoom(level);
      hoverCell = point;
      const cell = heat.cells.get(`${level}:${point.x}:${point.y}`);
      if (tourSession) {
        tourSession.interacted = true;
        if (cell?.ids.length) tourSession.cell = cell;
      }
      inspect(
        cell || {
          ...point,
          visits: 0,
          ids: [],
        },
      );
    }
  });
}
bindMap(canvas);
function setSearchPaused(value) {
  paused = value;
  worker.postMessage({ type: value ? "pause" : "resume" });
  updateSearchControl();
  $("status").textContent = value ? "Paused" : "Exploring";
  $("status-dot").className = value ? "paused" : "";
}
function holdSearchForReplay() {
  const watching = playing && viewerReplay && !controlMode;
  if (watching === replayWatching) return;
  replayWatching = watching;
  if (watching && !paused && ready && !stats.stopped) {
    replayPaused = true;
    setSearchPaused(true);
  } else if (!watching && replayPaused) {
    replayPaused = false;
    if (paused && ready && !stats.stopped && !playSession && !branchBusy && !tourSession) setSearchPaused(false);
  }
}
$("pause").onclick = () => {
  replayPaused = false;
  setSearchPaused(!paused);
};
$("reset").onclick = () => {
  if (tourSession) tourSession.interacted = true;
  seed++;
  startSearch();
};
function audioError(e) {
  $("branch-message").hidden = false;
  $("branch-message").textContent = "Game audio unavailable: " + e.message;
}
function startAudio() {
  music.start(() => engine.capture()).catch(audioError);
}
$("play").onclick = async () => {
  stopControl();
  if (seeking) return;
  userSelected = true;
  if (playing) {
    playing = false;
    music.stop();
    $("play").textContent = "▶ Replay";
    return;
  }
  music.unlock().catch(audioError);
  if (currentFrame >= current.frames) {
    const state = current,
      reset = seek(isMapEvidence(state.observation, catalog.levels) ? levelEntryFrame(trace, state.observation.level) : 0),
      epoch = replayEpoch;
    if (!(await reset) || current !== state || epoch !== replayEpoch) return;
  }
  playing = true;
  viewerReplay = true;
  frameCredit = 0;
  startAudio();
  $("play").textContent = "Ⅱ Pause";
};
$("speed").onchange = () => {
  if (playing) startAudio();
};
$("scrub").oninput = () => {
  userSelected = true;
  seek(Number($("scrub").value));
};
const showInfo = (html) => {
  $("info-content").innerHTML = html;
  $("info").showModal();
};
$("close-info").onclick = () => $("info").close();
$("info").onclick = (e) => {
  if (e.target === $("info")) $("info").close();
};
$("credits").onclick = async () =>
  showInfo(typeof game.credits === "function" ? await game.credits(base) : game.credits);
function hideHistoryRail() {
  $("history-rail").hidden = true;
  $("workspace").classList.remove("history-collapsed");
}
function openInspector() {
  hideHistoryRail();
  if ($("inspector").hidden) {
    inspectorReturnFocus = document.activeElement;
    revealReplayRoom = true;
  }
  $("inspector").hidden = false;
  $("workspace").classList.add("inspect-open");
}
function closeInspector({ restoreFocus = true } = {}) {
  hideHistoryRail();
  hideRoutePreview();
  stopControl();
  music.stop();
  playing = false;
  $("play").textContent = "▶ Replay";
  $("inspector").hidden = true;
  $("workspace").classList.remove("inspect-open");
  if (playSession) {
    if (tourSession) tourSession.interacted = true;
    const resume = playSession.branch === activeSearch && !branchBusy && ready && !stats.stopped;
    playSession = null;
    current = selectedCell = null;
    clearRouteSelection();
    trace = segments = [];
    historyVerified = false;
    ++replayEpoch;
    seeking = false;
    invalidateStateRequest();
    if (resume) {
      paused = false;
      worker.postMessage({ type: "resume" });
      updateSearchControl();
    }
    updateBranchControls();
  }
  if (restoreFocus && inspectorReturnFocus?.isConnected)
    inspectorReturnFocus.focus({ preventScroll: true });
}
compactReplay.addEventListener("change", () => {
  updateBranchControls();
  if (tour.open && tour.ready) tour.setInteraction(tour.steps[tour.index].interactive?.() || []);
});
function updateBranchControls() {
  const soundParent = document.querySelector(!controlMode ? ".replay-actions" : ".history-controls");
  if ($("sound").parentElement !== soundParent) {
    if (soundParent.classList.contains("history-controls")) soundParent.prepend($("sound"));
    else soundParent.append($("sound"));
  }
  $("new-search").disabled = !ready || !engine || branchBusy || controlMode;
  renderLevelStrip();
  if (missingRom && !askedForRom && !$("new-search").disabled) {
    askedForRom = true;
    $("new-search").click();
  }
  $("tour-open").disabled =
    !ready || !engine || controlMode || branchBusy || seeking;
  const usable =
    !!current &&
    !!engine &&
    historyVerified &&
    !seeking &&
    !branchBusy &&
    !!frameObservation?.health;
  $("take-control").disabled = controlMode ? false : !usable;
  const draft = !!playSession && !!current?.branch?.manual;
  $("take-control").hidden = controlMode;
  $("search-here").hidden = $("discard-branch").hidden = !draft;
  $("search-here").disabled = !usable || !draft;
  $("discard-branch").disabled = branchBusy;
  $("inspector").classList.toggle("draft", draft);
  $("take-control").textContent = controlMode
    ? "🎮 Stop playing"
    : "🎮 Play from here";
  $("search-here").textContent = "↗ Branch search from here";
  $("take-control").setAttribute("aria-pressed", controlMode);
  $("game-controls").hidden = !controlMode;
  $("scrub").disabled = !current || controlMode || branchBusy;
  $("play").disabled = !current || controlMode || branchBusy || seeking;
  for (const button of $("branch-tree").querySelectorAll("button[data-search],button[data-resume-search],button[data-delete-search]"))
    button.disabled = branchBusy || controlMode;
  $("sound").disabled = !current;
  $("pause").disabled = !ready || branchBusy || controlMode || !!stats.stopped;
  $("inspector").classList.toggle("controlling", controlMode);
  for (const button of $("state-list").children)
    button.disabled = controlMode || branchBusy;
}
function stopControl() {
  if (!controlMode) return;
  controlMode = false;
  music.stop();
  controller.clear();
  for (const button of document.querySelectorAll(".touch-controls .held"))
    button.classList.remove("held");
  if (current && engine) {
    current.frames = currentFrame;
    current.observation = engine.observation();
    current.snapshot = engine.capture();
    if (current.branch?.manual) current.branch.manual.to = currentFrame;
    if (currentFrame > traceMaxFrame) recordTrail();
    segments = trailSegments(trace);
    timeline.index(current.actions, current.frames);
    timeline.trim(currentFrame);
    stateCache.set(current.id, current);
    renderStates();
    drawFilm();
  }
  updateBranchControls();
}
$("close-inspector").onclick = () => {
  const retained = !!current && !playSession, draft = !!playSession;
  closeInspector();
  applyCameraMode();
  if (draft) leaveTourGameplay();
  if (retained) {
    $("history-rail").hidden = false;
    $("workspace").classList.add("history-collapsed");
    $("history-reopen").focus({ preventScroll: true });
  }
};
$("history-reopen").onclick = () => {
  openInspector();
  $("close-inspector").focus({ preventScroll: true });
};
function expandInspector(expanded) {
  $("inspector").classList.toggle("expanded", expanded);
  $("expand-inspector").setAttribute("aria-expanded", expanded);
  $("expand-inspector").textContent = expanded ? "Collapse" : "Expand";
  if (!expanded) {
    revealReplayRoom = true;
    followReplayRoom(markerPoint());
  }
}
$("expand-inspector").onclick = () =>
  expandInspector(!$("inspector").classList.contains("expanded"));
$("take-control").onclick = () => {
  if (tour.open && tour.index === STEP.one && !controlMode) {
    tour.go(STEP.you).then(() => {
      if (tour.open && tour.index === STEP.you && tour.ready) $("take-control").click();
    });
    return;
  }
  hideRoutePreview();
  if (tourSession) { tourSession.interacted = true; tourSession.gameplay = true; }
  if (controlMode) {
    stopControl();
    leaveTourGameplay();
    return;
  }
  if (!current || seeking || branchBusy || !frameObservation?.health) return;
  userSelected = true;
  playing = false;
  frameCredit = 0;
  paused = true;
  worker.postMessage({ type: "pause" });
  updateSearchControl();
  const parent = current;
  playSession ||= { branch: activeSearch };
  current = {
    id: `manual-${++manualId}`,
    boot_level: bootLevel,
    actions: prefixAt(parent.actions, currentFrame),
    frames: currentFrame,
    observation: engine.observation(),
    snapshot: engine.capture(),
    branch: {
      parent_state: parent.id,
      parent_frame: currentFrame,
      manual: { from: currentFrame, to: currentFrame },
    },
  };
  trace = trace.filter((p) => p.frame <= currentFrame);
  traceMaxFrame = trace.at(-1)?.frame ?? -1;
  recordTrail();
  segments = trailSegments(trace);
  selectedCell = null;
  clearRouteSelection();
  $("cell-title").textContent = "Your branch";
  $("film-title").textContent = "History";
  $("branch-message").hidden = true;
  controller.clear();
  controlMode = true;
  if (matchMedia("(max-width: 800px)").matches) expandInspector(true);
  timeline.trim(currentFrame);
  startAudio();
  $("play").textContent = "▶ Replay";
  renderStates();
  updateBranchControls();
  $("film").focus({ preventScroll: true });
  document.querySelector(".screen").scrollIntoView({ block: "nearest", behavior: "instant" });
};
function leaveTourGameplay() {
  if (tour.open && tour.index === STEP.you) tour.go(STEP.fork);
}
$("discard-branch").onclick = () => {
  closeInspector();
  leaveTourGameplay();
};
$("search-here").onclick = async () => {
  hideRoutePreview();
  if (tourSession) tourSession.interacted = true;
  if (!playSession || !current?.branch?.manual || seeking || branchBusy || !frameObservation?.health) return;
  stopControl();
  music.stop();
  playing = false;
  paused = true;
  worker.postMessage({ type: "pause" });
  updateSearchControl();
  branchBusy = true;
  invalidateStateRequest();
  const branchFrame = currentFrame,
    observation = engine.observation(),
    point = isMapEvidence(observation, catalog.levels)
      ? {
          ...project(observation, maps.get(observation.level)),
          frame: branchFrame,
        }
      : { gap: true, frame: branchFrame },
    prefix = trace.filter((p) => p.frame < branchFrame);
  prefix.push(point);
  pendingOrigin = {
    point: point.gap ? null : point,
    frame: branchFrame,
    segments: trailSegments(prefix),
  };
  updateBranchControls();
  $("branch-message").hidden = false;
  $("branch-message").textContent = "Starting search from this frame…";
  if (tour.open && (tour.index === STEP.you || tour.index === STEP.fork)) tour.go(STEP.tree);
  const branchWorker = worker,
    epoch = ++replayEpoch;
  const actions = prefixAt(current.actions, currentFrame),
    snapshot = engine.capture(),
    branch = branchInfo(
      current.id.startsWith?.("manual-")
        ? current.branch
        : { parent_state: current.id, parent_frame: currentFrame },
      currentFrame,
    );
  try {
    const endpoint_sha256 = await snapshotHash(snapshot);
    if (worker !== branchWorker || epoch !== replayEpoch) return;
    const tape = {
      format: `harmony-${game.id}-browser-v1`,
      rom_sha256: game.rom.sha256,
      core_revision: engine.revision,
      boot_level: bootLevel,
      actions,
      endpoint_sha256,
      branch,
    };
    if (worker !== branchWorker || epoch !== replayEpoch) return;
    worker.postMessage({ type: "fork", tape, seed: ++seed });
  } catch (e) {
    if (worker !== branchWorker || epoch !== replayEpoch) return;
    branchBusy = false;
    pendingOrigin = null;
    showError(e);
    updateBranchControls();
  }
};
function deleteSearch(id) {
  if (!id || branchBusy || controlMode) return;
  if (tourSession) tourSession.interacted = true;
  clearBranchPreview();
  hideRoutePreview();
  closeInspector();
  branchBusy = true;
  invalidateStateRequest();
  ++replayEpoch;
  current = null;
  clearRouteSelection();
  trace = segments = [];
  historyVerified = false;
  updateBranchControls();
  worker.postMessage({ type: "delete", id, paused });
}
function switchSearch(id) {
  if (branchBusy || controlMode) return;
  if (tourSession) tourSession.interacted = true;
  if (id === activeSearch) {
    closeInspector();
    if (ready && !stats.stopped) {
      paused = false;
      worker.postMessage({ type: "resume" });
      updateSearchControl();
    }
    return;
  }
  clearBranchPreview();
  hideRoutePreview();
  branchBusy = true;
  closeInspector();
  userSelected = true;
  paused = true;
  pendingOrigin = null;
  ++replayEpoch;
  seeking = false;
  current = null;
  clearRouteSelection();
  trace = segments = [];
  historyVerified = false;
  invalidateStateRequest();
  updateSearchControl();
  updateBranchControls();
  if (phoneLayout.matches) followSearch();
  worker.postMessage({
    type: "switch",
    active: id,
  });
}
function updateSoundControl() {
  const muted = music.muted,
    label = muted ? "Unmute game audio" : "Mute game audio";
  $("sound").innerHTML =
    `<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3 9h4l5-4v14l-5-4H3z"/>${muted ? '<path d="m16 9 5 6m0-6-5 6"/>' : '<path d="M16 8a6 6 0 0 1 0 8m3-11a10 10 0 0 1 0 14"/>'}</svg>`;
  $("sound").setAttribute("aria-label", label);
  $("sound").title = label;
  $("sound").setAttribute("aria-pressed", muted);
}
$("sound").onclick = () => {
  music.mute();
  updateSoundControl();
};
updateSoundControl();
if (liveView) $("search-speed").closest(".search-speed").style.display = "none";
$("search-speed").max = paceStops.length - 1;
showPace();
$("search-speed").oninput = () => {
  const stop = paceStops[$("search-speed").value];
  pace = stop === Infinity ? 0 : stop;
  showPace();
  worker?.postMessage({ type: "pace", pace });
};
const ARROW_PANS = { ArrowLeft: [-1, "x", "left"], ArrowRight: [1, "x", "right"], ArrowUp: [-1, "y", "up"], ArrowDown: [1, "y", "down"] };
window.addEventListener("keydown", (e) => {
  const arrow = ARROW_PANS[e.key];
  if (
    arrow &&
    !controlMode &&
    !e.ctrlKey &&
    !e.metaKey &&
    !e.altKey &&
    !e.target.closest("input,select,textarea,[contenteditable=true],[role=slider],#guided-tour")
  ) {
    const c = mapCanvas(branchPreview?.room ?? mapLevel);
    if (c && panRoom(c)[arrow[2]]) {
      e.preventDefault();
      holdPan(c, arrow[0], arrow[1]);
    }
    return;
  }
  if (e.code === "Escape") {
    if (controlMode) stopControl();
    else if (!$("inspector").hidden && !$("info").open) {
      closeInspector();
      applyCameraMode();
    }
    return;
  }
  if (
    !controlMode ||
    e.ctrlKey ||
    e.metaKey ||
    e.altKey ||
    e.target.closest("input,select,[contenteditable=true],#guided-tour")
  )
    return;
  const bit = keyButtons[e.code];
  if (bit) {
    e.preventDefault();
    controller.press(e.code, bit);
  }
});
window.addEventListener("keyup", (e) => {
  const arrow = ARROW_PANS[e.key];
  if (arrow && panHold) releasePan(panHold.canvas, arrow[0], arrow[1]);
  controller.release(e.code);
  if (controlMode && keyButtons[e.code]) e.preventDefault();
});
window.addEventListener("blur", stopControl);
document.addEventListener("visibilitychange", () => {
  resetRetreat(camera);
  if (document.hidden) {
    stopControl();
    music.stop();
    playing = false;
    $("play").textContent = "▶ Replay";
    holdSearchForReplay();
  }
});
for (const button of document.querySelectorAll(".touch-controls button")) {
  button.addEventListener("contextmenu", (e) => e.preventDefault());
  button.addEventListener("pointerdown", (e) => {
    if (!controlMode) return;
    e.preventDefault();
    button.setPointerCapture(e.pointerId);
    controller.press(`touch-${e.pointerId}`, Number(button.dataset.button));
    button.classList.add("held");
  });
  for (const event of ["pointerup", "pointercancel", "lostpointercapture"])
    button.addEventListener(event, (e) => {
      controller.release(`touch-${e.pointerId}`);
      button.classList.remove("held");
    });
}
function drawOriginPulse(ctx, level, scale, now) {
  if (!originPulse?.point || originPulse.point.level !== level) return false;
  const age = (now - originPulse.start) / 2400;
  if (age < 0 || age >= 1) return false;
  const screenScale = scale * ctx.canvas.clientWidth / ctx.canvas.width;
  ctx.save();
  ctx.lineWidth = 3 / screenScale;
  ctx.strokeStyle = "#ffd76a";
  for (const offset of reducedMotion.matches ? [0] : [0, 0.18, 0.36]) {
    const phase = (age - offset) / (1 - offset);
    if (phase < 0) continue;
    ctx.globalAlpha = reducedMotion.matches ? 0.9 : 1 - phase;
    ctx.beginPath();
    ctx.arc(
      originPulse.point.x,
      originPulse.point.y - 8,
      (reducedMotion.matches ? 22 : 10 + 64 * phase) / screenScale,
      0,
      Math.PI * 2,
    );
    ctx.stroke();
  }
  ctx.restore();
  return true;
}
function startTraceReveal(animatePath = true) {
  traceReveal = reducedMotion.matches || !animatePath ? 0 : performance.now();
  const screen = document.querySelector(".screen");
  screen.classList.remove("revealed");
  void screen.offsetWidth;
  screen.classList.add("revealed");
}
function revealedSegments(drawn) {
  const progress = (performance.now() - traceReveal) / 1800;
  if (!traceReveal || progress >= 1) {
    traceReveal = 0;
    return { segments: drawn, head: null };
  }
  let budget = Math.max(2, Math.floor(drawn.reduce((n, segment) => n + segment.length, 0) * ease(progress)));
  const segments = [];
  for (const segment of drawn) {
    if (budget <= 0) break;
    segments.push(segment.slice(0, budget));
    budget -= segment.length;
  }
  return { segments, head: segments.at(-1)?.at(-1) };
}
function drawTrail(ctx, level, scale, override) {
  const selected = override ?? ($("inspector").hidden
    ? branchOrigin?.segments || []
    : routeHover ? routeHover.segments || [] : segments);
  const hovering = !override && !!routeHover?.segments;
  const { segments: drawnSegments, head } = override || hovering || $("inspector").hidden ? { segments: selected, head: null } : revealedSegments(selected);
  let count = 0;
  ctx.save();
  ctx.lineJoin = "round";
  ctx.lineCap = "round";
  const screenScale = scale * ctx.canvas.clientWidth / ctx.canvas.width;
  ctx.strokeStyle = "#fff0b3";
  ctx.lineWidth = (hovering ? 3.4 : 2.2) / screenScale;
  for (const segment of drawnSegments) {
    if (segment[0]?.level !== level) continue;
    count += segment.filter(p => panelContains(canvasPanel(ctx.canvas), { x: p.x, y: p.y - 8 })).length;
    ctx.beginPath();
    segment.forEach((p, i) =>
      i ? ctx.lineTo(p.x, p.y - 8) : ctx.moveTo(p.x, p.y - 8),
    );
    ctx.save();
    ctx.strokeStyle = "rgba(22,30,36,.5)";
    ctx.lineWidth = (hovering ? 5.4 : 4) / screenScale;
    ctx.stroke();
    ctx.restore();
    ctx.stroke();
  }
  for (let i = 1; i < drawnSegments.length; i++) {
    const exit = drawnSegments[i - 1].at(-1),
      entrance = drawnSegments[i][0];
    for (const p of [exit, entrance])
      if (p?.level === level) {
        ctx.fillStyle = "#f4dda2";
        ctx.beginPath();
        ctx.arc(p.x, p.y - 8, 3 / scale, 0, Math.PI * 2);
        ctx.fill();
        ctx.font = `${10 / scale}px system-ui`;
        ctx.fillText(String(i), p.x + 5 / scale, p.y - 8 - 5 / scale);
      }
  }
  if (head?.level === level) {
    ctx.fillStyle = "#fff6d2";
    ctx.shadowColor = "#ffd55a";
    ctx.shadowBlur = 14;
    ctx.beginPath();
    ctx.arc(head.x, head.y - 8, 5 / screenScale, 0, Math.PI * 2);
    ctx.fill();
  }
  ctx.restore();
  return count;
}

function animate(now) {
  if (terminalDrain && (reducedMotion.matches || heat.clock(now) >= terminalDrain.end)) {
    heat.setRunning(false, reducedMotion.matches ? now : now - (heat.clock(now) - terminalDrain.end));
    terminalDrain = null;
    paused = true;
    renderPauseButton();
  }
  const wallElapsed = lastTime ? now - lastTime : 0,
    elapsed = Math.min(100, wallElapsed);
  lastTime = now;
  if (wallElapsed) frameGap = frameGap * 0.9 + Math.min(100, wallElapsed) * 0.1;
  const mapMoving = !paused || !!camera.focus || !!panHold || playing || controlMode;
  if (statsDirty) {
    statsDirty = false;
    updateStats();
  } else if (liveView && !paused) updateCounters();
  if (mapMoving || now - lastMapPaint >= 1000 / 30) {
    drawMap(now);
    lastMapPaint = now;
  }
  $("film")
    .closest(".screen")
    .classList.toggle("updating", seeking && now - seekStarted > 120);
  if (controlMode && !seeking && current && !document.hidden) {
    frameCredit += (elapsed * 60) / 1000;
    const frames = Math.round(frameCredit);
    frameCredit -= frames;
    for (let i = 0; i < frames; i++) {
      if (
        currentFrame >= 200000 ||
        !appendInput(current.actions, controller.buttons(), 1)
      ) {
        stopControl();
        $("branch-message").hidden = false;
        $("branch-message").textContent =
          "History limit reached. Choose an earlier frame to continue.";
        break;
      }
      engine.run(controller.buttons(), 1, true);
      music.advance(controller.buttons());
      currentFrame++;
      if (currentFrame % 8 === 0) recordTrail();
    }
    $("film").dataset.audioFrames = music.samples;
    current.frames = currentFrame;
    if (current.branch?.manual) current.branch.manual.to = currentFrame;
    current.observation = engine.observation();
    $("scrub").max = currentFrame;
    segments = trailSegments(trace);
    drawFilm();
  }
  holdSearchForReplay();
  if (playing && !seeking && current) {
    frameCredit += ((wallElapsed * 60) / 1000) * Number($("speed").value);
    const frames = Math.round(frameCredit);
    frameCredit -= frames;
    if (frames) {
      advanceFilm(Math.min(current.frames, currentFrame + frames));
      $("film").dataset.audioFrames = music.samples;
      segments = trailSegments(trace);
      if (currentFrame === current.frames) {
        playing = false;
        music.stop();
        $("play").textContent = "↺ Replay";
        verify().then(updateBranchControls).catch(replayError);
      }
    }
  }
  requestAnimationFrame(animate);
}
let mapLayoutWidth = 0;
const mapResize = new ResizeObserver(([entry]) => {
  const width = Math.round(entry.contentRect.width);
  if (!width || Math.abs(width - mapLayoutWidth) < 32) return;
  mapLayoutWidth = width;
  for (const key of views.keys()) if (typeof key === "string") views.delete(key);
  renderMapRows(catalog.levels.find(l => l.rooms.includes(mapLevel)));
  drawMap(performance.now());
});
mapResize.observe($("map-rows"));
let tourSession = null,
  tourOffered = tourSeen();
function tourMap() {
  const cell = tourSession?.cell;
  return cell && mapCanvas(cell.level, { x: cell.x * 32 + 16, y: cellTop(cell.y) + 16 });
}
function tourCellRect() {
  const c = tourMap(),
    cell = tourSession?.cell;
  if (!c || !cell) return null;
  const r = c.getBoundingClientRect(),
    map = canvasPanel(c),
    view = roomView(cell.level, c),
    scale = Math.min(c.width / map.width, c.height / map.height) * view.zoom,
    x = (((cell.x * 32 - view.x) * scale + c.width / 2) * r.width) / c.width,
    y =
      (((cellTop(cell.y) - view.y) * scale + c.height / 2) * r.height) /
      c.height;
  return {
    element: c,
    left: Math.max(r.left, r.left + x),
    top: Math.max(r.top, r.top + y),
    right: Math.min(r.right, r.left + x + (32 * scale * r.width) / c.width),
    bottom: Math.min(r.bottom, r.top + y + (32 * scale * r.height) / c.height),
  };
}
function tourCell(prefer = camera.introRoom ?? mapLevel) {
  if (selectedCell?.ids.length) return selectedCell;
  return [...heat.cells.values()]
    .filter((c) => c.ids.length > 1 && c.x * 32 >= 96)
    .sort(
      (a, b) =>
        (b.level === prefer) - (a.level === prefer) ||
        b.x - a.x ||
        b.ids.length - a.ids.length ||
        b.visits - a.visits,
    )[0];
}
function activeRoomSpotlights() {
  const rooms = cameraRooms(), runs = [];
  rooms.forEach((room, i) => {
    if (!swarmRooms.get(room)?.length) return;
    if (runs.at(-1)?.end === i - 1) runs.at(-1).end = i;
    else runs.push({ start: i, end: i });
  });
  if (!runs.length) return [rowsSpotlight([tourMap()])];
  return runs.map(({ start, end }) => rowsSpotlight(rooms.slice(start, end + 1).map((room) => mapCanvas(room))));
}
function rowsSpotlight(canvases) {
  const rows = [...new Set(canvases.map((c) => c?.closest(".map-row")).filter(Boolean))];
  if (!rows.length) return null;
  return {
    element: $("map-rows"), padding: 4,
    getBoundingClientRect() {
      const boxes = rows.map((row) => row.getBoundingClientRect());
      const left = Math.min(...boxes.map((r) => r.left)), top = Math.min(...boxes.map((r) => r.top));
      const right = Math.max(...boxes.map((r) => r.right)), bottom = Math.max(...boxes.map((r) => r.bottom));
      return { left, top, right, bottom, width: right - left, height: bottom - top };
    },
    scrollIntoView: (options) => rows[0].scrollIntoView(options),
  };
}
function searchesSpotlight() {
  const element = $("branches");
  return { element, padding: 0, radius: 14,
    getBoundingClientRect: () => element.getBoundingClientRect(),
    scrollIntoView: (options) => element.scrollIntoView(options) };
}
const STEP = { many: 0, one: 1, you: 2, fork: 3, tree: 4 };
const tour = new GuidedTour({
  occluders: () => [$("inspector"), ...(document.body.classList.contains("tour-phone") && tour.index >= STEP.fork ? [$("branches")] : [])],
  steps: [
    {
      id: "many",
      reveal: () => tourMap(),
      title: "Multiverse exploration",
      copy: [
        "Harmony plays games by creating a multiverse of timelines.",
        "It continually tries small actions all over the areas it has explored, and keeps the ones that produce progress.",
      ],
      targets: () => activeRoomSpotlights(),
      interactive: () => [$("map-rows"), $("pause")],
    },
    {
      id: "one",
      reveal: () => tourMap(),
      title: "Zooming in",
      ping: () => (traceReveal ? null : tourCellRect()),
      copy: [
        "Harmony can zoom in on any single timeline it has explored. You can press Replay to watch how it reached this one particular state.",
      ],
      targets: () => [
        segments.length
          ? rowsSpotlight([...new Set([tourSession?.cell?.level, ...segments.flat().map((p) => p.level)])].map((room) => mapCanvas(room)))
          : tourCellRect(),
        $("inspector"),
      ],
      interactive: () => [document.querySelector(".transport"), $("sound"), $("state-list"), $("map-rows"), $("take-control")],
    },
    {
      id: "you",
      title: "🎮 Your turn",
      copy: [
        "Harmony lets you jump in and play at any point within any timeline. Give it a shot!",
      ],
      targets: () => [$("take-control").hidden ? $("game-controls") : $("take-control"), document.querySelector(".screen")],
      interactive: () => [$("inspector")],
    },
    {
      id: "fork",
      searches: true,
      skip: () => !tourSession?.gameplay,
      title: "Hand it back",
      copy: [
        "Harmony can start a new search from right where you left off, or you can discard your branch.",
      ],
      targets: () => $("inspector").hidden ? [searchesSpotlight()] : [document.querySelector(".branch-actions"), document.querySelector(".screen")],
      interactive: () => [$("inspector"), $("branches")],
    },
    {
      id: "tree",
      searches: true,
      title: "Search branches",
      copy: [
        "Harmony keeps track of the branches you create. You can switch between them to compare and guide the search.",
      ],
      targets: () => [searchesSpotlight()],
      interactive: () => [$("branches")],
    },
  ],
  onStart() {
    if ($("workspace").classList.contains("timeline-collapsed")) $("timeline-toggle").click();
    if (visualization !== "movement") setVisualization("movement");
    tourSession = {
      cell: tourCell(),
      wasPaused: paused && !replayPaused,
      prepared: false,
      previousManual: String(current?.id).startsWith("manual-")
        ? current
        : null,
      previousFrame: currentFrame,
      interacted: false,
    };
    replayPaused = false;
    userSelected = true;
    playing = false;
    music.stop();
    $("play").textContent = "▶ Replay";
  },
  async beforeStep(index, valid) {
    hideRoutePreview();
    if (index <= STEP.you && tourSession.gameplay && playSession) {
      closeInspector({ restoreFocus: false });
      tourSession.prepared = tourSession.gameplay = false;
    }
    if (index <= STEP.one) expandInspector(false);
    if (controlMode && index !== STEP.you && index !== STEP.fork) stopControl();
    if (index === STEP.tree) return;
    if (index !== STEP.one) {
      playing = false;
      if (!controlMode) music.stop();
      $("play").textContent = "▶ Replay";
    }
    if (index === STEP.fork && tourSession.gameplay) {
      paused = !!playSession;
      worker.postMessage({ type: paused ? "pause" : "resume" });
      updateSearchControl();
      followReplayRoom(markerPoint());
      return;
    }
    if (index === STEP.one && !tourSession.prepared) tourSession.cell = tourCell();
    const cell = (tourSession.cell ||= tourCell());
    if (!cell) throw new Error("No retained cell yet");
    if (index <= STEP.one && mapLevel !== cell.level) setRoom(cell.level, true);
    if (index > STEP.one) followReplayRoom(markerPoint());
    if (index === STEP.many) {
      if (!tourSession.interacted && !tourSession.wasPaused && !stats.stopped && paused) {
        paused = false;
        worker.postMessage({ type: "resume" });
        updateSearchControl();
      }
      return;
    }
    if (index === STEP.one) stopCamera();
    paused = true;
    worker.postMessage({ type: "pause" });
    updateSearchControl();
    const fresh = !tourSession.prepared || !current || $("inspector").hidden;
    if (fresh) {
      if (selectedCell?.key !== cell.key || !current || $("inspector").hidden)
        inspect(cell);
    }
    const deadline = performance.now() + 15000;
    while (valid()) {
      if (
        !seeking &&
        current &&
        historyVerified
      ) {
        if (frameObservation?.health) {
          tourSession.prepared = true;
          break;
        }
        const alive = cell.ids
          .map((id) => stateCache.get(id))
          .find((s) => s?.observation.health);
        if (alive) await selectState(alive);
        else throw new Error("No playable route here");
      }
      if (performance.now() > deadline) throw new Error("Route not ready");
      await new Promise((resolve) => setTimeout(resolve, 16));
    }
    if (!valid()) return;
    if (index === STEP.one) {
      frameRoute();
      $("state-list")
        .querySelector(".selected")
        ?.scrollIntoView({ block: "nearest", behavior: "instant" });
    }
    if (index >= STEP.fork && !document.body.classList.contains("tour-phone"))
      $("branches").scrollIntoView({
        block: "nearest",
        behavior: "instant",
      });
  },
  onClose() {
    const { previousManual: previous, previousFrame, interacted } = tourSession;
    if (controlMode) stopControl();
    clearBranchPreview();
    hideRoutePreview();
    tourSession = null;
    let restoration;
    if (!interacted && previous && current !== previous) {
      invalidateStateRequest();
      selectedCell = null;
      clearRouteSelection();
      restoration = selectState(previous)
        .then(async (success) => {
          if (!success) return;
          if (currentFrame !== previousFrame && !(await seek(previousFrame)))
            return;
          if (!tour.open && current === previous)
            $("film").scrollIntoView({ block: "nearest", behavior: "instant" });
        })
        .catch(fail);
    }
    if (ready && !stats.stopped) {
      paused = false;
      worker.postMessage({ type: "resume" });
      updateSearchControl();
    }
    applyCameraMode();
    return restoration;
  },
});
function beginTour() {
  if ($("tour-open").disabled || tour.open) return;
  tourOffered = true;
  tour.start();
}
function offerTour() {
  if (
    !tourOffered &&
    !userSelected &&
    !document.hidden &&
    !$("info").open &&
    !$("level-picker").open &&
    camera.introDone &&
    stats.executions >= 30 &&
    tourCell()
  )
    beginTour();
}
$("tour-open").onclick = beginTour;
function familyOf(choice) {
  return choice.family ? [...GAMES.values()].filter((member) => member.family === choice.family) : [choice];
}
$("new-search").onclick = () => {
  const wasPlaying = playing;
  if (wasPlaying) { playing = false; music.stop(); $("play").textContent = "▶ Replay"; }
  const games = [...GAMES.values()].filter((choice) => !choice.family || choice.id === choice.family),
    family = familyOf(game);
  pickerGames = [];
  const buttons = games.map((choice) => {
    const button = document.createElement("button");
    button.textContent = choice.title;
    button.setAttribute("aria-pressed", choice === game);
    pickerGames.push({ button, choice });
    const play = () => {
      const url = new URL(location.href);
      url.searchParams.set("game", choice.id);
      location.assign(url);
    };
    button.onclick = () => {
      if (nativeTarget) showNativeLevels(choice);
      else if (choice.rom.upload && !bundled.has(choice.id) && (choice !== requested || missingRom)) showRomPanel(choice, play);
      else if (choice === game || (choice.family && choice.family === game.family)) showLevelGrid();
      else play();
    };
    return button;
  });
  $("homebrew-games").replaceChildren(...buttons.filter((_, i) => !games[i].rom.upload));
  $("rom-games").replaceChildren(...buttons.filter((_, i) => games[i].rom.upload));
  const shown = missingRom && [...GAMES.values()].find((choice) => choice === missingRom);
  if (shown) showRomPanel(shown, () => location.reload());
  else showLevelGrid();
  applyTarget();
  renderLevels(family);
  if (game.buildMap) builtMaps.wanted = catalog.levels[0].rooms[0];
  $("level-picker").showModal();
};
function renderLevels(family) {
  $("level-grid").replaceChildren();
  for (const member of family) {
    if (family.length > 1) {
      const heading = document.createElement("h3");
      heading.className = "level-group";
      heading.textContent = member.group;
      $("level-grid").append(heading);
    }
    const current = member === game,
      memberAssets = current ? assets : new URL(member.assets, base);
    for (const level of current ? catalog.levels : member.levels) {
      const card = document.createElement("button"), caption = document.createElement("span");
      let image = document.createElement("img");
      card.className = "level-card";
      card.dataset.game = member.id;
      card.dataset.level = level.id;
      card.setAttribute("aria-label", family.length > 1 ? `${member.group}: ${level.label}` : level.label);
      card.setAttribute("aria-current", current && level.id === bootLevel ? "true" : "false");
      if (current && game.buildMap) {
        image = document.createElement("canvas");
        Object.assign(image, { width: 256, height: 96, className: "level-preview" });
        image.dataset.level = level.rooms[0];
        const bitmap = panorama(level.rooms[0]);
        if (bitmap) drawLevelPreview(image, bitmap);
      } else {
        image.src = new URL(`maps/${level.rooms[0]}-preview.png`, memberAssets).href; image.loading = "lazy"; image.alt = "";
      }
      caption.textContent = level.label;
      card.append(image, caption);
      const open = current
        ? () => {
            $("level-picker").close();
            tour.finish();
            ++seed;
            startSearch(level.id);
            $("new-search").focus({ preventScroll: true });
          }
        : () => {
            const url = new URL(location.href);
            url.searchParams.set("game", member.id);
            url.searchParams.set("level", level.id);
            location.assign(url);
          };
      card.onclick = () => (nativeTarget ? showHardware(member, level, card) : open());
      $("level-grid").append(card);
    }
  }
}
$("close-level-picker").onclick = () => $("level-picker").close();
let launcher = null, nativeTarget = null, pickerGames = [], nativeChoice = game;
function renderTargetRow() {
  $("run-target").hidden = !launcher;
  if (!launcher) return;
  const heading = document.createElement("h3");
  heading.textContent = "Run on";
  $("run-target").replaceChildren(heading, segmentButtons([["", "This browser"], ...launcher.machines.map((name) => [name, name])], nativeTarget ?? "", (name) => {
    nativeTarget = name || null;
    if (nativeTarget) launcher.setMachine(nativeTarget);
    applyTarget();
  }));
}
function nativeGame(choice) {
  return [...GAMES.values()].some((member) => (member.family ?? member.id) === (choice.family ?? choice.id) && launcher.available(member.id));
}
function applyTarget() {
  renderTargetRow();
  for (const { button, choice } of pickerGames) button.disabled = !!nativeTarget && !nativeGame(choice);
  if (nativeTarget) showNativeLevels(nativeGame(nativeChoice) ? nativeChoice : pickerGames.find(({ choice }) => nativeGame(choice))?.choice ?? game);
  else if ($("rom-panel").hidden) {
    renderLevels(familyOf(game));
    showLevelGrid();
  }
}
function showNativeLevels(choice) {
  nativeChoice = choice;
  if (!choice.levels || !choice.assets) return showHardware(choice);
  renderLevels(familyOf(choice));
  showLevelGrid(choice);
}
function showHardware(member, level, card) {
  pressGame(nativeChoice);
  $("rom-panel").hidden = true;
  $("level-grid").hidden = true;
  closePlaythrough();
  launcher.setLevel(member.id, level, card?.querySelector("img, canvas"), () => showNativeLevels(nativeChoice));
  $("native-panel").hidden = false;
  launcher.activate();
}
let playthroughPlayer = null;
function pressGame(choice) {
  for (const button of $("game-choice").querySelectorAll("#homebrew-games button, #rom-games button")) button.setAttribute("aria-pressed", button.textContent === choice.title);
}
function showLevelGrid(choice = game) {
  pressGame(choice);
  $("native-panel").hidden = true;
  launcher?.deactivate();
  $("rom-panel").hidden = true;
  $("level-grid").hidden = false;
  closePlaythrough();
}
function showRomPanel(choice, play) {
  pressGame(choice);
  $("native-panel").hidden = true;
  launcher?.deactivate();
  $("level-grid").hidden = true;
  $("rom-panel").hidden = false;
  $("rom-panel").dataset.game = choice.id;
  $("rom-title").textContent = `To explore ${choice.title} please supply the game’s ROM`;
  $("rom-message").hidden = true;
  const drop = $("rom-drop");
  const accept = (file) => file && acceptRom(choice, file, play);
  drop.onclick = () => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = ".nes,application/octet-stream";
    input.onchange = () => accept(input.files?.[0]);
    input.click();
  };
  drop.ondragover = (event) => {
    event.preventDefault();
    drop.classList.add("dragging");
  };
  drop.ondragleave = () => drop.classList.remove("dragging");
  drop.ondrop = (event) => {
    event.preventDefault();
    drop.classList.remove("dragging");
    accept(event.dataTransfer?.files?.[0]);
  };
  closePlaythrough();
  const video = choice.playthrough;
  $("playthrough").hidden = !video;
  if (!video) return;
  $("playthrough-note").hidden = !video.note;
  $("playthrough-note").textContent = video.note || "";
  youtubeApi()
    .then((YT) => {
      if ($("rom-panel").hidden || $("rom-panel").dataset.game !== choice.id || !$("level-picker").open) return;
      let fast = false;
      playthroughPlayer = new YT.Player("playthrough-video", {
        host: "https://www.youtube-nocookie.com",
        videoId: video.id,
        playerVars: { rel: 0, playsinline: 1 },
        events: {
          onReady: (event) => event.target.setPlaybackRate(2),
          onStateChange: (event) => {
            if (event.data !== YT.PlayerState.PLAYING || fast) return;
            fast = true;
            event.target.setPlaybackRate(2);
          },
        },
      });
    })
    .catch(() => ($("playthrough").hidden = true));
}
function closePlaythrough() {
  playthroughPlayer?.destroy?.();
  playthroughPlayer = null;
  const frame = document.querySelector(".playthrough-frame");
  if (frame && !frame.querySelector("#playthrough-video")) {
    const div = document.createElement("div");
    div.id = "playthrough-video";
    frame.replaceChildren(div);
  }
}
function youtubeApi() {
  if (window.YT?.Player) return Promise.resolve(window.YT);
  return (youtubeApi.loading ||= new Promise((resolve, reject) => {
    window.onYouTubeIframeAPIReady = () => resolve(window.YT);
    const script = document.createElement("script");
    script.src = "https://www.youtube.com/iframe_api";
    script.onerror = reject;
    document.head.append(script);
  }));
}
$("level-picker").addEventListener("close", () => {
  closePlaythrough();
  launcher?.deactivate();
  if (!worker) startSearch();
});
async function acceptRom(choice, file, play) {
  const bytes = new Uint8Array(await file.arrayBuffer());
  $("rom-message").hidden = false;
  if (bytes.length !== choice.rom.bytes || (await sha256Hex(bytes)) !== choice.rom.sha256)
    $("rom-message").textContent = `That file is not the ${choice.title} ROM this demo supports. It needs the ${choice.rom.bytes.toLocaleString()}-byte ROM with SHA-256 ${choice.rom.sha256.slice(0, 12)}…`;
  else if (!(await saveRom(choice.id, bytes)))
    $("rom-message").textContent = "This browser blocks site storage, so the ROM cannot be kept for the page.";
  else play();
}

const tapeUrl = params.get("tape");
let tapeStarted = false;
function startTape() {
  if (tapeStarted || !engine || !ready) return;
  tapeStarted = true;
  watchTape().catch(fail);
}
async function watchTape() {
  const tape = await (await fetch(new URL(tapeUrl, location.href))).json();
  if (!Array.isArray(tape.actions) || !tape.actions.length || tape.actions.length > 10000) throw new Error("Unsupported tape");
  const actions = tape.actions.map((a) => ({ buttons: a.buttons, frames: a.frames ?? a.hold_frames }));
  for (const a of actions)
    if (!Number.isInteger(a.buttons) || a.buttons < 0 || a.buttons > 255 || !Number.isInteger(a.frames) || a.frames < 1 || a.frames > game.maxActionFrames)
      throw new Error("Invalid controller action");
  const frames = actions.reduce((total, a) => total + a.frames, 0);
  engine.restore(origin);
  for (const a of actions) engine.run(a.buttons, a.frames);
  const observation = engine.observation();
  engine.restore(origin);
  userSelected = true;
  paused = true;
  worker?.postMessage({ type: "pause" });
  updateSearchControl();
  mergeProgress(cleared, levelProgress(observation), bootLevel);
  openInspector();
  await selectState({ id: "tape", boot_level: bootLevel, actions, frames, observation }, true);
}
requestAnimationFrame(animate);
createEngine(base, game, engineInputs)
  .then((e) => {
    engine = e;
    origin = e.boot(bootLevel);
    originPixels = e.pixels();
    drawFilm();
    $("verification").textContent = "Original game";
    if (tapeUrl) return startTape();
    const state = stateCache.get(0);
    if (state) selectState(state, true).catch(fail);
  })
  .catch(fail);
const linkedLevel = params.has("level") ? Number(params.get("level")) : null;
if (catalog.levels.some((level) => level.id === linkedLevel)) startSearch(linkedLevel);
else if (liveView || tapeUrl) startSearch(catalog.levels[0].id);
else {
  askedForRom = true;
  $("new-search").onclick();
}
detectLauncher()
  .then((options) => {
    if (!options) return;
    launcher = nativePanel(options, [...GAMES.values()], game.id);
    $("native-panel").replaceChildren(launcher.element);
    renderTargetRow();
  })
  .catch(() => {});
