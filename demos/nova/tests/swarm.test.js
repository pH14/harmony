// SPDX-License-Identifier: AGPL-3.0-or-later
import { test } from "node:test";
import assert from "node:assert/strict";
import { NovaSwarm, RolloutRecorder, swarmPoint, NO_ROOM } from "../src/swarm.js";

test("swarm interpolation never connects rooms, death or reload gaps", () => {
  const samples = new Uint16Array([0, 0, 10, 20, 0, 4, 0, 18, 28, 2,
    8, NO_ROOM, 0, 0, 0, 12, 49, 50, 90, 10, 16, 49, 60, 100, 11]);
  assert.deepEqual(swarmPoint(samples, 2), { level: 0, x: 14, y: 24, pose: 0 });
  assert.equal(swarmPoint(samples, 6).x, 18);
  assert.equal(swarmPoint(samples, 10), null);
  assert.deepEqual(swarmPoint(samples, 14), { level: 49, x: 55, y: 95, pose: 10 });
  const door = new Uint16Array([0, 0, 100, 10, 0, 4, 49, 50, 100, 0]);
  assert.equal(swarmPoint(door, 3).level, 0);
  assert.equal(swarmPoint(door, 3).x, 100);
  assert.equal(swarmPoint(door, 4).level, 49);
});

test("one global swarm allowance evicts payloads across branches and resets fully", () => {
  const samples = new Uint16Array([0, 0, 10, 20, 0, 4, 0, 18, 28, 2]);
  const swarm = new NovaSwarm(40, 9);
  swarm.add(0, [samples, samples]);
  swarm.add(1, [samples]);
  assert.equal(swarm.bytes, 40);
  assert.equal(swarm.frame(0, 0).get(0).length, 1);
  assert.equal(swarm.frame(1, 0).get(0).length, 1);
  assert.notDeepEqual(swarm.frame(1, 0), swarm.frame(1, 10));
  assert.deepEqual(swarm.frame(1, 0, true), swarm.frame(1, 10000, true));
  swarm.clear();
  assert.equal(swarm.bytes, 0);
  assert.equal(swarm.trails.size, 0);
  const capped = new NovaSwarm(1000, 1);
  capped.add(0, [samples, samples]);
  assert.equal(capped.trails.size, 1);
});

test("recorder isolates restored rollouts and leaves nonrecorded execution alone", () => {
  let x = 0;
  const runs = [];
  const engine = { restore: (n) => { x = n; }, run: (b, n, render) => { x += n; runs.push([b, n, render]); } };
  const recorder = new RolloutRecorder(engine, () => [0, x, 20, 0]);
  engine.run(128, 9, true);
  assert.deepEqual(runs, [[128, 9, true]]);
  recorder.begin();
  engine.restore(10);
  engine.run(128, 9, true);
  engine.restore(50);
  engine.run(0, 2);
  const trails = recorder.finish();
  assert.equal(trails.length, 2);
  assert.deepEqual([...trails[0]], [0, 0, 10, 20, 0, 4, 0, 14, 20, 0,
    8, 0, 18, 20, 0, 9, 0, 19, 20, 0]);
  assert.deepEqual([...trails[1]], [0, 0, 50, 20, 0, 2, 0, 52, 20, 0]);
  assert.deepEqual(runs.slice(1, 4), [[128, 4, false], [128, 4, false], [128, 1, true]]);
  engine.run(128, 9);
  assert.deepEqual(runs.at(-1), [128, 9, false]);
});
