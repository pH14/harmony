// SPDX-License-Identifier: AGPL-3.0-or-later
import "./style.css";
import { createEngine, ROM_SHA256, CORE_REVISION } from "./emulator.js";
import { Heatmap, validateTape } from "./heat.js";
import { CREDIT, creditPNG, snapshotHash } from "./media.js";
import { viewCenter } from "./view.js";
const base = new URL(import.meta.env.BASE_URL, location.href),
  $ = (id) => document.getElementById(id);
document.querySelector("#app").innerHTML = `
<header><a class="brand" href="https://github.com/pH14/harmony">◈ <b>harmony</b></a><span class="divider">/</span><span>Nova explorer</span><span class="local"><i></i> Everything runs in your browser</span></header>
<main><section class="intro"><div><div class="eyebrow">A LIVING MAP OF POSSIBILITIES</div><h1>Watch the search find its way.</h1><p>One squirrel. Thousands of alternate histories. Click a warm cell to see how it got there.</p></div><button id="about" class="quiet">How it works ↗</button></section>
<section class="exploration" aria-label="Live exploration"><div class="toolbar"><div class="run-status"><i id="status-dot"></i><strong id="status">Loading Nova…</strong><span id="seed-label">seed 1</span><select id="room" aria-label="Level room"><option value="0">Introduction</option><option value="40">Main level</option></select></div><div class="controls"><button id="pause" disabled>Pause search</button><button id="reset" disabled>Start fresh</button><button id="zoom">Zoom in</button><button id="fit">Fit level</button><button id="left" aria-label="Pan left">←</button><button id="right" aria-label="Pan right">→</button></div></div>
<div class="map-wrap"><canvas id="map" width="1280" height="224" tabindex="0" aria-label="Level one heatmap. Arrow keys move the selection; Enter inspects a cell."></canvas><span class="map-label" id="map-label">LEVEL 1 · INTRODUCTION</span><div id="map-hint">Cold terrain becomes warm as the search visits it.</div><div id="hover" hidden></div></div>
<div class="map-footer"><span>Recent activity <span class="gradient"></span> <span class="legend">cold → busy</span></span><span>Heat cools. Histories stay.</span><label><input id="heat-toggle" type="checkbox" checked> Show heat</label></div>
<div class="metrics"><div><b id="attempts">0</b><span>paths explored</span></div><div><b id="states">0</b><span>states retained</span></div><div><b id="cells">0</b><span>cells visited</span></div><div><b id="distance">0%</b><span>furthest into this room</span></div><div><b id="work">0</b><span>game frames executed</span></div></div></section>
<section class="inspect"><div class="film"><div class="section-title"><div><span class="eyebrow">FOLLOW A HISTORY</span><h2 id="film-title">The first possibility</h2></div><span id="verification" class="badge">Starting emulator</span></div><div class="screen"><canvas id="film" width="256" height="224" aria-label="Nova gameplay replay"></canvas><span id="frame-label">FRAME 0</span></div><div class="transport"><button id="play" disabled>▶ Play history</button><input id="scrub" aria-label="Replay frame" type="range" min="0" max="0" value="0" disabled><select id="speed" aria-label="Playback speed"><option value="1">1×</option><option value="4" selected>4×</option><option value="12">12×</option></select></div><div class="film-actions"><button id="screenshot" disabled>Save this frame</button><button id="export" disabled>Save history</button><button id="import">Open history</button><input id="history-file" type="file" accept="application/json,.json" hidden></div></div>
<aside><div class="section-title"><div><span class="eyebrow">EXACT STATES, SAME PLACE</span><h2 id="cell-title">Let the map wake up.</h2></div><span id="cell-visits" class="badge">Live</span></div><p id="selection-hint">The film follows the furthest state until you choose a cell. Each state has its own controller history.</p><div id="state-list"></div><div id="details" class="details"></div><div class="event-title">EXPLORATION NOTES</div><ol id="events" aria-label="Recent discoveries"></ol></aside></section>
<footer><span>Nova the Squirrel by <a href="https://novasquirrel.com/">NovaSquirrel</a> · Original game artwork <a href="https://creativecommons.org/licenses/by-nc-sa/4.0/">CC BY-NC-SA 4.0</a></span><button id="credits">Credits & source</button></footer>
<div id="error" role="alert" hidden></div>
<dialog id="info"><button id="close-info" class="close" aria-label="Close">×</button><div id="info-content"></div></dialog></main>`;
let heat = new Heatmap(),
  worker,
  engine,
  origin,
  originPixels,
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
  bestX = 52,
  latestBest = null,
  lastAuto = 0,
  zoom = 1,
  center = 640,
  centerY = 112,
  hoverCell = null,
  selectedCell = null,
  mapLevel = 0,
  mapWidth = 1280,
  roomFound = false,
  bestMainX = 0;
