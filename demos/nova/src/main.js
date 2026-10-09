// SPDX-License-Identifier: AGPL-3.0-or-later
import "./style.css";
import { GuidedTour, tourSeen } from "./tour.js";
import { ReplayTimeline, prefixTrail, trailPoint } from "./replay.js";
import { GameAudio } from "./audio.js";
import { createEngine, ROM_SHA256, CORE_REVISION } from "./emulator.js";
import { Heatmap, routeIds } from "./heat.js";
import { CREDIT, snapshotHash } from "./media.js";
import { viewCenter } from "./view.js";
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
const catalog = await (await fetch(new URL("maps.json", base))).json();
const maps = new Map(catalog.maps.map((map) => [map.id, map]));
const artCredit = `<small class="art-credit"><a href="${CREDIT.source}">${CREDIT.title}</a> art by <a href="https://novasquirrel.com/">${CREDIT.author}</a> \u00b7 <a href="${CREDIT.license_url}" rel="license">${CREDIT.license}</a></small>`;
document.querySelector("#app").innerHTML = `
<header><a class="brand" href="https://github.com/pH14/harmony"><b>harmony</b></a><span class="divider">/</span><span>Nova explorer</span><button id="tour-open" disabled>Guided tour</button></header>
<main><div class="workspace" id="workspace"><section class="exploration" aria-label="Live exploration"><div class="toolbar"><div class="controls"><span id="status" hidden>Loading Nova…</span><i id="status-dot" hidden></i><button id="pause" class="icon-button" aria-label="Pause Search" title="Pause Search" disabled></button><button id="reset" disabled>Restart Search</button><label class="branch-picker">Search branch<select id="branch-choice" aria-label="Search branch" disabled><option value="0">Original search</option></select></label><span id="branch-feedback" class="visually-hidden" role="status"></span></div><div class="metrics"><div><b id="attempts">0</b><span>paths explored</span></div><div><b id="states">0</b><span>states retained</span></div><div><b id="cells">0</b><span>cells visited</span></div><div><b id="distance">0%</b><span>furthest into this area</span></div><div><b id="work">0</b><span>game frames executed</span></div></div></div>
<div class="goal"><div><strong id="goal-title">Level 1</strong><span id="goal-status" hidden></span></div><div><button id="completion" hidden>Watch completion</button></div></div>
<nav id="room-tabs" aria-label="Areas in this level" hidden></nav>
<div class="map-wrap"><div id="map-rows"></div><canvas id="map" width="1280" height="320" tabindex="0" aria-label="Game area heatmap. Drag to move when zoomed. Arrow keys move the selection; Enter inspects a cell."></canvas><span class="map-label" id="map-label" hidden>INTRODUCTION</span><div id="map-hint">Click a warm cell to watch its history</div><div id="hover" hidden></div></div>
<div class="map-footer"><span>Recent activity <span class="gradient"></span><span class="legend">cold → busy</span></span><span id="memory-limit" hidden></span></div>
${artCredit}</section>
<section class="inspect inspector" id="inspector" aria-label="History inspector" hidden><div class="drawer-top"><h2 id="film-title">History</h2><div><button id="expand-inspector" aria-expanded="false">Expand</button><button id="close-inspector" aria-label="Close history inspector">×</button></div></div><div class="film"><span id="verification" hidden>Starting emulator</span><div class="screen"><canvas id="film" tabindex="0" width="256" height="224" aria-label="Nova gameplay replay"></canvas><span id="frame-label">FRAME 0</span></div>${artCredit}<div class="transport"><button id="play" disabled>▶ Play history</button><input id="scrub" aria-label="Replay frame" type="range" min="0" max="0" value="0" disabled><select id="speed" aria-label="Playback speed"><option value="1" selected>1×</option><option value="4">4×</option><option value="12">12×</option></select><button id="sound" class="icon-button" aria-label="Mute game audio" title="Mute game audio" aria-pressed="false"></button></div><div class="branch-actions"><button id="take-control" aria-describedby="branch-hint" disabled>🎮 Play from here</button><p id="branch-hint">Your original search stays in the branch menu.</p><button id="search-here" aria-describedby="branch-hint" disabled>↗ Branch search from here</button></div><div id="branch-message" role="status" hidden></div><div id="game-controls" hidden><small>Move ←↑↓→ / WASD · Jump Z / Space · Ability X</small><div class="touch-controls" aria-label="Game controller"><div class="dpad"><button data-button="16" aria-label="Move up">↑</button><button data-button="64" aria-label="Move left">←</button><button data-button="32" aria-label="Move down">↓</button><button data-button="128" aria-label="Move right">→</button></div><button data-button="2" aria-label="Use ability">B</button><button data-button="1" aria-label="Jump">A</button></div></div></div>
<aside class="state-picker" aria-label="Retained histories"><div class="section-title"><h2 id="cell-title">Retained history</h2><span id="cell-visits">Live</span></div><p id="selection-hint" hidden></p><div id="state-list"></div><details class="state-disclosure"><summary>Game state</summary><div id="details" class="details"></div></details></aside></section></div>
<section class="atlas"><div class="section-title"><h2>The game</h2><nav id="worlds" aria-label="Game worlds"></nav></div><div id="atlas" class="atlas-grid"></div>${artCredit}</section>
<footer><span>Nova the Squirrel by <a href="https://novasquirrel.com/">NovaSquirrel</a> · Original game artwork <a href="https://creativecommons.org/licenses/by-nc-sa/4.0/">CC BY-NC-SA 4.0</a></span><button id="credits">Credits & source</button></footer>
<div id="error" role="alert" hidden></div>
<dialog id="info"><button id="close-info" class="close" aria-label="Close">×</button><div id="info-content"></div></dialog></main>`;
let heat = new Heatmap(),
  worker,
  engine,
  origin,
  originPixels,
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
  focusedWorld = 1,
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
let controlMode = false,
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
    stats: {},
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
  hoverCell = selectedCell = null;
  lastAuto = 0;
  return fresh;
}
function revealBranchOrigin() {
  const point = branchOrigin?.point;
  if (!point) return;
  setRoom(point.level, true);
  const view = roomView(point.level),
    row = document.querySelector(`.map-row[data-map="${point.level}"]`),
    canvas = row?.querySelector("canvas");
  if (!canvas) return;
  if (view.zoom > 1) {
    const center = viewCenter(
      maps.get(point.level).width,
      view.zoom,
      { x: point.x, y: point.y - 8 },
      maps.get(point.level).height,
      canvas,
    );
    view.x = center.x;
    view.y = center.y;
  }
  const bounds = row.getBoundingClientRect();
  if (bounds.top < 20 || bounds.bottom > innerHeight - 20)
    window.scrollBy({ top: bounds.top - 24, behavior: "instant" });
}
function roomView(id) {
  if (!views.has(id)) {
    const map = maps.get(id);
    views.set(id, { zoom: 1, x: map.width / 2, y: map.height / 2 });
  }
  return views.get(id);
}
const budget = memoryBudget(
  navigator.deviceMemory,
  matchMedia("(pointer: coarse)").matches,
);
const timeline = new ReplayTimeline(
  (budget.snapshotsMiB === 32 ? 2 : 4) * 1048576,
);
const music = new GameAudio(() => createEngine(base));
const panoramas = new Map(),
  thumbnails = new Map();
