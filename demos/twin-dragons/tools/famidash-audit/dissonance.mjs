// SPDX-License-Identifier: AGPL-3.0-or-later
import { readFile, writeFile, mkdir } from "node:fs/promises";
import { resolve, join } from "node:path";
import { pathToFileURL } from "node:url";
import { gzipSync } from "node:zlib";
import { machine, routeTape, tileAt, hash, compress } from "./runtime.mjs";

const [indexPath, romPath, runtimePath, bindingsPath, wasmPath, outputPath, ...args] = process.argv.slice(2);
if (!outputPath) throw new Error("usage: dissonance.mjs INDEX ROM RUNTIME BINDINGS WASM OUTPUT [--side main|b|c|d] [--level N] [--seed N] [--executions N] [--tail N] [--checkpoint N] [--clear 1]");
const options = { side: "main", level: 0, seed: 1, executions: 100000, tail: 150, checkpoint: 0, clear: 0 };
for (let i = 0; i < args.length; i += 2) {
  const key = args[i].slice(2);
  if (!(key in options) || args[i + 1] === undefined) throw new Error("invalid option: " + args[i]);
  options[key] = key === "side" ? args[i + 1] : Number(args[i + 1]);
}
if (!["main", "b", "c", "d"].includes(options.side)
    || ["level", "seed", "executions", "tail", "checkpoint", "clear"].some(key => !Number.isSafeInteger(options[key]) || options[key] < 0)
    || options.executions < 1 || options.tail < 1) throw new Error("invalid search options");
const indexBytes = await readFile(indexPath), index = JSON.parse(indexBytes);
const layouts = new Map(index.levels.filter(l => l.side === options.side).map(l => [l.level, l]));
if (!layouts.has(options.level)) throw new Error("unknown starting level");
const routes = JSON.parse(await readFile(new URL(`../famidash-routes/${options.side}.json`, import.meta.url)));
const bounds = new Map(), references = {}, m = await machine(romPath, runtimePath), powerOn = m.core.capture();
for (const [level] of layouts) {
  try {
    const tape = routeTape(routes.routes[level]);
    m.boot(options.side, level);
    const bands = {};
    let complete = false, interiorFrames = 0;
    for (const buttons of tape) {
      m.core.step(buttons, 1);
      const o = m.observe();
      if (o.dead || o.state !== 2) { complete = o.state === 3; break; }
      const tile = tileAt(layouts.get(level), o.x + 8, o.y + 8), definition = tile && index.metatiles[tile.id];
      if (o.x >= 128 && definition?.collision === "COL_NONE"
          && ["CHECKERBOARD_15", "X_NO_OUTLINE", "BLACK_NONE"].includes(definition.name)) interiorFrames++;
      const band = Math.floor(o.x / 64), b = bands[band] ??= { min: Infinity, max: -Infinity };
      b.min = Math.min(b.min, o.y);
      b.max = Math.max(b.max, o.y);
    }
    references[level] = { complete, interior_frames: interiorFrames, used_for_bounds: complete && interiorFrames === 0 };
    if (complete && interiorFrames === 0) bounds.set(level, bands);
  } catch { references[level] = { status: "missing-or-assisted" }; }
}
const bindings = await import(pathToFileURL(resolve(bindingsPath)));
await bindings.default({ module_or_path: await readFile(wasmPath) });
m.boot(options.side, options.level);
const prefixTape = options.checkpoint ? routeTape(routes.routes[options.level]).slice(0, options.checkpoint) : [];
if (prefixTape.length !== options.checkpoint) throw new Error("checkpoint exceeds the control route");
for (const buttons of prefixTape) {
  m.core.step(buttons, 1);
  const o = m.observe();
  if (o.dead || o.state !== 2) throw new Error("checkpoint is outside live gameplay");
}
const root = m.core.capture(), rom = await readFile(romPath);
globalThis.harmonyCores = { 0: {
  ...m.core, rom,
  reset: () => m.core.restore(powerOn),
  memory: () => m.ram().slice(), ramView: m.ram,
  frames(buttons, frames) {
    const data = new Uint8Array(frames * 2048);
    for (let i = 0; i < frames; i++) { m.core.step(buttons, 1); data.set(m.ram(), i * 2048); }
    return data;
  },
} };
const game = options.side === "main" ? (options.level >= 22 ? "famidash-community" : "famidash") : `famidash-${options.side}`;
const identity = `quicknes-browser:${m.core.revision}`;
const search = new bindings.CampaignSearch(game, 0, rom, identity, m.core_sha256,
  options.seed, root, JSON.stringify(compress(prefixTape)), 512, options.executions, false);
