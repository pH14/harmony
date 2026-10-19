// SPDX-License-Identifier: AGPL-3.0-or-later
import { readFile, writeFile, mkdir } from "node:fs/promises";
import { resolve, join } from "node:path";
import { pathToFileURL } from "node:url";
import { gzipSync, gunzipSync } from "node:zlib";
import { machine, routeTape, compress, hash } from "./runtime.mjs";
import { players, signals } from "./signals.mjs";
import { symbolMachine } from "./symbol-runtime.mjs";

const [indexPath, romDirectory, runtime, bindingsPath, wasmPath, out, side, levelText, roundText = "1"] = process.argv.slice(2);
if (!levelText) throw new Error("soak-level.mjs INDEX ROMS RUNTIME BINDINGS WASM OUTPUT SIDE LEVEL [ROUND]");
const indexBytes = await readFile(indexPath), index = JSON.parse(indexBytes), level = Number(levelText), round = Number(roundText);
const layout = index.levels.find(l => l.side === side && l.level === level);
if (!layout || !Number.isInteger(round) || round < 1) throw new Error("unknown level or invalid round");
await mkdir(out, { recursive: true });
const rom = await readFile(join(romDirectory, layout.rom));
const m = process.env.FAMIDASH_SYMBOLS
  ? await symbolMachine(join(romDirectory, layout.rom), runtime, process.env.FAMIDASH_SYMBOLS)
  : await machine(join(romDirectory, layout.rom), runtime);
if (m.levelName && m.levelName(level) !== layout.name)
  throw new Error(`level index ${level} selects ${m.levelName(level)}, expected ${layout.name}`);
const targetLevel = layout.release_target_level ?? level;
const targetSide = layout.release_target_side ?? side;
if (!["main", "b", "c", "d"].includes(targetSide)) throw new Error("unknown release target side");
if (targetLevel !== level) {
  if (!m.mapTargetLevel) throw new Error("level aliases require a symbol-based memory adapter");
  m.mapTargetLevel(level, targetLevel);
}
const menuDelay = Number(process.env.FAMIDASH_MENU_DELAY || 0);
if (!Number.isSafeInteger(menuDelay) || menuDelay < 0 || menuDelay > 3600)
  throw new Error("invalid menu delay");
const startupFrames = Number(process.env.FAMIDASH_STARTUP_FRAMES || (process.env.FAMIDASH_SYMBOLS ? 180 : 22));
if (!Number.isSafeInteger(startupFrames) || startupFrames < 1 || startupFrames > 3600)
  throw new Error("invalid startup wait");
const boot = m.boot.bind(m);
m.boot = (side, level) => boot(side, level, menuDelay, startupFrames);
const powerOn = m.core.capture();
if (m.rom_sha256 !== layout.rom_sha256) throw new Error("release ROM identity mismatch");
const bindings = await import(pathToFileURL(resolve(bindingsPath)));
const wasm = await readFile(wasmPath);
await bindings.default({ module_or_path: wasm });
const mixture = process.env.FAMIDASH_MIXTURE || "alphabet_only";
if (mixture !== "alphabet_only" && bindings.famidash_level_search.length !== 12)
  throw new Error("configured search mixture requires the updated single-level API");
globalThis.harmonyCores = { 0: { ...m.core, rom, reset: () => m.core.restore(powerOn),
  memory: () => m.ram().slice(), ramView: m.ram,
  frames(buttons, frames) { const data = new Uint8Array(frames * 2048); for (let i = 0; i < frames; i++) { m.core.step(buttons, 1); data.set(m.ram(), i * 2048); } return data; } } };
if (m.adapter) Object.assign(globalThis.harmonyCores[0], m.adapter);
const game = targetSide === "main" ? targetLevel >= 22 ? "famidash-community" : "famidash" : `famidash-${targetSide}`;
const identity = `quicknes-browser:${m.core.revision}`, seen = new Set(), cells = new Set(), modes = new Set();
const toolHashes = Object.fromEntries(await Promise.all(
  ["soak-level.mjs", "runtime.mjs", "signals.mjs", "symbol-runtime.mjs"].map(async name =>
    [name, hash(await readFile(new URL(name, import.meta.url)))])));
const report = { schema: 1, engine: "dissonance-wasm-single-level", side, level, name: layout.name, round,
  rom_sha256: m.rom_sha256, core_sha256: m.core_sha256, search_wasm_sha256: hash(wasm),
  bindings_sha256: hash(await readFile(bindingsPath)), runner_sha256: hash(await readFile(new URL(import.meta.url))),
  source_index_sha256: hash(indexBytes), tool_sha256: toolHashes, runtime_loader_sha256: hash(await readFile(join(runtime, "engine/quicknes.js"))),
  mixture, setup: { menu_delay: menuDelay, startup_released_frames: startupFrames }, roots: [], candidates: [], inspected: 0, unavailable: 0,
  max_x: 0, dual_inspected: 0, cleared: false, coverage_cells: 0, modes: [], status: "running" };
