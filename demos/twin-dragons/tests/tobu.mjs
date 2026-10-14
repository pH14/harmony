// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import tobu from "../src/games/tobu-tobu-girl.js";
import { createEngine } from "../src/emulator.js";
import { isMapEvidence } from "../src/world.js";
import { snapshotDigest } from "../src/media.js";
import { RoundSearch, ROUND_PATHS } from "../src/rollout.js";
import init, { Explorer } from "../rust/pkg/twin_dragons_browser.js";

const base = new URL("../public/", import.meta.url),
  assets = new URL(tobu.assets, base);
const engine = await createEngine(base, tobu, {
  rom: await readFile(new URL(tobu.rom.file, base)),
  wasmBinary: await readFile(new URL(`engine/${tobu.core}.wasm`, base)),
});
globalThis.harmonyEngine = engine;
await init({ module_or_path: await readFile(new URL("../rust/pkg/twin_dragons_browser_bg.wasm", import.meta.url)) });
const catalog = JSON.parse(await readFile(new URL("maps.json", assets)));

for (let level = 0; level < tobu.levelCount; level++) {
  engine.boot(level);
  const o = engine.observation();
  assert.equal(o.level, level);
  assert.equal(o.loading, false);
  assert.equal(o.time, 32);
  assert.ok(isMapEvidence(o, catalog.levels));
  const map = catalog.maps[level];
  assert.ok(o.x >= 0 && o.x < map.width && o.y > map.height - 160 && o.y < map.height, `Level ${level + 1} starts at the bottom of its map`);
}
console.log("Each level boots into play with a full timer at the bottom of its map.");

const win = process.env.TOBU_WIN === "1";
const target = win ? tobu.levelCount : 1,
  budget = win ? 40000 : 12000,
  root = engine.boot(0);
for (const seed of [27]) {
  engine.restore(root);
  const search = new Explorer(seed),
    rounds = new RoundSearch(search, [engine]);
  let found = null, tries = 0;
  for (; tries < budget && !found; tries += ROUND_PATHS) {
    const { batch } = rounds.round();
    for (const point of batch.points)
      if (point.retained !== null && point.observation.cleared >= target) found = point;
  }
  assert.ok(found, `Seed ${seed} reaches ${win ? "the ending" : "Level 2"} within ${budget} paths`);
  const state = JSON.parse(search.state(found.retained));
  engine.restore(root);
  for (const action of state.actions) engine.run(action.buttons, action.frames);
  assert.equal(snapshotDigest(engine.capture()), search.digest(found.retained), "The archived state replays exactly from its controller history");
  assert.ok(state.actions.some((a) => a.buttons & 8), "The history includes the level transition's Start presses");
  const decoded = engine.observation();
  for (const key of Object.keys(decoded)) assert.equal(decoded[key], state.observation[key], `Browser and Rust decoders agree on ${key}`);
  console.log(`Seed ${seed}: ${win ? "ending" : "Level 2"} at ${tries} paths; exact replay passed.`);
  search.free();
}
