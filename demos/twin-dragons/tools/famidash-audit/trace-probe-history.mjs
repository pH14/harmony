// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { readFile, writeFile } from "node:fs/promises";
import { gunzipSync } from "node:zlib";
import { machine, expand, hash } from "./runtime.mjs";
import { symbolMachine } from "./symbol-runtime.mjs";

const [casePath, rom, debugPath, runtime, startText, endText, output] = process.argv.slice(2);
if (!output) throw new Error("trace-probe-history.mjs CASE ROM DBG RUNTIME START END OUTPUT");
const start = Number(startText), end = Number(endText);
assert.ok(Number.isSafeInteger(start) && Number.isSafeInteger(end) && start >= 1 && end >= start);
const caseBytes = await readFile(casePath), debugBytes = await readFile(debugPath);
const witness = JSON.parse(casePath.endsWith(".gz") ? gunzipSync(caseBytes) : caseBytes);
const symbols = new Map();
for (const line of debugBytes.toString().split("\n")) {
  if (!line.startsWith("sym\t")) continue;
  const name = line.match(/name="([^"]+)"/), value = line.match(/(?:^|,)val=0x([a-f0-9]+)/i);
  if (name && value) symbols.set(name[1], Number.parseInt(value[1], 16));
}
const arrays = ["pit_previous_room", "pit_previous_mode", "pit_previous_mini", "pit_previous_x",
  "pit_previous_y", "pit_previous_collision", "pit_previous_metatile"];
const scalars = ["ss_dx", "ss_dy", "ss_origin_x", "ss_origin_y", "ss_type", "ss_metatile",
  "ss_base_remaining", "ss_base_axis", "ss_base_open", "ss_x0", "ss_x1", "ss_y0", "ss_y1"];
for (const name of [...arrays, ...scalars]) {
  const address = symbols.get("_" + name);
  assert.ok(Number.isInteger(address) && address >= 0 && address + (arrays.includes(name) ? 2 : 1) <= 2048,
    "missing or invalid RAM symbol: " + name);
}
const m = witness.target_layout ? await symbolMachine(rom, runtime, debugPath) : await machine(rom, runtime);
assert.equal(m.rom_sha256, witness.rom_sha256);
assert.equal(m.core_sha256, witness.core_sha256);
if (witness.target_layout) assert.equal(m.debug_sha256, witness.target_layout.debug_sha256);
const tape = expand(witness.actions);
assert.equal(tape.length, witness.trace.length);
assert.ok(end <= tape.length);
const playerX = symbols.get("_player_relx") ?? symbols.get("_player_x");
const playerY = symbols.get("_player_rely") ?? symbols.get("_player_y");
const scrollX = symbols.get("_scroll_x");
for (const address of [playerX, playerY, scrollX])
  assert.ok(Number.isInteger(address) && address >= 0 && address + 4 <= 2048, "missing player-position RAM symbol");
m.boot(witness.options.side, witness.boot_level, witness.options.menu_delay ?? 0,
  witness.options.startup_released_frames);
const rows = [];
for (let i = 0; i < tape.length; i++) {
  m.core.step(tape[i], 1);
  const observation = m.observe();
  assert.deepEqual(observation, witness.trace[i], "witness differs at frame " + (i + 1));
  if (i + 1 < start || i + 1 > end) continue;
  const ram = m.ram(), values = {};
  for (const name of arrays) values[name] = Array.from(ram.subarray(symbols.get("_" + name), symbols.get("_" + name) + 2));
  for (const name of scalars) values[name] = ram[symbols.get("_" + name)];
  const players = [{ player: 0, x: observation.x, y: observation.y }];
  if (observation.dual) {
    const view = new DataView(ram.buffer, ram.byteOffset);
    players.push({ player: 1, x: view.getInt32(scrollX, true) + ram[playerX + 3],
      y: observation.camera + ram[playerY + 3] });
  }
  rows.push({ frame: i + 1, observation, players, values });
}
await writeFile(output, JSON.stringify({ schema: 2, engine: "read-only QuickNES RAM after verified witness frames",
  limitation: "Engine history and scratch state, not an independent geometry oracle; scratch values may be stale.",
  case_sha256: hash(caseBytes), debug_sha256: hash(debugBytes), rom_sha256: m.rom_sha256,
  core_sha256: m.core_sha256, verified_frames: tape.length, rows }, null, 2) + "\n");
console.log(JSON.stringify({ output, verified_frames: tape.length, sampled_frames: rows.length }));