if (m.adapter) report.target_layout = { kind: "symbol-based linear coordinates normalized for the release target",
  debug_sha256: m.debug_sha256, symbols: m.symbols, startup_released_frames: 180,
  actual_side: side, actual_level: level, release_target_side: targetSide, release_target_level: targetLevel };
const seed = (round * 1000003 + ["main", "b", "c", "d"].indexOf(side) * 1009 + level * 101) >>> 0;
const routes = JSON.parse(await readFile(new URL(`../famidash-routes/${targetSide}.json`, import.meta.url)));
function positive(value, name) {
  const number = Number(value);
  if (!Number.isSafeInteger(number) || number <= 0) throw new Error(`invalid positive integer: ${name}`);
  return number;
}
const genesisBudget = positive(process.env.FAMIDASH_GENESIS_EXECUTIONS || 50000, "genesis budget");
const checkpointBudget = positive(process.env.FAMIDASH_CHECKPOINT_EXECUTIONS || 5000, "checkpoint budget");
const step = positive(process.env.FAMIDASH_CHECKPOINT_STEP || 1024, "checkpoint step");
let reference = [];
try { reference = routeTape(routes.routes[targetLevel]); }
catch (error) {
  report.reference_rejection = String(error);
  const encoded = routes.routes[targetLevel], ordinary = typeof encoded === "string" ? encoded.match(/^[0-5]*/)[0] : "";
  if (ordinary) reference = routeTape(ordinary);
}
const verifiedPrefix = [];
m.boot(side, level);
let referenceFrames = 0;
for (const button of reference) {
  m.core.step(button, 1); referenceFrames++;
  const o = m.observe();
  if (o.dead || o.state !== 2) break;
  verifiedPrefix.push(button);
}
report.reference = { supplied_ordinary_frames: reference.length, replayed_frames: referenceFrames,
  usable_prefix_frames: verifiedPrefix.length, endpoint: m.observe(), clears: m.observe().state === 3,
  route_intent: "not-assessed-by-completion" };
const roots = [];
function addRoot(prefix, budget, origin) {
  const key = hash(JSON.stringify(prefix));
  if (!roots.some(root => root.key === key)) roots.push({ frame: prefix.length, prefix, budget, origin, key });
}
addRoot([], genesisBudget, "genesis");
function schedule(tape, origin = "dissonance-victory") {
  const offset = Math.floor(step * ((round - 1) % 3) / 3);
  for (let frame = step + offset; frame < tape.length - 32; frame += step)
    addRoot(tape.slice(0, frame), checkpointBudget, origin);
}
schedule(verifiedPrefix, "verified-reference-prefix");
if (verifiedPrefix.length > 128 && !report.reference.clears)
  addRoot(verifiedPrefix.slice(0, -128), checkpointBudget, "before-reference-failure-or-assistance");
