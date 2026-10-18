// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { gunzipSync, gzipSync } from "node:zlib";
import { compress, expand, hash, machine, replay, routeFloor, routeTape, suspect } from "./runtime.mjs";

const json = async (path) => {
  const bytes = await readFile(path);
  return JSON.parse(String(path).endsWith(".gz") ? gunzipSync(bytes) : bytes.toString());
};
const save = async (path, value) => {
  const bytes = Buffer.from(JSON.stringify(value, null, 2) + "\n");
  return writeFile(path, String(path).endsWith(".gz") ? gzipSync(bytes) : bytes);
};
const traceHash = (trace) => hash(JSON.stringify(trace));

function rng(seed) {
  let state = seed >>> 0;
  return (n) => {
    state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
    return Math.floor((state / 4294967296) * n);
  };
}

async function control(m, layout, tape) {
  m.boot(layout.side, layout.level);
  const trace = [], checkpoints = [], bands = {};
  for (let frame = 0; frame < tape.length; frame++) {
    if (frame % 16 === 0) checkpoints.push({ frame, state: m.core.capture(), observation: m.observe() });
    m.core.step(tape[frame], 1);
    const o = m.observe();
    trace.push(o);
    if (o.dead || o.debug || o.paused) throw new Error(`control fails at frame ${frame + 1}`);
    if (o.state === 3) return { trace, checkpoints, bands, tape: tape.slice(0, frame + 1) };
    if (o.state !== 2) throw new Error(`control leaves gameplay at frame ${frame + 1}`);
    const band = Math.floor(o.x / 64);
    bands[band] = Math.max(bands[band] ?? -Infinity, o.y);
  }
  throw new Error("route ends without a real level completion");
}

async function scan(indexPath, romDirectory, runtime, output, options) {
  const index = await json(indexPath), report = { schema: 1, source_revision: index.source_revision,
    detector: "below-route-decoration-v1", options, results: [] };
  await mkdir(output, { recursive: true });
  const machines = new Map();
  for (const layout of index.levels) {
    if (options.side && layout.side !== options.side) continue;
    if (options.levels && !options.levels.includes(layout.level)) continue;
    const result = { side: layout.side, level: layout.level, name: layout.name, findings: [], trials: 0 };
    try {
      if (!machines.has(layout.rom)) machines.set(layout.rom, await machine(join(romDirectory, layout.rom), runtime));
      const m = machines.get(layout.rom);
      assert.equal(m.rom_sha256, layout.rom_sha256, "ROM must match the source release");
      const routes = await json(new URL(`../famidash-routes/${layout.side}.json`, import.meta.url));
      const baseline = await control(m, layout, routeTape(routes.routes[layout.level]));
      result.control_frames = baseline.trace.length;
      result.control_sha256 = traceHash(baseline.trace);
      const rand = rng(options.seed + layout.level + ["main", "b", "c", "d"].indexOf(layout.side) * 1000);
      const clustered = new Set();
      search: for (let trial = 0; trial < options.trials; trial++) {
        result.trials++;
        const start = baseline.checkpoints[rand(baseline.checkpoints.length)];
        m.core.restore(start.state);
        const suffix = [];
        for (let chunk = 0; chunk < 30; chunk++) {
          const buttons = rand(2), hold = rand(40) + 1;
          for (let i = 0; i < hold; i++) {
            m.core.step(buttons, 1);
            suffix.push(buttons);
            const o = m.observe();
            if (o.dead || o.state !== 2) break;
            const signal = suspect(o, layout, index.metatiles, baseline.bands);
            if (!signal || clustered.has(Math.floor(o.x / 256))) continue;
            const state = m.core.capture(), tail = [];
            let visits = 0;
            for (let frame = 0; frame < 150; frame++) {
              m.core.step(0, 1);
              const next = m.observe();
              if (next.dead || next.state !== 2 || next.debug || next.invincible || next.paused) break;
              tail.push(0);
              if (suspect(next, layout, index.metatiles, baseline.bands)) visits++;
            }
            const endpoint = m.observe();
            if (tail.length !== 150 || visits < 120 || endpoint.x - o.x < 64) {
              m.core.restore(state);
              continue;
            }
            const tape = [...baseline.tape.slice(0, start.frame), ...suffix, ...tail],
              trace = await replay(m, layout.side, layout.level, tape);
            assert.equal(trace.length, tape.length, "witness must survive a fresh replay");
            assert.deepEqual(trace.at(-1), endpoint, "fresh replay must match the search endpoint");
            const first = trace.findIndex((point) => suspect(point, layout, index.metatiles, baseline.bands));
            const name = `${layout.side}-${layout.level}-${result.findings.length + 1}.json.gz`,
              witness = { schema: 1, status: "candidate", detector: report.detector,
                source_revision: index.source_revision, rom_sha256: m.rom_sha256,
                quicknes_wasm_sha256: m.core_sha256, seed: options.seed, trial,
                layout, metatiles: index.metatiles, bands: baseline.bands,
                actions: compress(tape), trace, trace_sha256: traceHash(trace),
                first_suspect_frame: first + 1, signal,
                control_actions: compress(baseline.tape), control_frames: baseline.trace.length,
                control_after_intro_sha256: traceHash(baseline.trace.slice(40)),
                control_sha256: traceHash(baseline.trace) };
            await save(join(output, name), witness);
            result.findings.push({ file: name, first_suspect_frame: first + 1, endpoint, signal });
            clustered.add(Math.floor(o.x / 256));
            if (result.findings.length >= options.maxFindings) break search;
            // Fresh replay replaced the search machine's state; this trial is finished.
            continue search;
          }
          if (m.observe().dead || m.observe().state !== 2) break;
        }
      }
      result.status = result.findings.length ? "candidates" : "no-candidate-in-budget";
    } catch (error) {
      result.status = "unassessed";
      result.reason = error.message;
    }
    report.results.push(result);
    await save(join(output, "report.json"), report);
    console.log(`${layout.side}:${layout.level} ${layout.name}: ${result.status}; ${result.findings.length} candidates; ${result.trials} trials${result.reason ? `; ${result.reason}` : ""}`);
  }
  return report;
}

