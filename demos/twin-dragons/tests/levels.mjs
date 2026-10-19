// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from 'node:assert/strict';
import twinDragons from "../src/games/twin-dragons.js";
import { snapshotDigest } from '../src/media.js';
import { readFile } from 'node:fs/promises';
import { createEngine } from '../src/emulator.js';
import { isMapEvidence } from '../src/world.js';
import { campaignSearch, campaignWorker, coreIdentity, runCampaign } from '../src/campaign.js';
const base = new URL('../public/', import.meta.url);
const engine = await createEngine(base, twinDragons, {
  rom: await readFile(new URL('twin-dragons.nes', base)),
  wasmBinary: await readFile(new URL('engine/quicknes.wasm', base)),
});
const identity = coreIdentity('browser'), worker = campaignWorker(engine, identity), JOBS = 32;
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
  const search = campaignSearch(engine, identity, { seed: 2, root: level.id ? root : undefined, memoryMiB: 8 });
  runCampaign(search, worker, { jobs: JOBS });
  const batch = JSON.parse(search.take_batch());
  assert.ok(batch.executions > 0, `Level ${level.id} must actually search`);
  const id = batch.points.findLast(p => p.retained !== null)?.retained ?? 0;
  const state = JSON.parse(search.state(id)), snapshot = search.digest(id);
  assert.deepEqual(engine.boot(level.id), root, 'Repeated bootstrap is byte deterministic');
  for (const action of state.actions) engine.run(action.buttons, action.frames);
  const endpoint = engine.capture();
  assert.equal(snapshotDigest(endpoint), snapshot, `Level ${level.id} archive must replay exactly`);
  const child = campaignSearch(engine, identity, { seed: 3, root: endpoint, prefix: state.actions, memoryMiB: 8 });
  assert.equal(child.digest(0), snapshot, 'Fork is rooted at the replayed endpoint');
  runCampaign(child, worker, { jobs: JOBS });
  assert.ok(JSON.parse(child.take_batch()).executions > 0, 'A fork searches from its replayed endpoint');
  child.free(); search.free();
}
console.log('All 3 level roots: healthy authentic boot, fresh search, exact archived replay, repeated warp and rooted fork passed.');