let thumbnailQueue = Promise.resolve();
function panorama(level) {
  if (!panoramas.has(level)) {
    const image = new Image();
    image.src = new URL(maps.get(level).file, base).href;
    panoramas.set(level, image);
  }
  return panoramas.get(level);
}
function thumbnail(level) {
  if (thumbnails.has(level)) return;
  const thumb = document.createElement("canvas");
  thumb.width = 320;
  thumb.height = 96;
  thumbnails.set(level, thumb);
  thumbnailQueue = thumbnailQueue.then(async () => {
    const image = new Image();
    await new Promise((resolve) => {
      image.onload = image.onerror = resolve;
      image.src = new URL(maps.get(level).file, base).href;
    });
    if (image.naturalWidth) {
      const scale = Math.min(
        320 / image.naturalWidth,
        96 / image.naturalHeight,
      );
      const context = thumb.getContext("2d");
      context.imageSmoothingEnabled = false;
      context.drawImage(
        image,
        (320 - image.naturalWidth * scale) / 2,
        (96 - image.naturalHeight * scale) / 2,
        image.naturalWidth * scale,
        image.naturalHeight * scale,
      );
    }
    image.src = "";
    drawAtlas(performance.now());
  });
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
  if (!point || !userSelected || $("inspector").hidden) return;
  const changed = point.level !== mapLevel;
  if (changed) setRoom(point.level, true);
  const view = roomView(point.level),
    row = document.querySelector(`.map-row[data-map="${point.level}"]`),
    c = row?.querySelector("canvas");
  if (!c) return;
  if (view.zoom > 1) {
    const next = viewCenter(
      maps.get(point.level).width,
      view.zoom,
      { x: point.x, y: point.y - 8 },
      maps.get(point.level).height,
      c,
    );
    view.x = next.x;
    view.y = next.y;
  }
  const phone = matchMedia("(max-width: 800px)").matches;
  if (changed || (phone && revealReplayRoom)) {
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
      $("play").textContent = "▶ Play history";
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
  $("film-title").textContent = routeName(state.id);
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
    $("play").textContent = "Ⅱ Pause film";
  }
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
    $("film-title").textContent = routeName(current.id);
    $("film-title").dataset.stateId = current.id;
    $("film-title").title =
      `State #${current.id} · ${fmt(current.frames)} frames`;
  }
  const ids = selectedCell
    ? routeIds(selectedCell, current?.id)
    : [current?.id].filter((x) => x !== undefined);
  $("cell-title").textContent = selectedCell
    ? "Routes to this spot"
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
      button.onclick = () => {
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
  $("film-title").textContent = "Loading history…";
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
function startSearch() {
  stopControl();
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
  $("branch-choice").replaceChildren(new Option("Original search", "0"));
  $("branch-message").hidden = true;
  if (worker) worker.terminate();
  heat = new Heatmap();
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
  setRoom(0);

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
        ready = true;
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
        ++request;
        selectedCell = hoverCell = null;
        updateRoomTabs();
        paused = data.paused;
        branchBusy = false;
        $("branch-message").hidden = true;
        $("branch-choice").replaceChildren(
          ...data.searches.map((s) => {
            const option = document.createElement("option");
            option.value = s.id;
            option.textContent = s.label;
            return option;
          }),
        );
        $("branch-choice").value = activeSearch;
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
          updateRoomTabs();
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
          $("branch-choice").focus({ preventScroll: true });
        }
        if (engine) selectState(state, initial).catch(fail);
        updateStats();
      } else if (data.type === "batch") {
        if (data.active !== activeSearch) return;
        stats = data;
        gameWon ||= data.won;
        const now = performance.now();
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
          heat.visit({ ...point, observation: project(o, map) }, now);
          sparks.push({ ...project(o, map), time: now });
          if (followDiscovery(o, first, focusedLevel, userSelected, cleared))
            setRoom(o.level);
          if (first) updateRoomTabs();
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
        if (data.request === 0) {
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
  worker.postMessage({ type: "init", base: base.href, seed, budget });
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
  focusedWorld = owner?.world ?? 1;
  $("map-label").textContent = map.label.toUpperCase();
  renderNavigation();
  updateStats();
  panorama(level);
}
function browseRoom(level) {
  stopControl();
  music.stop();
  playing = false;
  $("play").textContent = "▶ Play history";
  setRoom(level);
  const id = bestStates.get(level);
  if (id !== undefined) {
    watchRequested = false;
    worker.postMessage({ type: "states", ids: [id], request: ++request });
  }
}
function updateRoomTabs() {
  for (const button of $("room-tabs").children) {
    const id = Number(button.dataset.map);
    button.textContent =
      maps.get(id).label + (seenMaps.has(id) ? " \u2713" : "");
  }
}
function renderNavigation() {
  const owner = catalog.levels.find((l) => l.id === focusedLevel);
  $("room-tabs").replaceChildren(
    ...(owner?.rooms || [mapLevel]).map((id) => {
      const button = document.createElement("button");
      button.textContent =
        maps.get(id).label + (seenMaps.has(id) ? " \u2713" : "");
      button.dataset.map = id;
      button.className = id === mapLevel ? "selected" : "";
      button.setAttribute("aria-pressed", id === mapLevel);
      button.onclick = () => {
        userSelected = true;
        browseRoom(id);
      };
      return button;
    }),
  );
  $("worlds").replaceChildren(
    ...[1, 2, 3, 4, 5, 6].map((world) => {
      const button = document.createElement("button");
      button.textContent = world === 6 ? "Bonus" : `World ${world}`;
      button.className = world === focusedWorld ? "selected" : "";
      button.setAttribute("aria-pressed", world === focusedWorld);
      button.onclick = () => {
        userSelected = true;
        stopControl();
        music.stop();
        playing = false;
        $("play").textContent = "▶ Play history";
        focusedWorld = world;
        renderNavigation();
      };
      return button;
    }),
  );
  $("atlas").replaceChildren(
    ...catalog.levels
      .filter((l) => l.world === focusedWorld)
      .map((level) => {
        const article = document.createElement("article");
        article.className = "level-card";
        article.dataset.level = level.id;
        const title = document.createElement("h3");
        title.textContent =
          level.world === 6
            ? `Bonus ${level.id - 39}`
            : `Level ${level.id + 1}`;
        article.append(title);
        for (const id of level.rooms) {
          const button = document.createElement("button");
          button.className = "map-card" + (id === mapLevel ? " selected" : "");
          button.dataset.map = id;
          button.setAttribute(
            "aria-label",
            `View ${maps.get(id).label}, level ${level.id + 1}`,
          );
          const thumb = document.createElement("canvas");
          thumb.width = 320;
          thumb.height = 96;
          const label = document.createElement("b");
          label.textContent = maps.get(id).label;
          button.append(thumb, label);
          button.onclick = () => {
            userSelected = true;
            browseRoom(id);
          };
          article.append(button);
          thumbnail(id);
        }
        return article;
      }),
  );
  renderMapRows(owner);
  drawAtlas(performance.now());
}
function drawAtlas(now) {
  for (const card of document.querySelectorAll(".map-card")) {
    const id = Number(card.dataset.map),
      map = maps.get(id),
      c = card.querySelector("canvas"),
      context = c.getContext("2d"),
      image = thumbnails.get(id),
      scale = Math.min(c.width / map.width, c.height / map.height);
    context.imageSmoothingEnabled = false;
    context.fillStyle = "#211f1c";
    context.fillRect(0, 0, c.width, c.height);
    context.save();
    context.translate(
      (c.width - map.width * scale) / 2,
      (c.height - map.height * scale) / 2,
    );
    context.scale(scale, scale);
    if (image) {
      context.restore();
      context.drawImage(image, 0, 0);
      context.save();
      context.translate(
        (c.width - map.width * scale) / 2,
        (c.height - map.height * scale) / 2,
      );
      context.scale(scale, scale);
    }
    context.fillStyle = "rgba(7,24,43,.6)";
    context.fillRect(0, 0, map.width, map.height);
    for (const cell of heat.cells.values())
      if (cell.level === id) {
        const color = heat.color(cell, now);
        if (color) {
          context.fillStyle = `rgba(${color},.65)`;
          context.fillRect(cell.x * 32, cell.y * 32 - 8, 32, 32);
        }
      }
    context.restore();
  }
  for (const article of document.querySelectorAll(".level-card")) {
    const id = Number(article.dataset.level);
    article.classList.toggle("cleared", cleared.has(id));
    article.querySelector("h3").textContent =
      `${id >= 40 ? "Bonus " + (id - 39) : "Level " + (id + 1)}${cleared.has(id) ? " ✓ Cleared" : ""}`;
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
  $("distance").textContent =
    Math.min(
      100,
      Math.round(
        ((bestByMap.get(mapLevel) || 0) / maps.get(mapLevel).runtimeWidth) *
          100,
      ),
    ) + "%";
  $("goal-title").textContent = gameWon
    ? "Game complete"
    : `Level ${Math.min(focusedLevel + 1, 40)}${cleared.has(focusedLevel) ? " ✓" : ""}`;
  const witness = completionWitness();
  $("completion").hidden = !witness;
  if (witness) $("completion").textContent = witness.label;
  $("states").title =
    `${fmt(stats.snapshot_bytes / 1048576)} MiB compressed snapshots; ${budget.snapshotsMiB} MiB limit. Search memory: ${fmt(stats.wasm_bytes / 1048576)} MiB.`;
  drawAtlas(performance.now());
  $("work").textContent = fmt(stats.frames);
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
      const c = id === mapLevel ? canvas : document.createElement("canvas");
      c.className = "area-map";
      c.width = 1280;
      c.height =
        roomView(id).zoom > 1
          ? 320
          : Math.min(
              320,
              Math.max(
                128,
                Math.round((1280 / maps.get(id).width) * maps.get(id).height),
              ),
            );
      c.dataset.map = id;
      c.dataset.mapWidth = maps.get(id).width;
      c.dataset.mapHeight = maps.get(id).height;
      c.style.touchAction = roomView(id).zoom > 1 ? "none" : "pan-y";
      const heading = document.createElement("div");
      heading.className = "area-heading";
      const zoomButton = document.createElement("button");
      zoomButton.className = "area-zoom";
      zoomButton.dataset.map = id;
      if (id === mapLevel) zoomButton.id = "zoom";
      zoomButton.setAttribute("aria-label", `Zoom ${maps.get(id).label}`);
      zoomButton.textContent =
        roomView(id).zoom === 1 ? "Zoom in" : `${roomView(id).zoom}×`;
      zoomButton.onclick = () => zoomRoom(id);
      heading.append(label, zoomButton);
      if (c !== canvas) {
        c.tabIndex = 0;
        c.setAttribute(
          "aria-label",
          `${maps.get(id).label} heatmap. Click a warm cell to replay.`,
        );
        bindMap(c);
      }
      row.append(heading, c);
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
  for (const c of document.querySelectorAll(".area-map")) drawArea(c, now);
  $("map-hint").style.opacity = stats.executions > 25 ? "0" : "1";
}
function drawArea(canvas, now) {
  const ctx = canvas.getContext("2d"),
    mapLevel = Number(canvas.dataset.map),
    map = maps.get(mapLevel),
    mapWidth = map.width,
    mapHeight = map.height,
    view = roomView(mapLevel),
    areaZoom = view.zoom,
    areaCenter = view.x,
    areaCenterY = view.y;
  ctx.imageSmoothingEnabled = false;
  ctx.fillStyle = "#122b42";
  ctx.fillRect(0, 0, canvas.width, canvas.height);
  ctx.save();
  ctx.translate(canvas.width / 2, canvas.height / 2);
  const scale =
    Math.min(canvas.width / mapWidth, canvas.height / mapHeight) * areaZoom;
  ctx.scale(scale, scale);
  ctx.translate(-areaCenter, -areaCenterY);
  const image = panorama(mapLevel);
  if (image?.complete && image.naturalWidth) ctx.drawImage(image, 0, 0);
  ctx.fillStyle = "rgba(7,24,43,.62)";
  ctx.fillRect(0, 0, mapWidth, mapHeight);
  {
    ctx.fillStyle = "rgba(65,113,162,.11)";
    ctx.fillRect(0, 0, mapWidth, mapHeight);
    for (const cell of heat.cells.values()) {
      if (cell.level !== mapLevel) continue;
      const color = heat.color(cell, now);
      if (!color) continue;
      const x = cell.x * 32,
        y = cell.y * 32 - 8;
      ctx.fillStyle = `rgba(${color},.48)`;
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
  for (const p of sparks) {
    if (p.level !== mapLevel) continue;
    const age = (now - p.time) / 1600;
    if (age > 1) continue;
    ctx.fillStyle = `rgba(255,239,183,${(1 - age) * 0.75})`;
    ctx.beginPath();
    ctx.arc(p.x, p.y - 8, 1.5 + (1 - age) * 1.5, 0, Math.PI * 2);
    ctx.fill();
  }
  canvas.dataset.tracePoints = drawTrail(ctx, mapLevel, scale);
  canvas.dataset.zoom = areaZoom;
  canvas.dataset.centerX = areaCenter;
  canvas.dataset.centerY = areaCenterY;
  const focus = hoverCell || selectedCell;
  if (focus?.level === mapLevel) {
    ctx.strokeStyle = "#fff0bd";
    ctx.lineWidth = 1.5 / areaZoom;
    ctx.strokeRect(focus.x * 32 + 1, focus.y * 32 - 8 + 1, 30, 30);
  }
  const o = $("inspector").hidden
    ? branchOrigin?.point
    : current
      ? markerPoint()
      : null;
  const mapOrigin =
    branchOrigin?.point?.level === mapLevel ? branchOrigin : null;
  canvas.dataset.originFrame = mapOrigin?.frame ?? "";
  canvas.dataset.originPulse = String(
    drawOriginPulse(ctx, mapLevel, scale, now),
  );
  canvas.dataset.markerFrame =
    o?.level !== mapLevel
      ? ""
      : $("inspector").hidden
        ? branchOrigin.frame
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
    map = maps.get(id),
    rect = c.getBoundingClientRect(),
    px = ((e.clientX - rect.left) / rect.width) * c.width,
    py = ((e.clientY - rect.top) / rect.height) * c.height,
    view = roomView(id),
    scale = Math.min(c.width / map.width, c.height / map.height) * view.zoom;
  return {
    x: Math.floor(((px - c.width / 2) / scale + view.x) / 32),
    y: Math.floor(((py - c.height / 2) / scale + view.y + 8) / 32),
    level: id,
  };
}
function bindMap(c) {
  c.addEventListener("pointerdown", (e) => {
    const view = roomView(Number(c.dataset.map));
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
      map = maps.get(id),
      room = roomView(id);
    if (pointerStart?.canvas === c && e.buttons && room.zoom > 1) {
      const dx = e.clientX - pointerStart.x,
        dy = e.clientY - pointerStart.y;
      if (Math.hypot(dx, dy) > 4) dragged = true;
      if (dragged) {
        userSelected = true;
        const rect = c.getBoundingClientRect(),
          scale =
            Math.min(c.width / map.width, c.height / map.height) * room.zoom;
        const view = viewCenter(
          map.width,
          room.zoom,
          {
            x: pointerStart.center - (dx * c.width) / rect.width / scale,
            y: pointerStart.centerY - (dy * c.height) / rect.height / scale,
          },
          map.height,
          { width: c.width, height: c.height },
        );
        room.x = view.x;
        room.y = view.y;
        return;
      }
    }
    hoverCell = mapCoordinates(e);
    const cell = heat.cells.get(
      `${hoverCell.level}:${hoverCell.x}:${hoverCell.y}`,
    );
    $("hover").hidden = false;
    $("hover").textContent =
      `${cell?.visits || 0} visits · ${cell?.ids.length || 0} states`;
  });
  c.addEventListener("pointerleave", (e) => {
    if (c.hasPointerCapture(e.pointerId)) return;
    pointerStart = null;
    hoverCell = null;
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
    inspect(cell || { ...point, visits: 0, ids: [] });
  });
  c.addEventListener("keydown", (e) => {
    const level = Number(c.dataset.map),
      map = maps.get(level);
    const point = (hoverCell?.level === level ? hoverCell : null) ||
      (selectedCell?.level === level ? selectedCell : null) || {
        level,
        x: 1,
        y: 5,
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
    if (e.key === "Enter") {
      e.preventDefault();
      if (level !== mapLevel) setRoom(level);
      hoverCell = point;
      inspect(
        heat.cells.get(`${level}:${point.x}:${point.y}`) || {
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
  const map = maps.get(id),
    view = roomView(id),
    c = document.querySelector(`.map-row[data-map="${id}"] canvas`);
  view.zoom = view.zoom === 1 ? 2 : view.zoom === 2 ? 4 : 1;
  c.height =
    view.zoom > 1
      ? 320
      : Math.min(
          320,
          Math.max(128, Math.round((1280 / map.width) * map.height)),
        );
  c.style.touchAction = view.zoom > 1 ? "none" : "pan-y";
  const o =
    frameObservation?.level === id &&
    isMapEvidence(frameObservation, catalog.levels)
      ? project(frameObservation, map)
      : null;
  const point = o
    ? { x: o.x, y: o.y - 8 }
    : selectedCell?.level === id
      ? { x: selectedCell.x * 32 + 16, y: selectedCell.y * 32 + 8 }
      : { x: view.x, y: view.y };
  const next = viewCenter(map.width, view.zoom, point, map.height, c);
  view.x = next.x;
  view.y = next.y;
  document.querySelector(`.area-zoom[data-map="${id}"]`).textContent =
    view.zoom === 1 ? "Zoom in" : `${view.zoom}×`;
}
$("pause").onclick = () => {
  paused = !paused;
  worker.postMessage({ type: paused ? "pause" : "resume" });
  updateSearchControl();
  $("status").textContent = paused ? "Paused · heat cooling" : "Exploring";
  $("status-dot").className = paused ? "paused" : "";
};
$("reset").onclick = () => {
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
    $("play").textContent = "▶ Play history";
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
  $("play").textContent = "Ⅱ Pause film";
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
function openInspector() {
  if ($("inspector").hidden) {
    inspectorReturnFocus = document.activeElement;
    revealReplayRoom = true;
  }
  $("inspector").hidden = false;
  $("workspace").classList.add("inspect-open");
}
function closeInspector({ restoreFocus = true } = {}) {
  stopControl();
  music.stop();
  playing = false;
  $("play").textContent = "▶ Play history";
  $("inspector").hidden = true;
  $("workspace").classList.remove("inspect-open");
  if (restoreFocus && inspectorReturnFocus?.isConnected)
    inspectorReturnFocus.focus({ preventScroll: true });
}
function updateBranchControls() {
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
  $("search-here").disabled = !usable;
  $("take-control").textContent = controlMode
    ? "🎮 Stop playing"
    : "🎮 Play from here";
  $("search-here").textContent = "↗ Branch search from here";
  $("take-control").setAttribute("aria-pressed", controlMode);
  $("game-controls").hidden = !controlMode;
  $("scrub").disabled = !current || controlMode || branchBusy;
  $("play").disabled = !current || controlMode || branchBusy || seeking;
  $("branch-choice").disabled =
    branchBusy || controlMode || $("branch-choice").options.length < 2;
  $("sound").disabled = !current;
  $("pause").disabled = !ready || branchBusy || controlMode || !!stats.stopped;
  $("inspector").classList.toggle("controlling", controlMode);
  for (const button of $("state-list").children)
    button.disabled = controlMode || branchBusy;
  for (const button of document.querySelectorAll(
    ".area-label,.area-zoom,.map-card,#worlds button",
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
$("close-inspector").onclick = closeInspector;
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
  if (controlMode) {
    stopControl();
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
  current = {
    id: `manual-${++manualId}`,
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
  $("film-title").textContent = "Your branch";
  $("branch-message").hidden = true;
  controller.clear();
  controlMode = true;
  if (matchMedia("(max-width: 800px)").matches) expandInspector(true);
  timeline.trim(currentFrame);
  startAudio();
  $("play").textContent = "▶ Play history";
  renderStates();
  updateBranchControls();
  $("film").focus({ preventScroll: true });
};
$("search-here").onclick = async () => {
  if (!current || seeking || branchBusy || !frameObservation?.health) return;
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
$("branch-choice").onchange = () => {
  closeInspector();
  userSelected = true;
  paused = true;
  branchBusy = true;
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
    active: Number($("branch-choice").value),
  });
};
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
    e.target.closest("input,select,[contenteditable=true]")
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
    $("play").textContent = "▶ Play history";
  }
});
for (const button of document.querySelectorAll(".touch-controls button")) {
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
function drawTrail(ctx, level, scale) {
  const drawnSegments = $("inspector").hidden
    ? branchOrigin?.segments || []
    : segments;
  let count = 0;
  ctx.save();
  ctx.lineJoin = "round";
  ctx.lineCap = "round";
  ctx.strokeStyle = "rgba(255,218,142,.85)";
  ctx.lineWidth = 1.8 / scale;
  for (const segment of drawnSegments) {
    if (segment[0]?.level !== level) continue;
    count += segment.length;
    ctx.beginPath();
    segment.forEach((p, i) =>
      i ? ctx.lineTo(p.x, p.y - 8) : ctx.moveTo(p.x, p.y - 8),
    );
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
  if (Math.floor(now / 250) !== Math.floor((now - elapsed) / 250))
    drawAtlas(now);
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
        $("play").textContent = "↺ Play again";
        verify().then(updateBranchControls).catch(replayError);
      }
    }
  }
  requestAnimationFrame(animate);
}
let tourSession = null,
  tourOffered = tourSeen();
function tourMap() {
  return document.querySelector(
    `.map-row[data-map="${tourSession?.cell?.level}"] canvas`,
  );
}
function tourCellRect() {
  const c = tourMap(),
    cell = tourSession?.cell;
  if (!c || !cell) return null;
  const r = c.getBoundingClientRect(),
    map = maps.get(cell.level),
    view = roomView(cell.level),
    scale = Math.min(c.width / map.width, c.height / map.height) * view.zoom,
    x = (((cell.x * 32 - view.x) * scale + c.width / 2) * r.width) / c.width,
    y =
      (((cell.y * 32 - 8 - view.y) * scale + c.height / 2) * r.height) /
      c.height;
  return {
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
  steps: [
    {
      title: "Watch the search explore",
      copy: "Each warm cell marks real search activity. Green, orange and red show where exploration is concentrated. Activity cools as the search moves on; visited ground stays blue.",
      targets: () => [tourMap()],
    },
    {
      title: "One spot, many histories",
      copy: "Click a cell to inspect the states retained there. We’ve opened a real one: each route is a different history that brought Nova to this spot.",
      targets: () => [$("state-list"), tourCellRect()],
    },
    {
      title: "Follow one route",
      copy: "Choose a route to trace its path in gold. Play its history, or scrub to any frame. The game reconstructs that moment from the original controller inputs.",
      targets: () => [document.querySelector(".transport")],
    },
    {
      title: "🎮 Step into the experiment",
      copy: "Play from here gives you control at the displayed frame. Your inputs become part of a new history—like pausing a software test to debug it live.",
      targets: () => [$("take-control")],
    },
    {
      title: "Guide what happens next",
      copy: "Branch search from here starts a new search at this moment, including any moves you made. Your original search stays intact. The new branch appears in the menu above.",
      targets: () => [
        $("search-here"),
        document.querySelector(".branch-picker"),
      ],
    },
    {
      title: "Compare alternate futures",
      copy: "New branches appear here. Switch back to your original search, or resume another branch. Each keeps its own retained states, routes and heatmap.",
      targets: () => [document.querySelector(".branch-picker")],
    },
  ],
  onStart() {
    tourSession = {
      cell: tourCell(),
      wasPaused: paused,
      prepared: false,
      previousManual: String(current?.id).startsWith("manual-")
        ? current
        : null,
      previousFrame: currentFrame,
    };
    userSelected = true;
    playing = false;
    music.stop();
    $("play").textContent = "▶ Play history";
  },
  async beforeStep(index, valid) {
    if (index === 1 && !tourSession.prepared) tourSession.cell = tourCell();
    const cell = (tourSession.cell ||= tourCell());
    if (!cell) throw new Error("No retained cell yet");
    if (mapLevel !== cell.level) setRoom(cell.level, true);
    const view = roomView(cell.level);
    if (view.zoom > 1) {
      view.x = cell.x * 32 + 16;
      view.y = cell.y * 32 + 8;
    }
    if (index === 0) {
      if (!tourSession.wasPaused && !stats.stopped && paused) {
        paused = false;
        worker.postMessage({ type: "resume" });
        updateSearchControl();
      }
      return;
    }
    paused = true;
    worker.postMessage({ type: "pause" });
    updateSearchControl();
    if (!tourSession.prepared) {
      if (selectedCell?.key !== cell.key || !current || $("inspector").hidden)
        inspect(cell);
    }
    const deadline = performance.now() + 15000;
    while (valid()) {
      if (
        !seeking &&
        current &&
        cell.routes.has(current.id) &&
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
    if (index === 1)
      $("state-list")
        .querySelector(".selected")
        ?.scrollIntoView({ block: "nearest", behavior: "instant" });
    if (index >= 4)
      $("branch-choice").scrollIntoView({
        block: "nearest",
        behavior: "instant",
      });
  },
  onClose(intent) {
    const { previousManual: previous, previousFrame, wasPaused } = tourSession;
    tourSession = null;
    let restoration;
    if (intent !== "play" && previous && current !== previous) {
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
    if (intent !== "play" && !wasPaused && !stats.stopped) {
      paused = false;
      worker.postMessage({ type: "resume" });
      updateSearchControl();
    }
    if (intent === "play") {
      $("take-control").click();
      $("film").scrollIntoView({ block: "nearest", behavior: "instant" });
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
    stats.executions >= 30 &&
    tourCell()
  )
    beginTour();
}
$("tour-open").onclick = beginTour;
requestAnimationFrame(animate);
createEngine(base)
  .then((e) => {
    engine = e;
    origin = e.boot();
    originPixels = e.pixels();
    drawFilm();
    $("verification").textContent = "Original game";
    const state = stateCache.get(0);
    if (state) selectState(state, true).catch(fail);
  })
  .catch(fail);
startSearch();
