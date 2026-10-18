// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { readFile, writeFile } from "node:fs/promises";
import { gzipSync } from "node:zlib";
import { machine, hash, expand, compress, replay } from "./runtime.mjs";
import { symbolMachine } from "./symbol-runtime.mjs";

const [inputPath, reportPath, rom, runtime, output] = process.argv.slice(2);
if (!output) throw new Error("export-control.mjs CLEARING_INPUT REPORT ROM RUNTIME OUTPUT.json.gz");
const inputBytes = await readFile(inputPath), reportBytes = await readFile(reportPath);
const input = JSON.parse(inputBytes), report = JSON.parse(reportBytes);
assert.equal(input.engine, "dissonance-wasm-single-level");
assert.equal(report.engine, input.engine);
assert.ok(report.cleared);
const m = process.env.FAMIDASH_SYMBOLS
  ? await symbolMachine(rom, runtime, process.env.FAMIDASH_SYMBOLS) : await machine(rom, runtime);
assert.equal(m.rom_sha256, report.rom_sha256);
assert.equal(m.core_sha256, report.core_sha256);
const trace = await replay(m, report.side, report.level, expand(input.actions));
assert.equal(trace.at(-1).state, 3, "control must complete in a fresh ordinary-input replay");
assert.ok(!trace.some(o => o.dead || o.debug || o.paused));
const witness = { schema: 1, engine: "dissonance-campaign", purpose: "clearing-control",
  discovery_engine: input.engine, discovery: { input_sha256: hash(inputBytes), report_sha256: hash(reportBytes),
    seed: input.seed, executions: input.executions, root: input.root, search_wasm_sha256: report.search_wasm_sha256 },
  options: { side: report.side }, boot_level: report.level, target_level: report.level,
  rom_sha256: m.rom_sha256, core_sha256: m.core_sha256, actions: compress(expand(input.actions).slice(0, trace.length)), trace };
if (m.symbols) witness.target_layout = { kind: "symbol-based linear coordinates", symbols: m.symbols,
  debug_sha256: m.debug_sha256, startup_released_frames: 180 };
await writeFile(output, gzipSync(JSON.stringify(witness)));
console.log(JSON.stringify({ output, frames: trace.length, endpoint: trace.at(-1), sha256: hash(await readFile(output)) }));
