// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from 'node:assert/strict';
import { snapshotDigest } from '../src/media.js';
import { readFile } from 'node:fs/promises';
import { createEngine } from '../src/emulator.js';
import { isMapEvidence } from '../src/world.js';
import init, { Explorer } from '../rust/pkg/nova_browser.js';
const base = new URL('../public/', import.meta.url);
const engine = await createEngine(base, {
  rom: await readFile(new URL('nova.nes', base)),
  wasmBinary: await readFile(new URL('engine/quicknes.wasm', base)),
});
globalThis.harmonyEngine = engine;
await init({module_or_path: await readFile(new URL('../rust/pkg/nova_browser_bg.wasm', import.meta.url))});
const catalog = JSON.parse(await readFile(new URL('maps.json', base)));
assert.throws(() => engine.boot(-1), /Unknown/);
assert.throws(() => engine.boot(44), /Unknown/);
for (const level of catalog.levels) {
  const root = engine.boot(level.id), observation = engine.observation();
  assert.equal(observation.selected_level, level.id);
  assert.equal(observation.checkpoint_level, level.id);
  assert.equal(observation.health, 4);
  assert.equal(observation.cleared, 0);
  assert.equal(observation.chips, 0);
  assert.ok(isMapEvidence(observation, catalog.levels));
  const search = level.id ? Explorer.from_history(2, '[]') : new Explorer(2);
  search.set_snapshot_budget(4 * 1048576);
  let batch;
  for (let n = 0; n < 15; n++) batch = JSON.parse(search.advance(2));
  assert.equal(batch.executions, 30, `Level ${level.id} must actually search`);
  const id = batch.points.findLast(p => p.retained !== null)?.retained ?? 0;
  const state = JSON.parse(search.state(id)), snapshot = search.digest(id);
  assert.deepEqual(engine.boot(level.id), root, 'Repeated bootstrap is byte deterministic');
  for (const action of state.actions) engine.run(action.buttons, action.frames);
  assert.equal(snapshotDigest(engine.capture()), snapshot, `Level ${level.id} archive must replay exactly`);
  const child = Explorer.from_history(3, JSON.stringify(state.actions));
  assert.equal(child.digest(0), snapshot, 'Fork is rooted at the replayed endpoint');
  assert.equal(JSON.parse(child.advance(2)).executions, 2);
  child.free(); search.free();
}
console.log('All 44 native level roots: healthy authentic boot, fresh search, exact archived replay, repeated warp and rooted fork passed.');