let stateCache = new Map(),
  sparks = [],
  events = [],
  stats = {};
const panoramas = new Map();
for (const [level, file] of [
  [0, "level-one.png"],
  [40, "level-one-main.png"],
]) {
  const image = new Image();
  image.src = new URL(file, base).href;
  panoramas.set(level, image);
}
const canvas = $("map"),
  ctx = canvas.getContext("2d"),
  film = $("film").getContext("2d");
const fmt = (n) => Math.round(n || 0).toLocaleString();
function note(message) {
  events.unshift(message);
  events = events.slice(0, 4);
  $("events").replaceChildren(
    ...events.map((m) => {
      const li = document.createElement("li");
      li.textContent = m;
      return li;
    }),
  );
}
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
  $("pause").textContent = "Resume search";
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
    ["Chips", state.observation.chips],
    ["Ability", state.observation.ability],
    ["Room", state.observation.level],
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
  if (state.observation.level !== mapLevel) setRoom(state.observation.level);
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
    note("This cell has not been visited yet.");
    return;
  }
  userSelected = true;
  selectedCell = cell;
  $("cell-title").textContent = `Cell ${cell.x}, ${cell.y}`;
  $("cell-visits").textContent = `${fmt(cell.visits)} visits`;
  $("selection-hint").textContent =
    "Choose a retained state here, then play or scrub its entire history.";
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
  paused = false;
  ready = false;
  seeking = false;
  bestX = 52;
  bestMainX = 0;
  roomFound = false;
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
  events = [];
  $("error").hidden = true;
  $("status").textContent = "Loading Nova…";
  $("pause").disabled = true;
  $("pause").textContent = "Pause search";
  $("seed-label").textContent = "seed " + seed;
  $("status-dot").className = "";
  $("cell-title").textContent = "Let the map wake up.";
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
        $("pause").disabled = false;
        $("reset").disabled = false;
        const state = data.state;
        stateCache.set(0, state);
        heat.visit(
          { observation: state.observation, retained: 0 },
          performance.now(),
        );
        if (engine) selectState(state, true).catch(fail);
        note("Started from the original game, at level one.");
      } else if (data.type === "batch") {
        stats = data;
        const now = performance.now();
        for (const point of data.points) {
          if (point.observation.level === 40) {
            if (!roomFound) {
              roomFound = true;
              note("Found the exit. Exploring the main room.");
              if (!userSelected) setRoom(40);
            }
            if (point.observation.x > bestMainX) {
              bestMainX = point.observation.x;
              if (point.retained !== null) latestBest = point.retained;
            }
          }
          heat.visit(point, now);
          sparks.push({
            x: point.observation.x,
            y: point.observation.y,
            level: point.observation.level,
            time: now,
          });
          if (point.observation.level === 0 && point.observation.x > bestX) {
            bestX = point.observation.x;
            if (point.retained !== null && !roomFound)
              latestBest = point.retained;
          }
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
          note(
            `Reached ${Math.round(((mapLevel === 0 ? bestX : bestMainX) / mapWidth) * 100)}% of this room.`,
          );
        }
        updateStats();
      } else if (data.type === "states") {
        for (const state of data.states) stateCache.set(state.id, state);
        if (data.request === 0) {
          if (!userSelected && data.states[0])
            selectState(data.states[0], true).catch(fail);
        } else if (data.request === request) {
          renderStates();
          if (data.states[0]) selectState(data.states[0]).catch(fail);
        }
      } else if (data.type === "limit") {
        paused = true;
        $("status").textContent = "Run complete";
        $("pause").disabled = true;
        $("status-dot").className = "paused";
        note("Run budget reached. Every retained history is still available.");
      }
    } catch (e) {
      fail(e);
    }
  };
  worker.postMessage({ type: "init", base: base.href, seed });
}
function setRoom(level) {
  mapLevel = level === 40 ? 40 : 0;
  mapWidth = mapLevel === 0 ? 1280 : 3584;
  canvas.width = mapWidth;
  zoom = 1;
  center = mapWidth / 2;
  centerY = 112;
  selectedCell = null;
  hoverCell = null;
  $("room").value = String(mapLevel);
  $("map-label").textContent =
    "LEVEL 1 · " + (mapLevel === 0 ? "INTRODUCTION" : "MAIN ROOM");
  $("zoom").textContent = "Zoom in";
  updateStats();
}
$("room").onchange = () => {
  userSelected = true;
  setRoom(Number($("room").value));
};
$("left").onclick = () => {
  userSelected = true;
  center = Math.max(mapWidth / 2 / zoom, center - mapWidth / 8 / zoom);
};
$("right").onclick = () => {
  userSelected = true;
  center = Math.min(
    mapWidth - mapWidth / 2 / zoom,
    center + mapWidth / 8 / zoom,
  );
};
function updateStats() {
  $("attempts").textContent = fmt(stats.executions);
  $("states").textContent = fmt(stats.states);
  $("cells").textContent = fmt(heat.cells.size);
  $("distance").textContent =
    Math.min(
      100,
      Math.round(((mapLevel === 0 ? bestX : bestMainX) / mapWidth) * 100),
    ) + "%";
  $("work").textContent = fmt(stats.frames);
}
function drawMap(now) {
  ctx.imageSmoothingEnabled = false;
  ctx.fillStyle = "#122b42";
  ctx.fillRect(0, 0, mapWidth, 224);
  ctx.save();
  ctx.translate(mapWidth / 2, 112);
  ctx.scale(zoom, zoom);
  ctx.translate(-center, -centerY);
  const panorama = panoramas.get(mapLevel);
  if (panorama?.complete && panorama.naturalWidth)
    ctx.drawImage(panorama, 0, 0);
  ctx.fillStyle = "rgba(7,24,43,.62)";
  ctx.fillRect(0, 0, mapWidth, 224);
  if ($("heat-toggle").checked) {
    ctx.fillStyle = "rgba(65,113,162,.11)";
    ctx.fillRect(0, 0, mapWidth, 224);
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
    ctx.lineTo(x, 224);
    ctx.stroke();
  }
  for (let y = 24; y < 224; y += 32) {
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
  if (current?.observation.level === mapLevel) {
    const o = current.observation;
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
    px = ((e.clientX - rect.left) / rect.width) * mapWidth,
    py = ((e.clientY - rect.top) / rect.height) * 224;
  return {
    x: Math.floor(((px - mapWidth / 2) / zoom + center) / 32),
    y: Math.floor(((py - 112) / zoom + centerY + 8) / 32),
    level: mapLevel,
  };
}
canvas.addEventListener("pointermove", (e) => {
  hoverCell = mapCoordinates(e);
  const cell = heat.cells.get(`${mapLevel}:${hoverCell.x}:${hoverCell.y}`);
  $("hover").hidden = false;
  $("hover").textContent =
    `Cell ${hoverCell.x}, ${hoverCell.y} · ${cell?.visits || 0} visits · ${cell?.ids.length || 0} states`;
});
canvas.addEventListener("pointerleave", () => {
  hoverCell = null;
  $("hover").hidden = true;
});
canvas.addEventListener("click", (e) => {
  const c = mapCoordinates(e);
  inspect(heat.cells.get(`${mapLevel}:${c.x}:${c.y}`));
});
canvas.addEventListener("keydown", (e) => {
  const c = hoverCell || selectedCell || { level: 0, x: 1, y: 5 };
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
          7,
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
  const point =
    current?.observation.level === mapLevel
      ? { x: current.observation.x, y: current.observation.y - 8 }
      : {
          x:
            (selectedCell?.x ??
              Math.floor((mapLevel === 0 ? bestX : bestMainX) / 32)) *
              32 +
            16,
          y: (selectedCell?.y ?? 5) * 32 + 8,
        };
  const view = viewCenter(mapWidth, zoom, point);
  center = view.x;
  centerY = view.y;
  $("zoom").textContent = zoom === 1 ? "Zoom in" : `${zoom}× · zoom in`;
};
$("fit").onclick = () => {
  userSelected = true;
  zoom = 1;
  center = mapWidth / 2;
  centerY = 112;
  $("zoom").textContent = "Zoom in";
};
$("pause").onclick = () => {
  paused = !paused;
  worker.postMessage({ type: paused ? "pause" : "resume" });
  $("pause").textContent = paused ? "Resume search" : "Pause search";
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
    note("Opened a saved history locally.");
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
$("about").onclick = () =>
  showInfo(
    `<span class="eyebrow">WHAT YOU ARE WATCHING</span><h2>Search, made visible.</h2><p>QuickNES runs the original Nova ROM. A Rust search worker uses Dissonance’s real archive and state selector to restore retained states, try controller actions, and keep new possibilities.</p><p>Each square is a 32-pixel region. Warmth shows recently visited endpoints; it fades with a six-second half-life. A square can hold several histories with different health, abilities or progress.</p><p>Click a visited square to inspect its states. Play a history from the beginning, scrub to any frame, save a screenshot, or download a controller history to reopen later.</p><p>This is a bounded browser search driver, with one worker and at most 20,000 paths. The map background is a panorama captured from the original game; all heat and states come from the live search. The separate Linux/Consonance demonstration is still to come.</p>`,
  );
$("credits").onclick = () =>
  showInfo(
    `<span class="eyebrow">CREDITS & LICENSING</span><h2>Nova the Squirrel</h2><p>Created by <a href="https://novasquirrel.com/">NovaSquirrel</a>. <a href="https://novasquirrel.itch.io/nova-the-squirrel">Play the original game</a>.</p><p>The game’s code is GPL-3.0-or-later. Its original graphics, sound and the gameplay imagery shown here are <a href="https://creativecommons.org/licenses/by-nc-sa/4.0/">CC BY-NC-SA 4.0</a>. This is a noncommercial software demonstration, not endorsed by NovaSquirrel. The original character designs and gameplay are preserved.</p><p>QuickNES library sources: LGPL-2.1-or-later. The compiled libretro browser core is distributed under GPL-2.0 terms with a GPL-2.0-or-later C++ shim. The separate Harmony browser interface and Rust search code: AGPL-3.0-or-later.</p><p><a href="licenses/CREDITS.md">Full credits and restrictions</a> · <a href="licenses/nova-source.tar.gz">Nova corresponding source</a> · <a href="licenses/quicknes-source.tar.gz">QuickNES source</a> · <a href="licenses/harmony-source.tar.gz">Harmony source</a> · <a href="https://github.com/pH14/harmony">Repository and build instructions</a></p>`,
  );
function animate(now) {
  const elapsed = lastTime ? Math.min(100, now - lastTime) : 0;
  lastTime = now;
  drawMap(now);
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
