// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from 'node:assert/strict';
import { snapshotDigest } from '../src/media.js';
import { readFile } from 'node:fs/promises';
import { createEngine } from '../src/emulator.js';
import { isMapEvidence } from '../src/world.js';
import { RoundSearch, ROUND_PATHS } from '../src/rollout.js';
import init, { Explorer } from '../rust/pkg/twin_dragons_browser.js';
const base = new URL('../public/', import.meta.url);
const engine = await createEngine(base, {
  rom: await readFile(new URL('twin-dragons.nes', base)),
  wasmBinary: await readFile(new URL('engine/quicknes.wasm', base)),
});
globalThis.harmonyEngine = engine;
await init({module_or_path: await readFile(new URL('../rust/pkg/twin_dragons_browser_bg.wasm', import.meta.url))});
const catalog = JSON.parse(await readFile(new URL('maps.json', base)));
assert.throws(() => engine.boot(-1), /Unknown/);
assert.throws(() => engine.boot(3), /Unknown/);
assert.equal(catalog.levels.length, 3);
for (const level of catalog.levels) {
  const root = engine.boot(level.id), observation = engine.observation();
  assert.equal(observation.level, level.id);
  assert.equal(observation.health, 3);
  assert.equal(observation.cleared, level.id);
  assert.equal(observation.loading, false);
  assert.equal(observation.on_ground, true);
  assert.ok(isMapEvidence(observation, catalog.levels));
  const search = level.id ? Explorer.from_history(2, '[]') : new Explorer(2);
  search.set_snapshot_budget(4 * 1048576);
  const rounds = new RoundSearch(search, [engine]);
  let batch;
  for (let n = 0; n < 2; n++) batch = rounds.round().batch;
  assert.equal(batch.executions, 2 * ROUND_PATHS, `Level ${level.id} must actually search`);
  const id = batch.points.findLast(p => p.retained !== null)?.retained ?? 0;
  const state = JSON.parse(search.state(id)), snapshot = search.digest(id);
  assert.deepEqual(engine.boot(level.id), root, 'Repeated bootstrap is byte deterministic');
  for (const action of state.actions) engine.run(action.buttons, action.frames);
  assert.equal(snapshotDigest(engine.capture()), snapshot, `Level ${level.id} archive must replay exactly`);
  const child = Explorer.from_history(3, JSON.stringify(state.actions));
  assert.equal(child.digest(0), snapshot, 'Fork is rooted at the replayed endpoint');
  assert.equal(new RoundSearch(child, [engine]).round().batch.executions, ROUND_PATHS);
  child.free(); search.free();
}
console.log('All 3 level roots: healthy authentic boot, fresh search, exact archived replay, repeated warp and rooted fork passed.');