if (process.env.FAMIDASH_PREVIOUS_ROUND) {
  const previous = join(process.env.FAMIDASH_PREVIOUS_ROUND, `${side}-${level}`);
  let input;
  try { input = JSON.parse(await readFile(join(previous, "clearing-input.json"))); }
  catch { try { input = JSON.parse(await readFile(join(previous, "frontier.json"))); } catch {} }
  if (input) {
    const tape = expandInput(input.actions);
    for (let frame = Math.floor(step / 2); frame < tape.length - 32; frame += step)
      addRoot(tape.slice(0, frame), checkpointBudget, "previous-round-input");
    const frame = Math.max(0, tape.length - 384);
    addRoot(tape.slice(0, frame), 15000, "before-previous-frontier");
  }
}
function expandInput(actions) {
  return actions.flatMap(a => {
    if (![0, 1].includes(a.buttons) || !Number.isSafeInteger(a.frames) || a.frames <= 0 || a.frames > 200000)
      throw new Error("root input must contain bounded ordinary controller actions");
    return Array(a.frames).fill(a.buttons);
  });
}
if (process.env.FAMIDASH_ROOT_PLAN) {
  roots.length = 0;
  const plan = JSON.parse(await readFile(process.env.FAMIDASH_ROOT_PLAN));
  if (!Array.isArray(plan) || !plan.length) throw new Error("root plan must be a nonempty array");
  for (const item of plan) {
    const bytes = await readFile(item.input);
    const input = JSON.parse(item.input.endsWith(".gz") ? gunzipSync(bytes) : bytes);
    const tape = expandInput(input.actions);
    const trim = item.trim ?? 0;
    if (!Number.isSafeInteger(trim) || trim < 0 || trim > tape.length)
      throw new Error("root plan trim must be within the input length");
    const prefix = tape.slice(0, tape.length - trim);
    addRoot(prefix, positive(item.budget ?? 50000, "root plan budget"), item.input);
  }
}
let frontier = null;
const progress = async () => {
  report.coverage_cells = cells.size; report.modes = [...modes].sort();
  await writeFile(join(out, "report.json"), JSON.stringify(report, null, 2) + "\n");
};
for (let rootIndex = 0; rootIndex < roots.length; rootIndex++) {
  const root = roots[rootIndex];
  m.boot(side, level);
  let valid = true;
  for (const buttons of root.prefix) { m.core.step(buttons, 1); const o = m.observe(); if (o.dead || o.state !== 2) { valid = false; break; } }
  if (!valid) { report.roots.push({ frame: root.frame, status: "prefix-dies-or-completes" }); continue; }
  const search = bindings.famidash_level_search(game, 0, rom, identity, m.core_sha256, targetLevel,
    (seed + rootIndex * 7919) >>> 0, m.core.capture(), JSON.stringify(compress(root.prefix)), 256, root.budget, mixture);
  const worker = bindings.famidash_level_worker(game, 0, rom, identity, m.core_sha256, targetLevel);
  const record = { frame: root.frame, origin: root.origin, prefix_sha256: root.key, seed: (seed + rootIndex * 7919) >>> 0, budget: root.budget, executions: 0, max_x: 0, cleared: false };
  report.roots.push(record);
  for (let execution = 1; execution <= root.budget; execution++) {
    const job = search.plan(); if (!job) break;
    search.admit(worker.run(job), worker.note()); record.executions = execution;
    if (execution % 32 && execution !== root.budget) continue;
    const batch = JSON.parse(search.take_batch());
    if (search.won() && !record.cleared) {
      const actions = JSON.parse(search.victory());
      const input = { engine: report.engine, setup: report.setup, actions,
        seed: record.seed, executions: execution, root: root.frame };
      record.clearing_input = `clearing-root-${rootIndex}-${root.frame}.json`;
      await writeFile(join(out, record.clearing_input), JSON.stringify(input));
      record.cleared = true;
      if (!report.cleared) {
        await writeFile(join(out, "clearing-input.json"), JSON.stringify(input));
        report.cleared = true;
        schedule(actions.flatMap(a => Array(a.frames).fill(a.buttons)));
      }
    }
    for (const point of batch.points) {
      if (point.retained === null || point.observation.loading) continue;
      let snapshot; try { snapshot = search.snapshot(point.retained); } catch { report.unavailable++; continue; }
      m.core.restore(snapshot); report.inspected++;
      const observations = players(m);
      if (observations[0].level !== level) throw new Error("single-level search advanced to another level");
      for (const o of observations) {
        if (o.player) report.dual_inspected++;
        if (o.state !== 2 || o.dead) continue;
        cells.add(`${o.player}:${Math.floor(o.x / 32)}:${Math.floor(o.y / 16)}:${o.mode}:${o.gravity}`);
        modes.add(o.mode); record.max_x = Math.max(record.max_x, o.x);
        if (o.x > (frontier?.observation.x ?? 0) + 64) {
          frontier = { engine: report.engine, setup: report.setup, tool_sha256: toolHashes, side, level, round, root: root.frame, seed: record.seed,
            executions: execution, archive_id: point.retained, actions: JSON.parse(search.state(point.retained)).actions,
            observation: o, rom_sha256: m.rom_sha256, core_sha256: m.core_sha256,
            search_wasm_sha256: report.search_wasm_sha256 };
        }
        report.max_x = Math.max(report.max_x, o.x);
        for (const signal of signals(o, layout, index.metatiles)) {
          const key = `${o.player}:${signal.kind}:${signal.column ?? Math.floor(o.x / 16)}:${signal.row ?? Math.floor(o.y / 16)}`;
          if (seen.has(key)) continue;
          const state = JSON.parse(search.state(point.retained));
          const candidate = { schema: 1, engine: report.engine, setup: report.setup, tool_sha256: toolHashes, side, level, round, root: root.frame, seed: record.seed,
            executions: execution, archive_id: point.retained, signal, player: o.player, observation: o,
            actions: state.actions, rom_sha256: m.rom_sha256, core_sha256: m.core_sha256, search_wasm_sha256: report.search_wasm_sha256 };
          const file = `candidate-${report.candidates.length}.json.gz`;
          await writeFile(join(out, file), gzipSync(JSON.stringify(candidate)));
          report.candidates.push({ file, signal, player: o.player, x: o.x, y: o.y }); seen.add(key);
        }
      }
    }
    if (execution % 4096 === 0) await progress();
  }
  record.cleared = search.won();
  search.free(); worker.free();
  await progress();
  console.log(side, level, round, "root", root.frame, "executions", record.executions, "max_x", record.max_x, "candidates", report.candidates.length);
}
if (frontier) await writeFile(join(out, "frontier.json"), JSON.stringify(frontier));
report.status = report.roots.some(root => root.executions > 0) ? "searched" : "unassessed-no-valid-root";
await progress();
console.log("finished", side, level, round, report.cleared, report.max_x, report.candidates.length);
