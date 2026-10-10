// SPDX-License-Identifier: AGPL-3.0-or-later
import "./style.css";
import { GuidedTour, tourSeen } from "./tour.js";
import { ReplayTimeline, prefixTrail, trailPoint } from "./replay.js";
import { RoutePreviews } from "./preview.js";
import { GameAudio } from "./audio.js";
import { createEngine, ROM_SHA256, CORE_REVISION } from "./emulator.js";
import { Heatmap, routeIds } from "./heat.js";
import { NovaSwarm } from "./swarm.js";
import { snapshotHash } from "./media.js";
import { roomPanels, panelContains, panelCenter } from "./view.js";
import {
  prefixAt,
  appendInput,
  Controller,
  keyButtons,
  trailSegments,
  branchInfo,
} from "./branch.js";
import { StateCache, memoryBudget } from "./memory.js";
import {
  project,
  completedLevels,
  mergeProgress,
  isMapEvidence,
  followDiscovery,
} from "./world.js";
const base = new URL(import.meta.env.BASE_URL, location.href),
  $ = (id) => document.getElementById(id);
const compactReplay = matchMedia("(max-width: 800px), (pointer: coarse) and (max-width: 1000px) and (max-height: 500px)");
const catalog = await (await fetch(new URL("maps.json", base))).json();
const maps = new Map(catalog.maps.map((map) => [map.id, map]));
document.querySelector("#app").innerHTML = `
<header><a class="brand" href="https://github.com/pH14/harmony"><b>harmony</b></a><button id="theme" class="theme-button" aria-label="Switch color theme">◐</button></header>
<main><div class="workspace" id="workspace"><aside id="branches" class="branches" aria-label="Search branches"><div class="pane-heading"><h2>Searches</h2><button id="timeline-toggle" aria-expanded="true" aria-label="Collapse Searches">‹</button></div><nav aria-label="Search branches"><ol id="branch-tree"></ol></nav></aside><section class="exploration" aria-label="Exploration"><h2 class="exploration-heading">Exploration</h2><div class="toolbar"><div class="controls" hidden><span id="status" hidden>Loading Nova…</span><i id="status-dot" hidden></i><button id="pause" class="icon-button" aria-label="Pause Search" title="Pause Search" disabled></button><button id="reset" class="icon-button" aria-label="Restart Search" title="Restart Search" disabled><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 10a8 8 0 1 1 1 8M4 4v6h6" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/></svg></button></div><span id="branch-feedback" class="visually-hidden" role="status"></span><div class="metrics"><div><b id="attempts">0</b><span>paths explored</span></div><div><b id="states">0</b><span>states retained</span></div><div><b id="cells">0</b><span>cells visited</span></div><div><b id="work">0</b><span>game frames executed</span></div><div><b id="memory">0 MB</b><span>archive memory</span></div></div><button id="tour-open" disabled>Guided tour</button></div>
<div class="goal"><div><button id="goal-title" class="level-picker-open" aria-haspopup="dialog" aria-controls="level-picker" disabled>World 1 – Level 1</button><span id="goal-status" hidden></span></div><div><div id="visualization" class="segments" role="group" aria-label="Visualization" data-value="movement"><button data-viz="movement" aria-pressed="true">Movement</button><button data-viz="heat" aria-pressed="false">Heatmap</button><button data-viz="both" aria-pressed="false">Both</button></div><button id="completion" hidden>Watch completion</button></div></div>
<div class="map-wrap"><div id="map-rows"></div><canvas id="map" width="1280" height="320" tabindex="0" aria-label="Game area heatmap. Drag to move when zoomed. Arrow keys move the selection; Enter inspects a cell."></canvas><span class="map-label" id="map-label" hidden>INTRODUCTION</span><div id="map-hint" hidden>Click a warm cell to watch its history</div><div id="hover" hidden></div></div>
<div class="map-footer"><span id="memory-limit" hidden></span></div>
<p class="game-attribution"><a href="https://github.com/NovaSquirrel/NovaTheSquirrel">Nova the Squirrel</a> by <a href="https://novasquirrel.com/">NovaSquirrel</a> · Original game artwork <a href="https://creativecommons.org/licenses/by-nc-sa/4.0/">CC BY-NC-SA 4.0</a></p>
</section>
<section class="inspect inspector" id="inspector" aria-label="History inspector" hidden><div class="drawer-top"><div class="history-heading"><h2 id="film-title">History</h2></div><div class="history-controls"><button id="sound" class="icon-button" aria-label="Mute game audio" title="Mute game audio" aria-pressed="false"></button><button id="expand-inspector" aria-expanded="false">Expand</button><button id="close-inspector" aria-controls="inspector" aria-label="Collapse History" aria-expanded="true" title="Collapse History">›</button></div></div><div class="film"><span id="verification" hidden>Starting emulator</span><div class="screen"><canvas id="film" tabindex="0" width="256" height="224" aria-label="Nova gameplay replay"></canvas><span id="frame-label">FRAME 0</span></div><div class="transport"><div class="replay-actions"><button id="play" disabled>▶ Replay</button></div><input id="scrub" aria-label="Replay frame" type="range" min="0" max="0" value="0" disabled><select id="speed" aria-label="Playback speed"><option value="1" selected>1×</option><option value="4">4×</option><option value="12">12×</option></select></div><div id="game-controls" hidden><div class="keyboard-guide" aria-label="Keyboard controls"><span><kbd>↑ ← ↓ →</kbd><kbd>WASD</kbd><span>Move</span></span><span><kbd>Z</kbd><kbd>Space</kbd><span>Jump</span></span><span><kbd>X</kbd><span>Ability</span></span></div><div class="touch-controls" aria-label="Game controller"><div class="dpad"><button data-button="16" aria-label="Move up">↑</button><button data-button="64" aria-label="Move left">←</button><button data-button="32" aria-label="Move down">↓</button><button data-button="128" aria-label="Move right">→</button></div><div class="action-buttons"><button data-button="2" aria-label="Use ability">B</button><button data-button="1" aria-label="Jump">A</button></div></div></div><div class="branch-actions"><button id="take-control" disabled>🎮 Play from here</button><button id="search-here" hidden disabled>↗ Branch search from here</button><button id="discard-branch" hidden><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 6h16M9 6V3h6v3M6 6l1 15h10l1-15M10 10v7m4-7v7"/></svg>Discard branch</button></div><div id="branch-message" role="status" hidden></div></div>
<aside class="state-picker" aria-label="Retained histories"><div class="section-title"><h2 id="cell-title">Retained history</h2><span id="cell-visits">Live</span></div><p id="selection-hint" hidden></p><div id="state-list"></div><details class="state-disclosure"><summary>Game state</summary><div id="details" class="details"></div></details></aside></section><aside id="history-rail" class="history-rail" hidden><button id="history-reopen" aria-controls="inspector" aria-label="Expand History" aria-expanded="false" title="Expand History">‹<span>History</span></button></aside></div>
<footer><button id="credits">Credits & source</button></footer>
<div id="route-preview" class="route-preview" hidden><canvas width="256" height="224"></canvas><span></span></div><div id="error" role="alert" hidden></div>
<dialog id="level-picker" aria-labelledby="level-picker-title"><div class="level-picker-heading"><button id="close-level-picker" class="close" aria-label="Close level selector">×</button><h2 id="level-picker-title">Choose a starting point</h2><p>A fresh search, from any level.</p></div><div id="level-worlds"></div></dialog>
<dialog id="info"><button id="close-info" class="close" aria-label="Close">×</button><div id="info-content"></div></dialog></main>`;
let heat = new Heatmap(),
  worker,
  engine,
  origin,
  originPixels,
  bootLevel = 0,
  gameWon = false,
  paused = false,
  ready = false,
  seed = 2,
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
  sparks = [],
  bestByMap = new Map(),
  bestStates = new Map(),
  arrivals = new Map(),
  seenMaps = new Set(),
  cleared = new Set(),
  wins = new Map(),
  stats = {};
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
  lastDetailPaint = 0;
