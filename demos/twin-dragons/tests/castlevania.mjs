// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { homedir } from "node:os";
import castlevania, { buildMap, buildSprites, MAP_WIDTHS } from "../src/games/castlevania.js";
import { createEngine } from "../src/emulator.js";

const path = process.env.CASTLEVANIA_ROM || `${homedir()}/workspace/harmony/.arcade-local/roms/castlevania.nes`;
let rom;
try {
  rom = new Uint8Array(await readFile(path));
} catch {
  console.log(`Skipping Castlevania: no ROM at ${path}. Set CASTLEVANIA_ROM to run it.`);
  process.exit(0);
}
const base = new URL("../public/", import.meta.url);
const engine = await createEngine(base, castlevania, { rom, wasmBinary: await readFile(new URL("engine/quicknes.wasm", base)) });

engine.boot(0);
const start = castlevania.decode(engine.ramView());
assert.equal(start.stage, 0, "A new game starts in the courtyard before stage 1");
console.log("Castlevania boots into the courtyard.");

for (const level of [0, 1, 9]) {
  const map = buildMap(engine, level);
  assert.equal(castlevania.decode(engine.ramView()).stage, level + 1, "The walk leaves the engine in the stage it mapped");
  assert.equal(map.width, MAP_WIDTHS[level], `${castlevania.catalog.maps[level].name} is ${MAP_WIDTHS[level]} pixels wide`);
  assert.equal(map.data.length, map.width * map.height * 4);
}
console.log("Stage maps build from the ROM at their measured widths.");

const sheet = buildSprites(engine);
assert.ok(sheet.data.some((v, i) => i % 4 === 3 && v > 0), "The sprite sheet holds Simon");
console.log("The sprite sheet builds from the ROM.");
