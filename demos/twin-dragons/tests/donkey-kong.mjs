// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { homedir } from "node:os";
import donkeyKong, { buildMap, buildSprites, STAGES } from "../src/games/donkey-kong.js";
import { createEngine } from "../src/emulator.js";
import { snapshotDigest } from "../src/media.js";
import { campaignSearch, campaignWorker, coreIdentity, runCampaign } from "../src/campaign.js";

const path = process.env.DONKEY_KONG_ROM || `${homedir()}/workspace/roms/donkey-kong.nes`;
let rom;
try {
  rom = new Uint8Array(await readFile(path));
} catch {
  console.log(`Skipping Donkey Kong: no ROM at ${path}. Set DONKEY_KONG_ROM to run it.`);
  process.exit(0);
}
const base = new URL("../public/", import.meta.url);
const engine = await createEngine(base, donkeyKong, { rom, wasmBinary: await readFile(new URL("engine/quicknes.wasm", base)) });

for (let level = 0; level < donkeyKong.levelCount; level++) {
  engine.boot(level);
  const o = engine.observation();
  assert.equal(o.level, level, `${donkeyKong.catalog.levels[level].label} boots on its own level`);
  assert.equal(o.loading, false);
  assert.equal(o.health, 1);
  assert.equal(o.cleared, level);
  assert.equal(o.round, 0);
  assert.equal(engine.ramView()[0x53], STAGES[level]);
}
console.log("Each of the 3 stages boots into play.");

for (let level = 0; level < donkeyKong.levelCount; level++) {
  const map = buildMap(engine, level);
  assert.equal(map.data.length, 256 * 224 * 4);
  assert.ok(map.data.some((v, i) => i % 4 === 0 && v > 0), `${donkeyKong.catalog.levels[level].label} map has pixels`);
}
const sheet = buildSprites(engine);
assert.ok(sheet.data.some((v, i) => i % 4 === 3 && v > 0), "The sprite sheet holds Mario");
console.log("Stage maps and the sprite sheet build from the ROM.");

const identity = coreIdentity("browser"),
  worker = campaignWorker(engine, identity),
  root = engine.boot(0),
  search = campaignSearch(engine, identity, { seed: 3, executions: 3000 });
let highest = null;
while (runCampaign(search, worker, { jobs: 16 }))
  for (const point of JSON.parse(search.take_batch()).points)
    if (point.retained !== null && (!highest || point.observation.y < highest.observation.y)) highest = point;
assert.ok(highest, "The search retains states");
const state = JSON.parse(search.state(highest.retained));
engine.restore(root);
for (const action of state.actions) engine.run(action.buttons, action.frames);
assert.equal(snapshotDigest(engine.capture()), search.digest(highest.retained), "The archived state replays exactly from its controller history");
assert.deepEqual(engine.observation(), state.observation, "The replayed state decodes to the archived observation");
console.log(`Barrels: highest state reached y=${highest.observation.y}; exact replay passed.`);
search.free();