const searchViews = new Map(),
  reducedMotion = matchMedia("(prefers-reduced-motion: reduce)");
function saveSearchView() {
  heat.setRunning(false, performance.now());
  searchViews.set(activeSearch, {
    heat,
    bestByMap,
    bestStates,
    arrivals,
    seenMaps,
    cleared,
    wins,
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
    gameWon,
    latestBest,
    stats,
    branchOrigin,
  } = view);
  searchViews.set(id, view);
  sparks = [];
  swarmRooms = swarm.frame(id, heat.clock(performance.now()), reducedMotion.matches);
  hoverCell = selectedCell = null;
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
  const panels = [...document.querySelectorAll(`.map-row[data-map="${id}"] canvas`)];
  return (point && panels.find(c => panelContains(canvasPanel(c), { x: point.x, y: point.y - 8 }))) || panels[0];
}
function roomView(id, c) {
  const key = c ? `${id}:${c.dataset.panel}` : id;
  if (!views.has(key)) {
    const map = c ? canvasPanel(c) : { x: 0, y: 0, ...maps.get(id) };
    views.set(key, { zoom: c ? roomView(id).zoom : 1, x: (map.x || 0) + map.width / 2, y: (map.y || 0) + map.height / 2 });
  }
  return views.get(key);
}
const budget = memoryBudget(
  navigator.deviceMemory,
  matchMedia("(pointer: coarse)").matches,
);
const swarm = new NovaSwarm(
  (budget.snapshotsMiB === 32 ? 2 : 8) * 1048576,
  budget.snapshotsMiB === 32 ? 2048 : 8192,
);
let visualization = "movement", swarmRooms = new Map();
const sprites = new Image();
sprites.src = new URL("nova-sprites.png", base).href;
function setVisualization(mode) {
  visualization = mode;
  $("visualization").dataset.value = mode;
  for (const button of $("visualization").children) button.setAttribute("aria-pressed", button.dataset.viz === mode);
  $("map-hint").hidden = mode === "movement";
  $("hover").hidden = true;
  drawMap(performance.now());
}
for (const button of $("visualization").children) button.onclick = () => {
  userSelected = true;
  setVisualization(button.dataset.viz);
};
let theme;
try { theme = localStorage.getItem("harmony.nova.theme"); } catch {}
function applyTheme(value) {
  document.documentElement.dataset.theme = value;
  $("theme").setAttribute("aria-label", `Switch to ${value === "dark" ? "light" : "dark"} mode`);
}
const themeMedia = matchMedia("(prefers-color-scheme: dark)");
applyTheme(theme === "light" || theme === "dark" ? theme : themeMedia.matches ? "dark" : "light");
themeMedia.addEventListener("change", () => { if (!theme) applyTheme(themeMedia.matches ? "dark" : "light"); });
$("theme").onclick = () => {
  theme = document.documentElement.dataset.theme === "dark" ? "light" : "dark";
  applyTheme(theme);
  try { localStorage.setItem("harmony.nova.theme", theme); } catch {}
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
function drawNovas(ctx, level, scale) {
  if (!sprites.complete || !sprites.naturalWidth) return 0;
  const map = maps.get(level), points = swarmRooms.get(level) || [];
  ctx.save();
  ctx.globalAlpha = 0.55;
  let count = 0;
  for (const raw of points) {
    const p = project(raw, map);
    if (p.x < 0 || p.x >= map.width || p.y < 0 || p.y >= map.height) continue;
    const size = Math.max(1, 4 / (16 * scale));
    ctx.drawImage(sprites, Math.floor(p.pose / 2) * 16, (p.pose & 1) * 24, 16, 24,
      p.x - (p.pose & 1 ? 0 : 8 * size), p.y + 16 - 24 * size, 16 * size, 24 * size);
    count++;
  }
  ctx.restore();
  return count;
}
let searchChoices = [{ id: 0, label: "Main", parent: null }],
  branchPreview = null,
  routeHover = null,
  previewTimer;
const routePreviews = new RoutePreviews(() => createEngine(base),
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
  routeHover = null;
  routePreviews.cancel();
  $("route-preview").hidden = true;
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
  const popup = $("route-preview"), rect = target.button.getBoundingClientRect();
  popup.querySelector("span").textContent = `${routeName(state.id)} · Previewing…`;
  popup.querySelector("canvas").hidden = true;
  popup.hidden = false;
  const width = Math.min(240, innerWidth - 24);
  popup.style.width = `${width}px`;
  popup.style.left = `${Math.max(12, Math.min(innerWidth - width - 12,
    rect.left >= width + 24 ? rect.left - width - 12 : rect.right + 12))}px`;
  popup.style.top = `${Math.max(12, Math.min(innerHeight - width * 224 / 256 - 48, rect.top))}px`;
  try {
    const pixels = await routePreviews.get(state);
    if (!pixels || target !== routeHover || !target.button.isConnected) return;
    target.segments = trailSegments(routePreviews.timeline.trail);
    target.point = isMapEvidence(state.observation, catalog.levels)
      ? project(state.observation, maps.get(state.observation.level)) : null;
    target.frames = state.frames;
    drawMap(performance.now());
    const canvas = popup.querySelector("canvas");
    canvas.getContext("2d").putImageData(new ImageData(pixels.data, pixels.width, pixels.height), 0, 0);
    canvas.hidden = false;
    popup.dataset.stateId = state.id;
    popup.querySelector("span").textContent = `${routeName(state.id)} · ${replayTime(state.frames)}`;
  } catch {
    if (target === routeHover) popup.querySelector("span").textContent = "Select this route to inspect it";
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
function renderBranches() {
  const pauseControl = $("pause"), resetControl = $("reset");
  const list = (parent, depth = 0) => {
    const ol = document.createElement("ol");
    for (const search of searchChoices.filter((s) => s.parent === parent)) {
      const li = document.createElement("li"), button = document.createElement("button");
      li.dataset.searchNode = search.id;
      button.dataset.search = search.id;
      button.textContent = search.label;
      if (depth > 3) {
        const parentLabel = searchChoices.find((s) => s.id === search.parent)?.label;
        const ancestry = document.createElement("small");
        ancestry.textContent = `from ${parentLabel}`;
        button.append(ancestry);
      }
      button.setAttribute("aria-pressed", search.id === activeSearch);
      button.title = search.branch ? `Forked at ${replayTime(search.branch.parent_frame)}` : "Original search";
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
        play.setAttribute("aria-label", `Resume ${search.label}`);
        play.title = `Resume ${search.label}`;
        play.innerHTML = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M7 4l14 8-14 8z"/></svg>';
        play.onclick = () => switchSearch(search.id);
      }
      const action = search.id === 0 ? resetControl : document.createElement("button");
      if (search.id !== 0) {
        action.className = "icon-button";
        action.dataset.deleteSearch = search.id;
        action.setAttribute("aria-label", `Delete ${search.label}`);
        action.title = `Delete ${search.label}`;
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
const music = new GameAudio(() => createEngine(base));
const panoramas = new Map();
function mapURL(level) {
  const url = new URL(maps.get(level).file, base);
  url.searchParams.set("v", "camera-hidden-1");
  return url.href;
}
function panorama(level) {
  if (!panoramas.has(level)) {
    const image = new Image();
    image.src = mapURL(level);
    panoramas.set(level, image);
  }
  return panoramas.get(level);
}
const canvas = $("map"),
  film = $("film").getContext("2d");
const fmt = (n) => Math.round(n || 0).toLocaleString();
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
  const p = currentFrame === 0 && originPixels ? originPixels : engine.pixels();
  film.putImageData(new ImageData(p.data, p.width, p.height), 0, 0);
  $("frame-label").textContent =
    `FRAME ${fmt(currentFrame)} / ${fmt(current?.frames)}`;
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
  if (!current?.snapshot) {
    $("verification").textContent = current?.frames
      ? "Controller history"
      : "Original game";
    return;
  }
  $("verification").textContent = equal(engine.capture(), current.snapshot)
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
      const deadline = performance.now() + 8;
      do {
        advanceFilm(Math.min(target, currentFrame + 120), false, target);
      } while (currentFrame < target && performance.now() < deadline);
      if (currentFrame < target)
        await new Promise((resolve) => setTimeout(resolve, 0));
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
  const selection = seek(autoplay ? 0 : state.frames),
    epoch = replayEpoch;
  const success = await selection;
  if (!success || current !== state || epoch !== replayEpoch) return false;
  if (autoplay) {
    if (userSelected && music.context) startAudio();
    playing = true;
    $("play").textContent = "Ⅱ Pause replay";
  }
  if (!autoplay) routePreviews.remember(state.id, state.frames === 0 ? originPixels : engine.pixels());
  renderStates();
  return true;
}
function renderDetails(observation) {
  $("details").replaceChildren();
  const fields = [
    ["Position", `${observation.x}, ${observation.y}`],
    ["Health", `${observation.health} / 4`],
    [
      "Chips",
      observation.chips_needed
        ? `${observation.chips} / ${observation.chips_needed}`
        : observation.chips,
    ],
    ["Ability", observation.ability],
    [
      "Area",
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
  return number !== undefined ? `Route ${number}` : "History";
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
    ? "Routes to this location"
    : "History";
  $("cell-title").title = selectedCell
    ? `Cell ${selectedCell.x}, ${selectedCell.y}`
    : "";
  $("cell-visits").textContent = selectedCell ? ids.length : "";
  $("cell-visits").title = selectedCell
    ? `${fmt(selectedCell.visits)} search visits; ${ids.length} retained routes`
    : "";
  $("state-list").replaceChildren(
    ...ids.map((id) => {
      const state = current?.id === id ? current : stateCache.get(id),
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
        ? `${replayTime(state.frames)} replay`
        : "Loading…";
      summary.append(name, duration);
      resources.className = "state-resources";
      resources.textContent = state
        ? `♥ ${state.observation.health}/4 · ${state.observation.chips} chips`
        : "";
      button.append(marker, summary, resources);
      button.setAttribute("aria-pressed", selected);
      if (state)
        button.setAttribute(
          "aria-label",
          `${routeName(id)}, ${Math.floor(state.frames / 60)} seconds of gameplay, health ${state.observation.health} of 4, ${state.observation.chips} chips`,
        );
      button.disabled = controlMode || branchBusy;
      button.onpointerenter = () => previewRoute(id, button);
      button.onfocus = () => previewRoute(id, button);
      button.onpointerleave = button.onblur = hideRoutePreview;
      button.onclick = () => {
        hideRoutePreview();
        if (tourSession) tourSession.interacted = true;
        userSelected = true;
        revealReplayRoom = true;
        ++request;
        if (state) selectState(state).catch(fail);
        else
          worker.postMessage({ type: "states", ids: [id], request: ++request });
      };
      return button;
    }),
  );
}
function inspect(cell) {
  hideRoutePreview();
  if (!cell || branchBusy) return;
  stopControl();
  music.stop();
  playing = false;
  ++request;
  $("inspector").dataset.empty = cell.ids.length ? "false" : "true";
  $("selection-hint").hidden = !!cell.ids.length;
  userSelected = true;
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
  $("cell-visits").textContent = `${fmt(cell.visits)} visits`;
  $("selection-hint").textContent = "";
  renderStates();
  if (cell.ids.length) {
    openInspector();
    worker.postMessage({ type: "states", ids: cell.ids, request: ++request });
  } else closeInspector({ restoreFocus: false });
}
function startSearch(level = bootLevel) {
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
  searchChoices = [{ id: 0, label: "Main", parent: null }];
  branchPreview = null;
  hideRoutePreview();
  routePreviews.clear();
  renderBranches();
  $("branch-message").hidden = true;
  if (worker) worker.terminate();
  heat = new Heatmap();
  swarm.clear();
  swarmRooms = new Map();
  stateCache = new StateCache();
  selectedCell = null;
  current = null;
  replayEpoch++;
  playing = false;
  userSelected = false;
  gameWon = false;
  paused = false;
  ready = false;
  seeking = false;

  bestByMap.clear();
  bestStates.clear();
  arrivals.clear();
  seenMaps.clear();
  cleared.clear();
  wins.clear();
  latestBest = null;
  lastAuto = 0;
  stats = {};
  for (const id of ["play", "scrub"]) $(id).disabled = true;
  $("details").replaceChildren();
  $("state-list").replaceChildren();
  updateStats();
  sparks = [];
  setRoom(bootLevel);

  $("error").hidden = true;
  $("memory-limit").hidden = true;
  $("status").textContent = "Loading Nova…";
  $("pause").disabled = true;
  updateSearchControl();

  $("status-dot").className = "";
  $("cell-title").textContent = "Retained history";
  worker = new Worker(new URL("./search-worker.js", import.meta.url), {
    type: "module",
  });
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
        activeSearch = data.active;
        const fresh = activateSearchView(activeSearch),
          handedOff = fresh && !!pendingOrigin,
          handoffTourStep = pendingOrigin?.tourStep;
        originPulse = null;
        if (handedOff) {
          branchOrigin = pendingOrigin;
          delete branchOrigin.tourStep;
          originPulse = { point: branchOrigin.point, start: performance.now() };
          saveSearchView();
        }
        pendingOrigin = null;
        ++request;
        selectedCell = hoverCell = null;
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
          mergeProgress(cleared, state.observation);
          for (const level of completedLevels(state.observation.cleared_levels))
            wins.set(level, state.id);
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
          $("branch-feedback").textContent =
            `Branch ${activeSearch} is searching from this frame.`;
          updateBranchControls();
          $("branch-tree").querySelector(`[data-search="${activeSearch}"]`).focus({ preventScroll: true });
          if (tour.open && [4, 5].includes(handoffTourStep) && tour.index === handoffTourStep) tour.go(handoffTourStep + 1);
        }
        if (engine) selectState(state, initial).catch(fail);
        updateStats();
        drawMap(performance.now());
      } else if (data.type === "batch") {
        if (data.active !== activeSearch) return;
        swarm.add(activeSearch, data.motion || [], heat.clock(performance.now()));
        const { motion, ...batchStats } = data;
        stats = batchStats;
        gameWon ||= data.won;
        const now = performance.now();
        heat.visitMotion(data.motion || [], maps, now);
        for (const point of data.points) {
          const o = point.observation,
            map = maps.get(o.level);
          for (const level of completedLevels(o.cleared_levels))
            if (!wins.has(level) && point.retained !== null)
              wins.set(level, point.retained);
          mergeProgress(cleared, o);
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
          sparks.push({ ...project(o, map), time: now });
          if (followDiscovery(o, first, focusedLevel, userSelected, cleared))
            setRoom(o.level);
        }
        sparks = sparks.slice(-120);
        if (
          latestBest !== null &&
          !userSelected &&
          now - lastAuto > 8000 &&
          !seeking
        ) {
          lastAuto = now;
          worker.postMessage({ type: "states", ids: [latestBest], request: 0 });
        }
        updateStats();
        offerTour();
      } else if (data.type === "states") {
        for (const state of data.states) stateCache.set(state.id, state);
        if (data.request === -2001) {
          if (routeHover) showRoutePreview(stateCache.get(routeHover.id), routeHover);
        } else if (data.request === 0) {
          if (!userSelected && data.states[0])
            selectState(data.states[0], true).catch(fail);
        } else if (data.request === request) {
          renderStates();
          if (data.states[0])
            selectState(data.states[0], watchRequested).catch(fail);
          watchRequested = false;
        }
      } else if (data.type === "limit") {
        paused = true;
        $("status").textContent = "Search limit reached";
        updateSearchControl();
        $("goal-status").textContent = data.won
          ? "Game complete"
          : "Search limit reached";
        $("pause").disabled = true;
        $("memory-limit").hidden = data.won;
        $("memory-limit").textContent = "Memory or search limit reached";
        $("status-dot").className = "paused";
      }
    } catch (e) {
      fail(e);
    }
  };
  worker.postMessage({ type: "init", base: base.href, seed, budget, boot_level: bootLevel });
}
function updateSearchControl() {
  heat.setRunning(ready && !paused && !stats.stopped, performance.now());
  $("pause").innerHTML = paused
    ? '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M7 4l14 8-14 8z"/></svg>'
    : '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M6 4h4v16H6zm8 0h4v16h-4z"/></svg>';
  const label = paused ? "Resume Search" : "Pause Search";
  $("pause").setAttribute("aria-label", label);
  $("pause").title = label;
  $("goal-status").textContent = !ready
    ? "Loading…"
    : paused
      ? "Paused"
      : "Searching";
}
function setRoom(level, preserveSelection = false) {
  const map = maps.get(level);
  if (!map) return;
  mapLevel = level;
  mapWidth = map.width;
  mapHeight = map.height;
  canvas.dataset.map = level;
  canvas.dataset.mapWidth = mapWidth;
  canvas.dataset.mapHeight = mapHeight;
  if (!preserveSelection) selectedCell = null;
  hoverCell = null;
  const owner = catalog.levels.find((l) => l.rooms.includes(level));
  focusedLevel = owner?.id ?? 0;
  $("map-label").textContent = map.label.toUpperCase();
  renderMapRows(owner);
  updateStats();
  panorama(level);
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
    worker.postMessage({ type: "states", ids: [id], request: ++request });
  }
}
function completionWitness() {
  if (wins.has(focusedLevel))
    return {
      id: wins.get(focusedLevel),
      label: `Watch Level ${focusedLevel + 1} finish`,
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
  watchRequested = true;
  worker.postMessage({ type: "states", ids: [witness.id], request: ++request });
};
function updateStats() {
  $("attempts").textContent = fmt(stats.executions);
  $("states").textContent = fmt(stats.states);
  $("cells").textContent = fmt(heat.cells.size);
  $("goal-title").textContent = gameWon
    ? "Game complete"
    : `World ${Math.floor(focusedLevel / 8) + 1} – Level ${focusedLevel % 8 + 1}${cleared.has(focusedLevel) ? " ✓" : ""}`;
  const witness = completionWitness();
  $("completion").hidden = !witness;
  if (witness) $("completion").textContent = witness.label;
  $("states").title =
    `Shared snapshot memory: ${fmt(stats.snapshot_bytes / 1048576)} MiB; ${budget.snapshotsMiB} MiB limit. Search memory: ${fmt(stats.wasm_bytes / 1048576)} MiB.`;
  $("work").textContent = fmt(stats.frames);
  $("memory").textContent = `${((stats.snapshot_bytes || 0) / 1000000).toFixed(1)} MB`;
  $("memory").title = "Retained snapshots across all search branches";
}
function renderMapRows(owner) {
  const focusedMap = document.activeElement?.closest(".map-row")?.dataset.map;
  const rooms = owner?.rooms || [mapLevel];
  for (const [id, image] of panoramas)
    if (!rooms.includes(id)) {
      image.src = "";
      panoramas.delete(id);
    }
  $("map-rows").replaceChildren(
    ...rooms.map((id) => {
      const row = document.createElement("section");
      row.className = "map-row";
      row.dataset.map = id;
      const label = document.createElement("button");
      label.textContent = maps.get(id).label;
      label.className = "area-label";
      label.setAttribute("aria-pressed", id === mapLevel);
      label.onclick = () => {
        userSelected = true;
        browseRoom(id);
      };
      const heading = document.createElement("div");
      heading.className = "area-heading";
      const zoomButton = document.createElement("button");
      zoomButton.className = "area-zoom";
      zoomButton.dataset.map = id;
      if (id === mapLevel) zoomButton.id = "zoom";
      zoomButton.setAttribute("aria-label", `Zoom ${maps.get(id).label}`);
      zoomButton.textContent = roomView(id).zoom === 1 ? "Zoom in" : `${roomView(id).zoom}×`;
      zoomButton.onclick = () => zoomRoom(id);
      heading.append(label, zoomButton);
      const parts = document.createElement("div");
      parts.className = "room-parts";
      const map = maps.get(id), panels = roomPanels(map.width, map.height, $("map-rows").clientWidth || innerWidth);
      parts.classList.toggle("vertical-room", panels[0].vertical && panels[0].width <= 384);
      row.dataset.parts = panels.length;
      panels.forEach((panel, index) => {
        const part = document.createElement("div");
        part.className = "room-part";
        const c = id === mapLevel && index === 0 ? canvas : document.createElement("canvas");
        c.className = "area-map";
        c.width = panel.width;
        c.height = panel.height;
        Object.assign(c.dataset, { map: id, panel: index, panelX: panel.x, panelY: panel.y,
          mapWidth: panel.width, mapHeight: panel.height, roomWidth: map.width, roomHeight: map.height });
        c.style.touchAction = "pan-y";
        c.tabIndex = 0;
        if (c !== canvas) bindMap(c);
        if (panels.length > 1) {
          const connection = document.createElement("div");
          connection.className = "room-continuation";
          connection.textContent = `${index + 1} / ${panels.length}`;
          const direction = panels[index + 1]?.x > panel.x ? "→" : panel.vertical ? "↓" : "→";
          connection.setAttribute("aria-label", `${map.label}, continuous ${panel.vertical ? "vertical" : "horizontal"} room, part ${index + 1} of ${panels.length}`);
          connection.dataset.direction = direction;
          const previous = document.createElement("button"), next = document.createElement("button");
          previous.textContent = panel.vertical ? "↑" : "↖";
          next.textContent = index === panels.length - 1 ? "—" : panel.vertical ? "↓" : "↘";
          previous.disabled = index === 0;
          next.disabled = index === panels.length - 1;
          previous.setAttribute("aria-label", "Previous section of this room");
          next.setAttribute("aria-label", "Next section of this room");
          const jump = offset => parts.children[index + offset]?.scrollIntoView({ block: "center", behavior: reducedMotion.matches ? "instant" : "smooth" });
          previous.onclick = () => jump(-1);
          next.onclick = () => jump(1);
          connection.prepend(previous);
          connection.append(next);
          part.append(connection);
        }
        part.append(c);
        parts.append(part);
      });
      row.append(heading, parts);
      panorama(id);
      return row;
    }),
  );
  if (focusedMap !== undefined) {
    const target =
      document.querySelector(`.map-row[data-map="${focusedMap}"] canvas`) ||
      canvas;
    target.focus({ preventScroll: true });
  }
}
function drawMap(now) {
  if (visualization !== "heat") swarmRooms = swarm.frame(activeSearch, heat.clock(now), reducedMotion.matches);
  for (const c of document.querySelectorAll(".area-map")) drawArea(c, now);
  $("map-hint").style.opacity = stats.executions > 25 ? "0" : "1";
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
  ctx.translate(canvas.width / 2, canvas.height / 2);
  const scale =
    Math.min(canvas.width / Number(canvas.dataset.mapWidth), canvas.height / Number(canvas.dataset.mapHeight)) * areaZoom;
  ctx.scale(scale, scale);
  ctx.translate(-areaCenter, -areaCenterY);
  const image = panorama(mapLevel);
  if (image?.complete && image.naturalWidth) ctx.drawImage(image, 0, 0);
  canvas.dataset.previewRoute = !preview && routeHover?.segments ? routeHover.id : "";
  canvas.dataset.zoom = areaZoom;
  canvas.dataset.centerX = areaCenter;
  canvas.dataset.centerY = areaCenterY;
  canvas.dataset.overlay = mode;
  const label = `${map.label} ${mode === "movement" ? "Nova movement" : mode === "both" ? "heatmap and Nova movement" : "heatmap"}. Click a cell to watch its history. Drag to move when zoomed. Arrow keys move the selection; Enter inspects a cell.`;
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
          y = cell.y * 32 - 8;
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
    for (let y = 24; y < mapHeight; y += 32) {
      ctx.beginPath();
      ctx.moveTo(0, y);
      ctx.lineTo(mapWidth, y);
      ctx.stroke();
    }
  }
  if (mode !== "heat")
    canvas.dataset.swarmCount = preview ? 0 : drawNovas(ctx, mapLevel, scale * (canvas.clientWidth / canvas.width));
  for (const p of preview || mode === "movement" ? [] : sparks) {
    if (p.level !== mapLevel) continue;
    const age = (now - p.time) / 1600;
    if (age > 1) continue;
    ctx.fillStyle = `rgba(255,239,183,${(1 - age) * 0.75})`;
    ctx.beginPath();
    ctx.arc(p.x, p.y - 8, 1.5 + (1 - age) * 1.5, 0, Math.PI * 2);
    ctx.fill();
  }
  canvas.dataset.tracePoints = drawTrail(ctx, mapLevel, scale, preview ? areaOrigin?.segments || [] : undefined);
  canvas.dataset.zoom = areaZoom;
  canvas.dataset.centerX = areaCenter;
  canvas.dataset.centerY = areaCenterY;
  const focus = preview ? null : hoverCell || selectedCell;
  if (focus?.level === mapLevel) {
    ctx.strokeStyle = "#fff0bd";
    ctx.lineWidth = 1.5 / areaZoom;
    ctx.strokeRect(focus.x * 32 + 1, focus.y * 32 - 8 + 1, 30, 30);
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
    y: Math.floor(((py - c.height / 2) / scale + view.y + 8) / 32),
    level: id,
  };
}
function bindMap(c) {
  let pinch;
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
  }, { passive: false });
  c.addEventListener("touchmove", (event) => {
    if (!pinch || event.touches.length !== 2) return;
    event.preventDefault();
    const id = Number(c.dataset.map), room = roomView(id, c), map = canvasPanel(c);
    const point = pair(event.touches), r = c.getBoundingClientRect();
    room.zoom = Math.max(1, Math.min(6, pinch.zoom * point.distance / Math.max(1, pinch.distance)));
    const scale = Math.min(c.width / map.width, c.height / map.height) * room.zoom;
    const center = panelCenter(map, room.zoom, {
      x: pinch.worldX - (point.x - r.left - r.width / 2) * c.width / r.width / scale,
      y: pinch.worldY - (point.y - r.top - r.height / 2) * c.height / r.height / scale,
    }, c);
    room.x = center.x; room.y = center.y;
    userSelected = dragged = true;
    document.querySelector(`.area-zoom[data-map="${id}"]`).textContent = room.zoom === 1 ? "Zoom in" : `${room.zoom.toFixed(1)}×`;
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
        dy = e.pointerType === "touch" ? 0 : e.clientY - pointerStart.y;
      if (Math.hypot(dx, dy) > 4) dragged = true;
      if (dragged) {
        userSelected = true;
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
    $("hover").hidden = false;
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
      map = maps.get(level);
    const point = (hoverCell?.level === level ? hoverCell : null) ||
      (selectedCell?.level === level ? selectedCell : null) || {
        level,
        x: Math.floor((Number(c.dataset.panelX) + 16) / 32),
        y: Math.floor((Number(c.dataset.panelY) + Math.min(168, c.height - 8)) / 32),
      };
    if (["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"].includes(e.key)) {
      e.preventDefault();
      hoverCell = {
        ...point,
        x: Math.max(
          0,
          Math.min(
            Math.floor(map.width / 32) - 1,
            point.x +
              (e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0),
          ),
        ),
        y: Math.max(
          0,
          Math.min(
            Math.ceil(map.height / 32) - 1,
            point.y +
              (e.key === "ArrowDown" ? 1 : e.key === "ArrowUp" ? -1 : 0),
          ),
        ),
      };
    }
    if (e.key.startsWith("Arrow") && hoverCell) {
      const next = mapCanvas(level, { x: hoverCell.x * 32 + 16, y: hoverCell.y * 32 + 8 });
      if (next && next !== c) { next.focus({ preventScroll: true }); next.scrollIntoView({ block: "nearest" }); }
    }
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
function zoomRoom(id) {
  userSelected = true;
  const view = roomView(id);
  view.zoom = view.zoom < 2 ? 2 : view.zoom < 4 ? 4 : 1;
  for (const c of document.querySelectorAll(`.map-row[data-map="${id}"] canvas`)) {
    const local = roomView(id, c);
    local.zoom = view.zoom;
    Object.assign(local, panelCenter(canvasPanel(c), local.zoom, local, c));
  }
  const point = markerPoint();
  if (point?.level === id) {
    const c = mapCanvas(id, point);
    Object.assign(roomView(id, c), panelCenter(canvasPanel(c), view.zoom, { x: point.x, y: point.y - 8 }, c));
  }
  document.querySelector(`.area-zoom[data-map="${id}"]`).textContent = view.zoom === 1 ? "Zoom in" : `${view.zoom}×`;
  drawMap(performance.now());
}
$("pause").onclick = () => {
  paused = !paused;
  worker.postMessage({ type: paused ? "pause" : "resume" });
  updateSearchControl();
  $("status").textContent = paused ? "Paused" : "Exploring";
  $("status-dot").className = paused ? "paused" : "";
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
      reset = seek(0),
      epoch = replayEpoch;
    if (!(await reset) || current !== state || epoch !== replayEpoch) return;
  }
  playing = true;
  frameCredit = 0;
  startAudio();
  $("play").textContent = "Ⅱ Pause replay";
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
$("credits").onclick = () =>
  showInfo(
    `<span class="eyebrow">CREDITS & LICENSING</span><h2>Nova the Squirrel</h2><p>Created by <a href="https://novasquirrel.com/">NovaSquirrel</a>. <a href="https://novasquirrel.itch.io/nova-the-squirrel">Play the original game</a>.</p><p>The game’s code is GPL-3.0-or-later. Its original graphics, sound and the gameplay imagery shown here are <a href="https://creativecommons.org/licenses/by-nc-sa/4.0/">CC BY-NC-SA 4.0</a>. This is a noncommercial software demonstration, not endorsed by NovaSquirrel. The original character designs and gameplay are preserved. Level panoramas are assembled from gameplay screenshots; browser maps add heat overlays. Original artwork is unchanged.</p><p>QuickNES library sources: LGPL-2.1-or-later. The compiled libretro browser core is distributed under GPL-2.0 terms with a GPL-2.0-or-later C++ shim. The separate Harmony browser interface and Rust search code: AGPL-3.0-or-later.</p><p><a href="licenses/CREDITS.md">Full credits and restrictions</a> · <a href="licenses/nova-source.tar.gz">Nova corresponding source</a> · <a href="licenses/quicknes-source.tar.gz">QuickNES source</a> · <a href="licenses/harmony-source.tar.gz">Harmony source</a> \u00b7 <a href="licenses/rust-dependencies.tar.gz">Rust dependency sources and notices</a> · <a href="https://github.com/pH14/harmony">Repository and build instructions</a></p>`,
  );
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
    trace = segments = [];
    historyVerified = false;
    ++replayEpoch;
    seeking = false;
    ++request;
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
  const soundParent = document.querySelector(!controlMode && compactReplay.matches ? ".replay-actions" : ".history-controls");
  if ($("sound").parentElement !== soundParent) {
    if (soundParent.classList.contains("history-controls")) soundParent.prepend($("sound"));
    else soundParent.append($("sound"));
  }
  $("goal-title").disabled = !ready || !engine || branchBusy || controlMode;
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
  for (const button of document.querySelectorAll(
    ".area-label,.area-zoom",
  ))
    button.disabled = branchBusy;
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
  $("cell-title").textContent = "Your branch";
  $("cell-visits").textContent = `From frame ${fmt(currentFrame)}`;
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
};
function leaveTourGameplay() {
  if (tour.open && tour.index === 4) tour.go(5);
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
  ++request;
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
    tourStep: tour.open ? tour.index : null,
    point: point.gap ? null : point,
    frame: branchFrame,
    segments: trailSegments(prefix),
  };
  updateBranchControls();
  $("branch-message").hidden = false;
  $("branch-message").textContent = "Starting search from this frame…";
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
      format: "harmony-nova-browser-v1",
      rom_sha256: ROM_SHA256,
      core_revision: CORE_REVISION,
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
  ++request;
  ++replayEpoch;
  current = null;
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
  trace = segments = [];
  historyVerified = false;
  ++request;
  updateSearchControl();
  updateBranchControls();
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
window.addEventListener("keydown", (e) => {
  if (e.code === "Escape") {
    if (controlMode) stopControl();
    else if (!$("inspector").hidden && !$("info").open) closeInspector();
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
  controller.release(e.code);
  if (controlMode && keyButtons[e.code]) e.preventDefault();
});
window.addEventListener("blur", stopControl);
document.addEventListener("visibilitychange", () => {
  if (document.hidden) {
    stopControl();
    music.stop();
    playing = false;
    $("play").textContent = "▶ Replay";
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
  const age = (now - originPulse.start) / 1200;
  if (age < 0 || age >= 1) return false;
  ctx.save();
  ctx.lineWidth = 2 / scale;
  for (const offset of reducedMotion.matches ? [0] : [0, 0.22]) {
    const phase = (age - offset) / (1 - offset);
    if (phase < 0) continue;
    ctx.strokeStyle = `rgba(255,245,204,${reducedMotion.matches ? 0.8 : 1 - phase})`;
    ctx.beginPath();
    ctx.arc(
      originPulse.point.x,
      originPulse.point.y - 8,
      (reducedMotion.matches ? 18 : 10 + 32 * phase) / scale,
      0,
      Math.PI * 2,
    );
    ctx.stroke();
  }
  ctx.restore();
  return true;
}
function drawTrail(ctx, level, scale, override) {
  const drawnSegments = override ?? ($("inspector").hidden
    ? branchOrigin?.segments || []
    : routeHover ? routeHover.segments || [] : segments);
  const hovering = !override && !!routeHover?.segments;
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
  ctx.restore();
  return count;
}

function animate(now) {
  const wallElapsed = lastTime ? now - lastTime : 0,
    elapsed = Math.min(100, wallElapsed);
  lastTime = now;
  if (now - lastMapPaint >= 1000 / 30) {
    drawMap(now);
    lastMapPaint = now;
  }
  $("film")
    .closest(".screen")
    .classList.toggle("updating", seeking && now - seekStarted > 120);
  if (controlMode && !seeking && current && !document.hidden) {
    frameCredit += (elapsed * 60) / 1000;
    const frames = Math.floor(frameCredit);
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
      if (currentFrame % traceStride === 0) recordTrail();
    }
    $("film").dataset.audioFrames = music.samples;
    current.frames = currentFrame;
    if (current.branch?.manual) current.branch.manual.to = currentFrame;
    current.observation = engine.observation();
    $("scrub").max = currentFrame;
    segments = trailSegments(trace);
    drawFilm();
  }
  if (playing && !seeking && current) {
    frameCredit += ((wallElapsed * 60) / 1000) * Number($("speed").value);
    const frames = Math.floor(frameCredit);
    frameCredit -= frames;
    if (frames) {
      advanceFilm(Math.min(current.frames, currentFrame + frames));
      $("film").dataset.audioFrames = music.samples;
      segments = trailSegments(trace);
      if (currentFrame === current.frames) {
        playing = false;
        music.stop();
        $("play").textContent = "↺ Replay again";
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
  return cell && mapCanvas(cell.level, { x: cell.x * 32 + 16, y: cell.y * 32 + 8 });
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
      (((cell.y * 32 - 8 - view.y) * scale + c.height / 2) * r.height) /
      c.height;
  return {
    element: c,
    left: Math.max(r.left, r.left + x),
    top: Math.max(r.top, r.top + y),
    right: Math.min(r.right, r.left + x + (32 * scale * r.width) / c.width),
    bottom: Math.min(r.bottom, r.top + y + (32 * scale * r.height) / c.height),
  };
}
function tourCell() {
  if (selectedCell?.ids.length) return selectedCell;
  return [...heat.cells.values()]
    .filter((c) => c.ids.length > 1 && c.x * 32 >= 96)
    .sort(
      (a, b) =>
        (b.level === mapLevel) - (a.level === mapLevel) ||
        b.x - a.x ||
        b.ids.length - a.ids.length ||
        b.visits - a.visits,
    )[0];
}
const tour = new GuidedTour({
  occluders: () => [$("inspector"), $("route-preview"), ...(document.body.classList.contains("tour-phone") && tour.index >= 5 ? [$("branches")] : [])],
  steps: [
    {
      reveal: () => tourMap(),
      title: "Welcome!",
      copy: [
        "Harmony is a system that searches and explores software in interesting ways. Video games are software, so let’s explore the delightful platformer, Nova the Squirrel for NES together.",
        "(Yes! A real NES emulator is now running in your browser.)",
      ],
      targets: () => [tourMap()],
      interactive: () => [$("map-rows"), $("pause")],
    },
    {
      reveal: () => tourMap(),
      title: "Exploration",
      copy: [
        "Watch as the density of the many little Nova-the-Squirrels shifts across the map as Harmony explores this level.",
        "Harmony explores a game, or software system, through a multiverse of possibilities. It plays tons of little actions in parallel and saves the ones that produced interesting results. It continuously adjusts its search frontier towards areas of the map that are more productive.",
      ],
      targets: () => [tourMap()],
      interactive: () => [$("map-rows"), $("pause")],
    },
    {
      reveal: () => tourMap(),
      title: "History",
      ping: () => tourCellRect(),
      copy: [
        "See all of the routes that Harmony has saved to lead to each part of the map. It will grow as the search continues.",
        "Harmony considers these saved states as starting points for future search attempts.",
      ],
      targets: () => [$("state-list"), ...(routeHover?.segments
        ? [...document.querySelectorAll(".area-map")].filter((canvas) => Number(canvas.dataset.tracePoints) > 0)
        : [tourCellRect()]), $("route-preview").hidden ? null : $("route-preview")],
      interactive: () => [$("state-list"), $("map-rows"), $("route-preview")],
    },
    {
      reveal: () => document.querySelector(`.map-row[data-map="${markerPoint()?.level ?? tourSession?.cell?.level}"] canvas`),
      title: "A singular timeline",
      copy: [
        "Look! We can observe any single timeline of Nova exploring the level. You can watch and rewind the gameplay for this one route if you’d like. I recommend it — the music rocks.",
        "Harmony allows you to view and explore any one timeline within its multiverse exploration at any moment.",
      ],
      targets: () => [
        document.querySelector(".transport"),
        compactReplay.matches ? null : $("sound"),
        document.querySelector(".screen"),
        ...[...document.querySelectorAll(".area-map")].filter((canvas) => Number(canvas.dataset.tracePoints) > 0),
      ],
      interactive: () => [document.querySelector(".transport"), $("sound"), $("state-list"), $("route-preview"), $("map-rows")],
    },
    {
      title: "🎮 Step into the experiment",
      copy: [
        "Want to play too? You can select any moment from this timeline and hop right in — right then, right there! Give it a shot.",
        "Harmony allows you to perform your own experiments that branch off any of its many timelines.",
      ],
      targets: () => [$("take-control").hidden ? $("game-controls") : $("take-control"), document.querySelector(".screen")],
      interactive: () => [$("inspector")],
    },
    {
      title: "To branch or not to branch",
      copy: [
        "Decide whether you want Harmony to start a new search fresh from where you just left off.",
        "Combining Harmony’s autonomous exploration with your input allows it to explore scenarios that might be challenging for just one of you to reach.",
      ],
      targets: () => [
        $("inspector").hidden ? null : document.querySelector(".branch-actions"),
        $("branches"),
        ...(controlMode ? [$("game-controls"), document.querySelector(".screen")] : []),
      ],
      interactive: () => [$("inspector"), $("branches")],
    },
    {
      title: "Searches",
      copy: [
        "Choose which branch of the search you want to have Harmony actively explore, or reset the whole exploration.",
        "Harmony organizes many searches together, showing how each branch relates to its parent.",
        "Now, go have fun!",
      ],
      targets: () => [$("branches"), ...(branchPreview ? [...document.querySelectorAll(".area-map")] : [])],
      interactive: () => [$("branches")],
    },
  ],
  onStart() {
    if ($("workspace").classList.contains("timeline-collapsed")) $("timeline-toggle").click();
    if (visualization !== "movement") setVisualization("movement");
    tourSession = {
      cell: tourCell(),
      wasPaused: paused,
      prepared: false,
      previousManual: String(current?.id).startsWith("manual-")
        ? current
        : null,
      previousFrame: currentFrame,
      interacted: false,
    };
    userSelected = true;
    playing = false;
    music.stop();
    $("play").textContent = "▶ Replay";
  },
  async beforeStep(index, valid) {
    hideRoutePreview();
    if (index <= 3) expandInspector(false);
    if (controlMode && index !== 4 && index !== 5) stopControl();
    if (index === 6) return;
    if (index !== 3) {
      playing = false;
      if (!controlMode) music.stop();
      $("play").textContent = "▶ Replay";
    }
    if (index === 5 && tourSession.gameplay) {
      paused = !!playSession;
      worker.postMessage({ type: paused ? "pause" : "resume" });
      updateSearchControl();
      followReplayRoom(markerPoint());
      return;
    }
    if (index === 2 && !tourSession.prepared) tourSession.cell = tourCell();
    const cell = (tourSession.cell ||= tourCell());
    if (!cell) throw new Error("No retained cell yet");
    if (index <= 2 && mapLevel !== cell.level) setRoom(cell.level, true);
    if (index >= 3) followReplayRoom(markerPoint());
    const view = roomView(cell.level, tourMap());
    if (index <= 2 && view.zoom > 1) {
      view.x = cell.x * 32 + 16;
      view.y = cell.y * 32 + 8;
    }
    if (index <= 1) {
      if (!tourSession.interacted && !tourSession.wasPaused && !stats.stopped && paused) {
        paused = false;
        worker.postMessage({ type: "resume" });
        updateSearchControl();
      }
      return;
    }
    paused = true;
    worker.postMessage({ type: "pause" });
    updateSearchControl();
    if (!tourSession.prepared || !current || $("inspector").hidden) {
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
    if (index === 2)
      $("state-list")
        .querySelector(".selected")
        ?.scrollIntoView({ block: "nearest", behavior: "instant" });
    if (index >= 5 && !document.body.classList.contains("tour-phone"))
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
      ++request;
      selectedCell = null;
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
    stats.executions >= 30 &&
    tourCell()
  )
    beginTour();
}
$("tour-open").onclick = beginTour;
$("goal-title").onclick = () => {
  const wasPlaying = playing;
  if (wasPlaying) { playing = false; music.stop(); $("play").textContent = "▶ Replay"; }
  $("level-worlds").replaceChildren();
  for (const world of [1, 2, 3, 4, 5, 6]) {
    const section = document.createElement("section"), heading = document.createElement("h3"), grid = document.createElement("div");
    heading.textContent = world === 6 ? "Bonus levels" : `World ${world}`;
    grid.className = "level-grid";
    for (const level of catalog.levels.filter((entry) => entry.world === world)) {
      const card = document.createElement("button"), image = document.createElement("img"), caption = document.createElement("span");
      card.className = "level-card";
      card.dataset.level = level.id;
      card.setAttribute("aria-label", `World ${world} – Level ${level.id % 8 + 1}`);
      card.setAttribute("aria-current", level.id === bootLevel ? "true" : "false");
      image.src = new URL(`maps/${level.rooms[0]}-preview.png`, base).href; image.loading = "lazy"; image.alt = "";
      caption.textContent = `Level ${level.id % 8 + 1}`;
      card.append(image, caption);
      card.onclick = () => {
        $("level-picker").close();
        tour.finish();
        ++seed;
        startSearch(level.id);
        $("goal-title").focus({ preventScroll: true });
      };
      grid.append(card);
    }
    section.append(heading, grid);
    $("level-worlds").append(section);
  }
  $("level-picker").showModal();
};
$("close-level-picker").onclick = () => $("level-picker").close();
requestAnimationFrame(animate);
createEngine(base)
  .then((e) => {
    engine = e;
    origin = e.boot(bootLevel);
    originPixels = e.pixels();
    drawFilm();
    $("verification").textContent = "Original game";
    const state = stateCache.get(0);
    if (state) selectState(state, true).catch(fail);
  })
  .catch(fail);
startSearch();
