// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { readFile, writeFile } from "node:fs/promises";
import { gunzipSync, gzipSync } from "node:zlib";
import { machine, hash, expand, compress, replay } from "./runtime.mjs";
import { symbolMachine } from "./symbol-runtime.mjs";
import { players } from "./signals.mjs";

const [input, rom, runtime, firstText, output, tailText = "0"] = process.argv.slice(2);
if (!output) throw new Error("export-witness.mjs CANDIDATE ROM RUNTIME FIRST_SUSPECT_FRAME OUTPUT [RELEASED_TAIL]");
const bytes = await readFile(input), candidate = JSON.parse(gunzipSync(bytes));
const m = process.env.FAMIDASH_SYMBOLS ? await symbolMachine(rom, runtime, process.env.FAMIDASH_SYMBOLS) : await machine(rom, runtime);
assert.equal(m.rom_sha256, candidate.rom_sha256);
assert.equal(m.core_sha256, candidate.core_sha256);
const tape = expand(candidate.actions), tail = Number(tailText), first = Number(firstText);
assert.ok(Number.isSafeInteger(tail) && tail >= 0 && tail <= 20000);
assert.ok(Number.isSafeInteger(first) && first > 0 && first <= tape.length);
const trace = await replay(m, candidate.side, candidate.level, tape, candidate.setup?.menu_delay ?? 0, candidate.setup?.startup_released_frames);
assert.equal(trace.length, tape.length, "candidate input must reach its saved endpoint");
const withoutPlayer = o => Object.fromEntries(Object.entries(o).filter(([k]) => k !== "player"));
assert.deepEqual(withoutPlayer(players(m)[candidate.player ?? 0]), withoutPlayer(candidate.observation));
for (let i = 0; i < tail && !trace.at(-1).dead && trace.at(-1).state === 2; i++) {
  m.core.step(0, 1);
  const observation = m.observe();
  assert.ok(!observation.debug && !observation.paused);
  trace.push(observation);
}
const witness = { schema: 1, engine: "dissonance-campaign", discovery_engine: candidate.engine,
  discovery: { input, candidate_sha256: hash(bytes), round: candidate.round, seed: candidate.seed,
    executions: candidate.executions, root: candidate.root, player: candidate.player ?? 0,
    search_wasm_sha256: candidate.search_wasm_sha256 },
  options: { side: candidate.side, menu_delay: candidate.setup?.menu_delay ?? 0, startup_released_frames: candidate.setup?.startup_released_frames,
    fceumm_startup_released_frames: candidate.setup?.startup_released_frames === 22 ? 180 : candidate.setup?.startup_released_frames }, boot_level: candidate.level, target_level: candidate.level,
  rom_sha256: m.rom_sha256, core_sha256: m.core_sha256, first_suspect_frame: first,
  actions: compress(tape.concat(Array(tail).fill(0)).slice(0, trace.length)), trace };
if (m.symbols) witness.target_layout = { kind: "symbol-based linear coordinates", symbols: m.symbols,
  debug_sha256: m.debug_sha256, startup_released_frames: 180 };
await writeFile(output, gzipSync(JSON.stringify(witness)));
console.log(JSON.stringify({ output, frames: trace.length, endpoint: trace.at(-1), sha256: hash(await readFile(output)) }));
