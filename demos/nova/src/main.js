// SPDX-License-Identifier: AGPL-3.0-or-later
import "./style.css";
import { createEngine, ROM_SHA256, CORE_REVISION } from "./emulator.js";
import { Heatmap, validateTape } from "./heat.js";
import { CREDIT, creditPNG, snapshotHash } from "./media.js";
import { viewCenter } from "./view.js";
import {
  project,
  completedLevels,
  mergeProgress,
  isMapEvidence,
} from "./world.js";
const base = new URL(import.meta.env.BASE_URL, location.href),
  $ = (id) => document.getElementById(id);
const catalog = await (await fetch(new URL("maps.json", base))).json();
const maps = new Map(catalog.maps.map((map) => [map.id, map]));
document.querySelector("#app").innerHTML = `
<header><a class="brand" href="https://github.com/pH14/harmony">◈ <b>harmony</b></a><span class="divider">/</span><span>Nova explorer</span></header>
<main><section class="exploration" aria-label="Live exploration"><div class="toolbar"><div class="controls"><span id="status" hidden>Loading Nova…</span><i id="status-dot" hidden></i><button id="pause" class="icon-button" aria-label="Pause Search" title="Pause Search" disabled></button><button id="reset" disabled>Restart Search</button><button id="zoom" title="Zoom into the selected state; drag the map to move">Zoom in</button></div></div>
<div class="goal"><div><strong id="goal-title">Level 1 · Find the exit</strong><span id="goal-status">Searching</span></div><div><span id="goal-count">0 / 40 levels cleared</span><button id="completion" hidden>Watch completion</button></div></div>
<nav id="room-tabs" aria-label="Areas in this level"></nav>
<div class="map-wrap"><canvas id="map" width="1280" height="320" tabindex="0" aria-label="Game area heatmap. Drag to move when zoomed. Arrow keys move the selection; Enter inspects a cell."></canvas><span class="map-label" id="map-label">INTRODUCTION</span><div id="map-hint">Click a warm cell to watch its history</div><div id="hover" hidden></div></div>
<div class="map-footer"><span>Recent activity <span class="gradient"></span><span class="legend">cold → busy</span></span><span id="area-progress">Introduction</span></div>
<div class="metrics"><div><b id="attempts">0</b><span>paths explored</span></div><div><b id="states">0</b><span>states retained</span></div><div><b id="cells">0</b><span>cells visited</span></div><div><b id="distance">0%</b><span>furthest into this area</span></div><div><b id="work">0</b><span>game frames executed</span></div></div></section>
<section class="inspect"><div class="film"><div class="section-title"><div><span class="eyebrow">FOLLOW A HISTORY</span><h2 id="film-title">The first possibility</h2></div><span id="verification" hidden>Starting emulator</span></div><div class="screen"><canvas id="film" width="256" height="224" aria-label="Nova gameplay replay"></canvas><span id="frame-label">FRAME 0</span></div><div class="transport"><button id="play" disabled>▶ Play history</button><input id="scrub" aria-label="Replay frame" type="range" min="0" max="0" value="0" disabled><select id="speed" aria-label="Playback speed"><option value="1">1×</option><option value="4" selected>4×</option><option value="12">12×</option></select></div><div class="film-actions"><button id="screenshot" disabled>Save this frame</button><button id="export" disabled>Save history</button><button id="import">Open history</button><input id="history-file" type="file" accept="application/json,.json" hidden></div></div>
<aside><div class="section-title"><div><span class="eyebrow">EXACT STATES, SAME PLACE</span><h2 id="cell-title">Selected state</h2></div><span id="cell-visits" class="badge">Live</span></div><p id="selection-hint" hidden></p><div id="state-list"></div><div id="details" class="details"></div></aside></section>
<section class="atlas"><div class="section-title"><h2>The game</h2><nav id="worlds" aria-label="Game worlds"></nav></div><div id="atlas" class="atlas-grid"></div></section>
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
  seed = 1,
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
  zoom = 1,
  center = 640,
  centerY = 112,
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
let stateCache = new Map(),
  sparks = [],
  bestByMap = new Map(),
  bestStates = new Map(),
  arrivals = new Map(),
  seenMaps = new Set(),
  cleared = new Set(),
  wins = new Map(),
  stats = {};
const panoramas = new Map();
function panorama(level) {
  if (!panoramas.has(level)) {
    const image = new Image();
    image.onload = () => drawAtlas(performance.now());
    image.src = new URL(maps.get(level).file, base).href;
    panoramas.set(level, image);
  }
  return panoramas.get(level);
}
const canvas = $("map"),
  ctx = canvas.getContext("2d"),
  film = $("film").getContext("2d");
const fmt = (n) => Math.round(n || 0).toLocaleString();
function showError(error) {
  $("error").hidden = false;
  $("error").textContent = String(error?.message || error);
}
function replayError(error) {
  if (current?.endpoint_sha256) showError(error);
  else fail(error);
}
function fail(error) {
  worker?.postMessage({ type: "pause" });
  $("error").hidden = false;
  $("error").textContent = String(error?.message || error);
  $("status").textContent = "Search paused";
  paused = true;
  updateSearchControl();
  $("status-dot").className = "paused";
}
function drawFilm() {
  const p = currentFrame === 0 && originPixels ? originPixels : engine.pixels();
  film.putImageData(new ImageData(p.data, p.width, p.height), 0, 0);
  $("frame-label").textContent =
    `FRAME ${fmt(currentFrame)} / ${fmt(current?.frames)}`;
  $("scrub").value = currentFrame;
}
function actionAt(frame) {
  let offset = 0;
  for (const action of current.actions) {
    if (frame < offset + action.frames)
      return {
        buttons: action.buttons,
        frames: offset + action.frames - frame,
      };
    offset += action.frames;
  }
  return null;
}
function advanceFilm(target) {
  while (currentFrame < target) {
    const action = actionAt(currentFrame);
    if (!action) break;
    const n = Math.min(action.frames, target - currentFrame);
    engine.run(action.buttons, n, true);
    currentFrame += n;
  }
  drawFilm();
}
async function verify() {
  const state = current,
    epoch = replayEpoch;
  if (state?.endpoint_sha256) {
    const hash = await snapshotHash(engine.capture());
    if (state !== current || epoch !== replayEpoch) return;
    if (hash !== state.endpoint_sha256)
      throw new Error("Saved history endpoint checksum mismatch");
    $("verification").textContent = "Saved controller history";
    return;
  }
  if (!current?.snapshot) {
    $("verification").textContent = current?.frames
      ? "Controller history"
      : "Original game";
    return;
  }
  $("verification").textContent = equal(engine.capture(), current.snapshot)
    ? "Exact replay ✓"
    : "Replay differs";
  if ($("verification").textContent === "Replay differs")
    throw new Error(
      "Replay verification failed. This history does not match its archived snapshot.",
    );
}
function equal(a, b) {
  return a.length === b.length && a.every((v, i) => v === b[i]);
}
async function seek(target, propagateError = false) {
  if (!current || !engine) return;
  const epoch = ++replayEpoch;
  playing = false;
  seeking = true;
  $("play").textContent = "Seeking…";
  $("verification").textContent = "Replaying";
  target = Math.max(0, Math.min(current.frames, Math.floor(target)));
  engine.restore(origin);
  currentFrame = 0;
  film.putImageData(
    new ImageData(originPixels.data, originPixels.width, originPixels.height),
    0,
    0,
  );
  try {
    while (currentFrame < target && epoch === replayEpoch) {
      advanceFilm(Math.min(target, currentFrame + 600));
      await new Promise((resolve) => setTimeout(resolve, 0));
    }
    if (epoch !== replayEpoch) return false;
    drawFilm();
    if (currentFrame === current.frames) await verify();
    else $("verification").textContent = "Frame " + fmt(currentFrame);
    return true;
  } catch (e) {
    if (propagateError) throw e;
    replayError(e);
    return false;
  } finally {
    if (epoch === replayEpoch) {
      seeking = false;
      $("play").textContent = "▶ Play history";
      $("scrub").value = currentFrame;
    }
  }
}
async function selectState(state, autoplay = false, propagateError = false) {
  if (!engine) return;
  current = state;
  frameCredit = 0;
  $("film-title").textContent =
    `State #${state.id ?? "saved"} · ${fmt(state.frames)} frames`;
  $("scrub").max = state.frames;
  for (const id of ["play", "scrub", "screenshot", "export"])
    $(id).disabled = false;
  $("details").replaceChildren();
  const fields = [
    ["Position", `${state.observation.x}, ${state.observation.y}`],
    ["Health", `${state.observation.health} / 4`],
    [
      "Chips",
      state.observation.chips_needed
        ? `${state.observation.chips} / ${state.observation.chips_needed}`
        : state.observation.chips,
    ],
    ["Ability", state.observation.ability],
    [
      "Area",
      isMapEvidence(state.observation, catalog.levels)
        ? maps.get(state.observation.level)?.label
        : "Level transition",
    ],
    ["Controller actions", state.actions.length],
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
  if (
    isMapEvidence(state.observation, catalog.levels) &&
    state.observation.level !== mapLevel
  )
    setRoom(state.observation.level);
  const selection = seek(autoplay ? 0 : state.frames, propagateError),
    epoch = replayEpoch;
  const success = await selection;
  if (!success || current !== state || epoch !== replayEpoch) return false;
  if (autoplay) {
    playing = true;
    $("play").textContent = "Ⅱ Pause film";
  }
  renderStates();
  return true;
}
function renderStates() {
  const ids = selectedCell?.ids || [current?.id].filter((x) => x !== undefined);
  $("state-list").replaceChildren(
    ...ids.map((id) => {
      const state = stateCache.get(id),
        button = document.createElement("button");
      button.className = "state" + (current?.id === id ? " selected" : "");
      button.textContent = state
        ? `#${id}  ·  ${fmt(state.frames)} frames  ·  ♥ ${state.observation.health}  ·  ${state.observation.chips} chips`
        : `#${id} · loading history…`;
      button.disabled = !state;
      button.onclick = () => {
        userSelected = true;
        selectState(state).catch(fail);
      };
      return button;
    }),
  );
}
function inspect(cell) {
  if (!cell) {
    return;
  }
  userSelected = true;
  selectedCell = cell;
  $("cell-title").textContent = `Cell ${cell.x}, ${cell.y}`;
  $("cell-visits").textContent = `${fmt(cell.visits)} visits`;
  $("selection-hint").textContent = "";
  renderStates();
  if (cell.ids.length)
    worker.postMessage({ type: "states", ids: cell.ids, request: ++request });
  else
    $("selection-hint").textContent =
      "The search visited this cell without retaining an endpoint here. Watch for a new arrival.";
}
function startSearch() {
  if (worker) worker.terminate();
  heat = new Heatmap();
  stateCache = new Map();
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
  for (const id of ["play", "scrub", "screenshot", "export"])
    $(id).disabled = true;
  $("details").replaceChildren();
  $("state-list").replaceChildren();
  updateStats();
  sparks = [];
  setRoom(0);
  zoom = 1;
  center = 640;

  $("error").hidden = true;
  $("status").textContent = "Loading Nova…";
  $("pause").disabled = true;
  updateSearchControl();

  $("status-dot").className = "";
  $("cell-title").textContent = "Selected state";
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
      if (data.type === "ready") {
        ready = true;
        $("status").textContent = "Exploring";
        updateSearchControl();
        $("pause").disabled = false;
        $("reset").disabled = false;
        const state = data.state;
        stateCache.set(0, state);
        seenMaps.add(state.observation.level);
        updateRoomTabs();
        heat.visit(
          {
            observation: project(
              state.observation,
              maps.get(state.observation.level),
            ),
            retained: 0,
          },
          performance.now(),
        );
        if (engine) selectState(state, true).catch(fail);
      } else if (data.type === "batch") {
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
          if (first && !userSelected) setRoom(o.level);
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
        $("status-dot").className = "paused";
      }
    } catch (e) {
      fail(e);
    }
  };
  worker.postMessage({ type: "init", base: base.href, seed });
}
function updateSearchControl() {
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
function setRoom(level) {
  const map = maps.get(level);
  if (!map) return;
  mapLevel = level;
  mapWidth = map.width;
  mapHeight = map.height;
  canvas.dataset.map = level;
  canvas.dataset.mapWidth = mapWidth;
  canvas.dataset.mapHeight = mapHeight;
  zoom = 1;
  center = mapWidth / 2;
  centerY = mapHeight / 2;
  selectedCell = null;
  hoverCell = null;
  const owner = catalog.levels.find((l) => l.rooms.includes(level));
  focusedLevel = owner?.id ?? 0;
  focusedWorld = owner?.world ?? 1;
  $("map-label").textContent = map.label.toUpperCase();
  $("zoom").textContent = "Zoom in";
  renderNavigation();
  updateStats();
  panorama(level);
}
function browseRoom(level) {
  setRoom(level);
  const id = bestStates.get(level) ?? (level === 0 && ready ? 0 : undefined);
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
          const status = document.createElement("span");
          status.className = "area-state";
          button.append(thumb, label, status);
          button.onclick = () => {
            userSelected = true;
            browseRoom(id);
          };
          article.append(button);
          panorama(id);
        }
        return article;
      }),
  );
  drawAtlas(performance.now());
}
function drawAtlas(now) {
  for (const card of document.querySelectorAll(".map-card")) {
    const id = Number(card.dataset.map),
      map = maps.get(id),
      c = card.querySelector("canvas"),
      context = c.getContext("2d"),
      image = panoramas.get(id),
      scale = Math.min(c.width / map.width, c.height / map.height);
    context.imageSmoothingEnabled = false;
    context.fillStyle = "#0b1928";
    context.fillRect(0, 0, c.width, c.height);
    context.save();
    context.translate(
      (c.width - map.width * scale) / 2,
      (c.height - map.height * scale) / 2,
    );
    context.scale(scale, scale);
    if (image?.complete && image.naturalWidth) context.drawImage(image, 0, 0);
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
    card.querySelector(".area-state").textContent = seenMaps.has(id)
      ? "Visited"
      : "Unexplored";
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
  const owner = catalog.levels.find((level) => level.id === focusedLevel);
  const area = [...(owner?.rooms || [])]
    .reverse()
    .find((id) => id >= 45 && arrivals.has(id));
  if (area !== undefined)
    return {
      id: arrivals.get(area),
      label: `Watch ${maps.get(area).label} arrival`,
    };
  return null;
}
$("completion").onclick = () => {
  const witness = completionWitness();
  if (!witness) return;
  userSelected = true;
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
  $("goal-count").textContent = `${cleared.size} / 40 levels cleared`;
  $("goal-title").textContent = gameWon
    ? "Game complete"
    : `Level ${Math.min(focusedLevel + 1, 40)} · ${cleared.has(focusedLevel) ? "Complete ✓" : "Find the exit"}`;
  const witness = completionWitness();
  $("completion").hidden = !witness;
  if (witness) $("completion").textContent = witness.label;
  $("area-progress").textContent =
    `${maps.get(mapLevel).label} · ${seenMaps.has(mapLevel) ? "Visited" : "Not reached yet"}`;
  drawAtlas(performance.now());
  $("work").textContent = fmt(stats.frames);
}
function drawMap(now) {
  ctx.imageSmoothingEnabled = false;
  ctx.fillStyle = "#122b42";
  ctx.fillRect(0, 0, canvas.width, canvas.height);
  ctx.save();
  ctx.translate(canvas.width / 2, canvas.height / 2);
  const scale =
    Math.min(canvas.width / mapWidth, canvas.height / mapHeight) * zoom;
  ctx.scale(scale, scale);
  ctx.translate(-center, -centerY);
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
      ctx.lineWidth = 0.7 / zoom;
      ctx.strokeRect(x + 1, y + 1, 30, 30);
    }
  }
  ctx.strokeStyle = "rgba(166,205,241,.07)";
  ctx.lineWidth = 0.5 / zoom;
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
  const focus = hoverCell || selectedCell;
  if (focus?.level === mapLevel) {
    ctx.strokeStyle = "#fff0bd";
    ctx.lineWidth = 1.5 / zoom;
    ctx.strokeRect(focus.x * 32 + 1, focus.y * 32 - 8 + 1, 30, 30);
  }
  if (
    current?.observation.level === mapLevel &&
    isMapEvidence(current.observation, catalog.levels)
  ) {
    const o = project(current.observation, maps.get(mapLevel));
    ctx.strokeStyle = "#fff5cc";
    ctx.lineWidth = 1.5 / zoom;
    ctx.beginPath();
    ctx.arc(o.x, o.y - 8, 9, 0, Math.PI * 2);
    ctx.stroke();
    ctx.fillStyle = "#fff5cc";
    ctx.fillRect(o.x - 2, o.y - 10, 4, 4);
  }
  ctx.restore();
  $("map-hint").style.opacity = stats.executions > 25 ? "0" : "1";
}
function mapCoordinates(e) {
  const rect = canvas.getBoundingClientRect(),
    px = ((e.clientX - rect.left) / rect.width) * canvas.width,
    py = ((e.clientY - rect.top) / rect.height) * canvas.height,
    scale = Math.min(canvas.width / mapWidth, canvas.height / mapHeight) * zoom;
  return {
    x: Math.floor(((px - canvas.width / 2) / scale + center) / 32),
    y: Math.floor(((py - canvas.height / 2) / scale + centerY + 8) / 32),
    level: mapLevel,
  };
}
canvas.addEventListener("pointerdown", (e) => {
  pointerStart = { x: e.clientX, y: e.clientY, center, centerY };
  dragged = false;
});
canvas.addEventListener("pointerup", () => {
  pointerStart = null;
});
canvas.addEventListener("pointercancel", () => {
  pointerStart = null;
});
canvas.addEventListener("pointermove", (e) => {
  if (pointerStart && e.buttons && zoom > 1) {
    const dx = e.clientX - pointerStart.x,
      dy = e.clientY - pointerStart.y;
    if (Math.hypot(dx, dy) > 4) dragged = true;
    if (dragged) {
      userSelected = true;
      const rect = canvas.getBoundingClientRect(),
        scale =
          Math.min(canvas.width / mapWidth, canvas.height / mapHeight) * zoom;
      const view = viewCenter(
        mapWidth,
        zoom,
        {
          x: pointerStart.center - (dx * canvas.width) / rect.width / scale,
          y: pointerStart.centerY - (dy * canvas.height) / rect.height / scale,
        },
        mapHeight,
        { width: canvas.width, height: canvas.height },
      );
      center = view.x;
      centerY = view.y;
      return;
    }
  }
  hoverCell = mapCoordinates(e);
  const cell = heat.cells.get(`${mapLevel}:${hoverCell.x}:${hoverCell.y}`);
  $("hover").hidden = false;
  $("hover").textContent =
    `Cell ${hoverCell.x}, ${hoverCell.y} · ${cell?.visits || 0} visits · ${cell?.ids.length || 0} states`;
});
canvas.addEventListener("pointerleave", () => {
  pointerStart = null;
  hoverCell = null;
  $("hover").hidden = true;
});
canvas.addEventListener("click", (e) => {
  if (dragged) {
    dragged = false;
    return;
  }
  const c = mapCoordinates(e);
  inspect(heat.cells.get(`${mapLevel}:${c.x}:${c.y}`));
});
canvas.addEventListener("keydown", (e) => {
  const c = hoverCell || selectedCell || { level: mapLevel, x: 1, y: 5 };
  if (["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"].includes(e.key)) {
    e.preventDefault();
    hoverCell = {
      ...c,
      x: Math.max(
        0,
        Math.min(
          Math.floor(mapWidth / 32) - 1,
          c.x + (e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0),
        ),
      ),
      y: Math.max(
        0,
        Math.min(
          Math.ceil(mapHeight / 32) - 1,
          c.y + (e.key === "ArrowDown" ? 1 : e.key === "ArrowUp" ? -1 : 0),
        ),
      ),
    };
  }
  if (e.key === "Enter") {
    e.preventDefault();
    inspect(heat.cells.get(`${mapLevel}:${c.x}:${c.y}`));
  }
});
$("zoom").onclick = () => {
  userSelected = true;
  zoom = zoom === 1 ? 2 : zoom === 2 ? 4 : 1;
  const o =
    current?.observation.level === mapLevel &&
    isMapEvidence(current.observation, catalog.levels)
      ? project(current.observation, maps.get(mapLevel))
      : null;
  const point = o
    ? { x: o.x, y: o.y - 8 }
    : {
        x: (selectedCell?.x ?? Math.floor(center / 32)) * 32 + 16,
        y: (selectedCell?.y ?? Math.floor(centerY / 32)) * 32 + 8,
      };
  const view = viewCenter(mapWidth, zoom, point, mapHeight, {
    width: canvas.width,
    height: canvas.height,
  });
  center = view.x;
  centerY = view.y;
  $("zoom").textContent = zoom === 1 ? "Zoom in" : `${zoom}× · zoom in`;
};
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
$("play").onclick = async () => {
  if (seeking) return;
  userSelected = true;
  if (playing) {
    playing = false;
    $("play").textContent = "▶ Play history";
    return;
  }
  if (currentFrame >= current.frames) await seek(0);
  playing = true;
  frameCredit = 0;
  $("play").textContent = "Ⅱ Pause film";
};
$("scrub").oninput = () => {
  userSelected = true;
  seek(Number($("scrub").value));
};
function download(blob, name) {
  const url = URL.createObjectURL(blob),
    a = document.createElement("a");
  a.href = url;
  a.download = name;
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
$("screenshot").onclick = () => {
  const frame = currentFrame,
    id = current?.id ?? "saved";
  $("film").toBlob(async (blob) =>
    download(
      await creditPNG(blob, frame),
      `nova-state-${id}-frame-${frame}.png`,
    ),
  );
};
$("export").onclick = async () => {
  const tape = {
    format: "harmony-nova-browser-v1",
    rom_sha256: ROM_SHA256,
    core_revision: CORE_REVISION,
    seed,
    actions: current.actions,
    observation: current.observation,
    endpoint_sha256:
      current.endpoint_sha256 ||
      (await snapshotHash(current.snapshot || origin)),
    credits: CREDIT,
  };
  download(
    new Blob([JSON.stringify(tape, null, 2)], { type: "application/json" }),
    `nova-history-${current?.id ?? "saved"}.json`,
  );
};
$("import").onclick = () => $("history-file").click();
$("history-file").onchange = async (e) => {
  try {
    const file = e.target.files[0];
    if (!file) return;
    if (file.size > 1000000) throw new Error("History file too large");
    const readingEpoch = replayEpoch,
      text = await file.text();
    if (readingEpoch !== replayEpoch) return;
    const tape = JSON.parse(text),
      frames = validateTape(tape);
    userSelected = true;
    const state = {
      id: "saved",
      actions: tape.actions,
      frames,
      endpoint_sha256: tape.endpoint_sha256,
      observation: { x: 0, y: 0, health: 0, chips: 0, ability: 0, level: 0 },
    };
    if (!(await selectState(state, false, true))) return;
    const epoch = replayEpoch;
    const actual = await snapshotHash(engine.capture());
    if (current !== state || epoch !== replayEpoch) return;
    if (actual !== tape.endpoint_sha256)
      throw new Error("Saved history endpoint checksum mismatch");
    state.observation = engine.observation();
    if (!(await selectState(state, false, true))) return;
    $("error").hidden = true;
    $("verification").textContent = "Saved controller history";
  } catch (err) {
    $("error").hidden = false;
    $("error").textContent = err.message;
  } finally {
    e.target.value = "";
  }
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
    `<span class="eyebrow">CREDITS & LICENSING</span><h2>Nova the Squirrel</h2><p>Created by <a href="https://novasquirrel.com/">NovaSquirrel</a>. <a href="https://novasquirrel.itch.io/nova-the-squirrel">Play the original game</a>.</p><p>The game’s code is GPL-3.0-or-later. Its original graphics, sound and the gameplay imagery shown here are <a href="https://creativecommons.org/licenses/by-nc-sa/4.0/">CC BY-NC-SA 4.0</a>. This is a noncommercial software demonstration, not endorsed by NovaSquirrel. The original character designs and gameplay are preserved.</p><p>QuickNES library sources: LGPL-2.1-or-later. The compiled libretro browser core is distributed under GPL-2.0 terms with a GPL-2.0-or-later C++ shim. The separate Harmony browser interface and Rust search code: AGPL-3.0-or-later.</p><p><a href="licenses/CREDITS.md">Full credits and restrictions</a> · <a href="licenses/nova-source.tar.gz">Nova corresponding source</a> · <a href="licenses/quicknes-source.tar.gz">QuickNES source</a> · <a href="licenses/harmony-source.tar.gz">Harmony source</a> \u00b7 <a href="licenses/rust-dependencies.tar.gz">Rust dependency sources and notices</a> · <a href="https://github.com/pH14/harmony">Repository and build instructions</a></p>`,
  );
function animate(now) {
  const elapsed = lastTime ? Math.min(100, now - lastTime) : 0;
  lastTime = now;
  drawMap(now);
  if (Math.floor(now / 250) !== Math.floor((now - elapsed) / 250))
    drawAtlas(now);
  if (playing && !seeking && current) {
    frameCredit += ((elapsed * 60) / 1000) * Number($("speed").value);
    const frames = Math.floor(frameCredit);
    frameCredit -= frames;
    if (frames) {
      advanceFilm(Math.min(current.frames, currentFrame + frames));
      if (currentFrame === current.frames) {
        playing = false;
        $("play").textContent = "↺ Play again";
        verify().catch(replayError);
      }
    }
  }
  requestAnimationFrame(animate);
}
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