async function check(casePath, rom, runtime, fixed) {
  const witness = await json(casePath), m = await machine(rom, runtime),
    trace = await replay(m, witness.layout.side, witness.layout.level, expand(witness.actions));
  assert.equal(m.core_sha256, witness.quicknes_wasm_sha256, "use the recorded QuickNES runtime");
  if (!fixed) {
    assert.equal(m.rom_sha256, witness.rom_sha256);
    assert.equal(traceHash(trace), witness.trace_sha256, "all recorded mechanical fields must replay");
    console.log(`Original witness reproduced: ${trace.length} frames, (${trace.at(-1).x}, ${trace.at(-1).y}).`);
  } else {
    assert(trace.at(-1).dead, "a level-data patch must reject this input with a death");
    assert(trace.length > witness.first_suspect_frame - 64, "patch must not kill the input far before the reported region");
    assert.deepEqual(trace.slice(0, -1), witness.trace.slice(0, trace.length - 1), "level-data patch must preserve the approach to the pit");
    console.log(`Patched witness dies at frame ${trace.length}, (${trace.at(-1).x}, ${trace.at(-1).y}).`);
  }
  const winning = await replay(m, witness.layout.side, witness.layout.level, expand(witness.control_actions));
  assert.equal(winning.at(-1).state, 3, "the clearing control must still complete");
  assert.equal(winning.length, witness.control_frames, "the control's timing must stay the same");
  assert.equal(traceHash(winning), witness.control_sha256, "the control's mechanical trace must stay the same");
  console.log(`Control completes with its original mechanical trace at frame ${winning.length}.`);
}

const [command, ...args] = process.argv.slice(2);
if (command === "scan") {
  const [index, roms, runtime, output, ...flags] = args;
  if (!output || flags.length % 2) throw new Error("scan INDEX ROM_DIRECTORY RUNTIME_DIRECTORY OUTPUT [--side SIDE] [--levels 1,2] [--trials 500] [--seed 1] [--max-findings 1]");
  const values = Object.fromEntries(Array.from({ length: flags.length / 2 }, (_, i) => [flags[i * 2], flags[i * 2 + 1]]));
  for (const key of Object.keys(values)) assert(["--side", "--levels", "--trials", "--seed", "--max-findings"].includes(key), `unknown option ${key}`);
  const options = { side: values["--side"] ?? null, levels: values["--levels"]?.split(",").map(Number) ?? null,
    trials: Number(values["--trials"] ?? 500), seed: Number(values["--seed"] ?? 1), maxFindings: Number(values["--max-findings"] ?? 1) };
  assert(!options.side || ["main", "b", "c", "d"].includes(options.side));
  for (const key of ["trials", "seed", "maxFindings"]) assert(Number.isSafeInteger(options[key]) && options[key] >= (key === "seed" ? 0 : 1));
  await scan(resolve(index), resolve(roms), resolve(runtime), resolve(output), options);
} else if (command === "replay") {
  const [witness, rom, runtime, flag] = args;
  if (!runtime || (flag && flag !== "--fixed")) throw new Error("replay CASE ROM RUNTIME_DIRECTORY [--fixed]");
  await check(witness, rom, runtime, flag === "--fixed");
} else throw new Error("Choose scan or replay");