const worker = new bindings.CampaignWorker(game, 0, rom, identity, m.core_sha256);
await mkdir(outputPath, { recursive: true });
const report = {
  schema: 1, engine: "dissonance-campaign", options, references,
  runner_sha256: hash(await readFile(new URL(import.meta.url))),
  rom_sha256: m.rom_sha256, core_sha256: m.core_sha256,
  search_wasm_sha256: hash(await readFile(wasmPath)), bindings_sha256: hash(await readFile(bindingsPath)),
  source_index_sha256: hash(indexBytes), executions: 0, counts: {}, max_x: {}, inspected: 0,
  unavailable_snapshots: 0, hints: {}, rejected_continuations: {}, replay_mismatches: 0, findings: [],
};
const seen = new Set();
function classify(o) {
  const layout = layouts.get(o.level);
  if (!layout || o.x < 128 || o.dead || o.debug || o.invincible || o.paused || o.dual || o.state !== 2) return null;
  const tile = tileAt(layout, o.x + 8, o.y + 8), definition = tile && index.metatiles[tile.id];
  if (definition?.collision === "COL_NONE" && ["CHECKERBOARD_15", "X_NO_OUTLINE", "BLACK_NONE"].includes(definition.name))
    return { kind: "decorative-interior", ...tile, tile_name: definition.name };
  const localX = (o.x + 8) % 16, localY = (o.y + 8) % 16;
  if (definition?.collision === "COL_ALL" && localX >= 3 && localX <= 12 && localY >= 3 && localY <= 12)
    return { kind: "solid-overlap", ...tile, tile_name: definition.name };
  if (definition?.collision === "COL_DEATH" && localX >= 4 && localX <= 8 && localY >= 4 && localY <= 11)
    return { kind: "hazard-overlap", ...tile, tile_name: definition.name };
  let min = Infinity, max = -Infinity;
  const bands = bounds.get(o.level);
  for (let b = Math.floor(o.x / 64) - 1; b <= Math.floor(o.x / 64) + 1; b++) {
    min = Math.min(min, bands?.[b]?.min ?? Infinity);
    max = Math.max(max, bands?.[b]?.max ?? -Infinity);
  }
  if (Number.isFinite(max) && o.y > max + 32) return { kind: "below-reference", reference: max, ...tile };
  if (Number.isFinite(min) && o.y < min - 32) return { kind: "above-reference", reference: min, ...tile };
  return null;
}
function replay(actions, stopAtComplete = false) {
  m.boot(options.side, options.level);
  const trace = [];
  for (const { buttons, frames } of actions) {
    if (!Number.isInteger(buttons) || buttons < 0 || buttons > 255 || !Number.isInteger(frames) || frames < 1 || frames > 200000)
      throw new Error("invalid campaign action");
    for (let i = 0; i < frames; i++) {
      m.core.step(buttons, 1);
      const o = m.observe();
      if (o.state === 2 && (o.dead || o.debug || o.paused)) throw new Error("campaign replay died or enabled debug/pause");
      trace.push(o);
      if (stopAtComplete && o.state === 3 && o.level === options.level) return trace;
    }
  }
  return trace;
}
campaign: for (let execution = 1; execution <= options.executions; execution++) {
  const job = search.plan();
  if (!job) break;
  search.admit(worker.run(job), worker.note());
  report.executions = execution;
  if (execution % 32 && execution !== options.executions) continue;
  const batch = JSON.parse(search.take_batch());
  report.search_stats = { states: batch.states, frames: batch.frames, deaths: batch.deaths, won: batch.won, stopped: batch.stopped };
  for (const point of batch.points) {
    const level = point.observation.level;
    report.counts[level] = (report.counts[level] ?? 0) + 1;
    if (point.retained === null || point.observation.loading || !layouts.has(level)) continue;
    let snapshot;
    try { snapshot = search.snapshot(point.retained); } catch { report.unavailable_snapshots++; continue; }
    m.core.restore(snapshot);
    report.inspected++;
    const original = m.observe(), signal = classify(original);
    report.max_x[level] = Math.max(report.max_x[level] ?? -Infinity, original.x);
    if (options.clear && level > options.level) {
      const state = JSON.parse(search.state(point.retained)), trace = replay(state.actions, true);
      if (!trace.some(o => o.level === options.level && o.state === 3)) throw new Error("clearing path did not complete");
      const tape = state.actions.flatMap(a => Array(a.frames).fill(a.buttons)).slice(0, trace.length);
      const file = "clearing-route.json.gz", bytes = gzipSync(JSON.stringify({ options, actions: compress(tape), trace,
        rom_sha256: m.rom_sha256, core_sha256: m.core_sha256, engine: report.engine, executions: execution }));
      await writeFile(join(outputPath, file), bytes);
      report.clearing = { file, sha256: hash(bytes), frames: trace.length };
      break campaign;
    }
    if (!signal) continue;
    report.hints[signal.kind] = (report.hints[signal.kind] ?? 0) + 1;
    const key = `${level}:${signal.kind}:${Math.floor(original.x / 128)}`;
    if (seen.has(key)) continue;
    const tail = signal.kind.endsWith("overlap") ? 16 : options.tail;
    let alive = 0, remains = 0;
    for (; alive < tail; alive++) {
      m.core.step(0, 1);
      const o = m.observe();
      if (o.dead || o.state !== 2 || o.level !== original.level) break;
      if (classify(o)?.kind === signal.kind) remains++;
    }
    if (alive !== tail || (!signal.kind.endsWith("overlap") && remains < Math.floor(tail * 0.8))) {
      report.rejected_continuations[signal.kind] = (report.rejected_continuations[signal.kind] ?? 0) + 1;
      continue;
    }
    const endpoint = m.observe(), state = JSON.parse(search.state(point.retained));
    let trace;
    try { trace = replay(state.actions); } catch (error) {
      report.replay_mismatches++;
      report.first_replay_mismatch ??= { error: error.message, original, state };
      continue;
    }
    if (JSON.stringify(trace.at(-1)) !== JSON.stringify(original)) {
      report.replay_mismatches++;
      report.first_replay_mismatch ??= { original, actual: trace.at(-1), state };
      continue;
    }
    const actions = [...state.actions, { buttons: 0, frames: tail }];
    trace = replay(actions);
    if (JSON.stringify(trace.at(-1)) !== JSON.stringify(endpoint)) throw new Error("continuation replay differs");
    const witness = { schema: 1, engine: report.engine, options, executions: execution, archive_id: point.retained, boot_level: options.level,
      target_level: level, signal, original, endpoint, first_suspect_frame: trace.length - tail,
      actions, trace, rom_sha256: m.rom_sha256, core_sha256: m.core_sha256,
      search_wasm_sha256: report.search_wasm_sha256, source_index_sha256: report.source_index_sha256 };
    const file = `case-${report.findings.length}.json.gz`, bytes = gzipSync(JSON.stringify(witness));
    await writeFile(join(outputPath, file), bytes);
    report.findings.push({ file, sha256: hash(bytes), level, signal, endpoint, frames: trace.length });
    seen.add(key);
    console.log("candidate", level, signal.kind, original.x, original.y, file);
  }
  if (execution % 4096 === 0) {
    console.log("progress", execution, JSON.stringify(report.counts), "candidates", report.findings.length);
    await writeFile(join(outputPath, "report.json"), JSON.stringify(report, null, 2) + "\n");
  }
}
await writeFile(join(outputPath, "report.json"), JSON.stringify(report, null, 2) + "\n");
console.log("finished", report.executions, "candidates", report.findings.length);
