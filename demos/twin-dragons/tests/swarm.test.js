// SPDX-License-Identifier: AGPL-3.0-or-later
import { test } from "node:test";
import assert from "node:assert/strict";
import { Heatmap } from "../src/heat.js";
import { Swarm, swarmPoint, ghosts, NO_ROOM } from "../src/swarm.js";

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
  const swarm = new Swarm(40, 9);
  swarm.add(0, [samples, samples], 0);
  swarm.add(1, [samples], 0);
  assert.equal(swarm.bytes, 40);
  assert.equal(swarm.frame(0, 0).get(0).length, 1);
  assert.equal(swarm.frame(1, 0).get(0).length, 1);
  assert.notDeepEqual(swarm.frame(1, 0), swarm.frame(1, 10));
  assert.deepEqual(swarm.frame(1, 0, true), swarm.frame(1, 10, true));
  swarm.clear();
  assert.equal(swarm.bytes, 0);
  assert.equal(swarm.trails.size, 0);
  const capped = new Swarm(1000, 1);
  capped.add(0, [samples, samples], 0);
  assert.equal(capped.trails.size, 1);
});

test("incoming attempts play once, expire without looping and release their bytes", () => {
  const samples = new Uint16Array([0, 0, 10, 20, 0, 60, 0, 110, 20, 2]);
  const swarm = new Swarm(1000, 10);
  swarm.add(0, [samples], 100);
  assert.equal(swarm.frame(0, 100).get(0)[0].x, 10);
  assert.equal(swarm.frame(0, 600).get(0)[0].x, 60);
  assert.ok(swarm.frame(0, 1099).get(0)[0].x > 109);
  assert.equal(swarm.frame(0, 1100).size, 0);
  assert.equal(swarm.bytes, 0);
  assert.equal(swarm.frame(0, 5100).size, 0);
  swarm.add(0, [samples], 5100);
  assert.equal(swarm.frame(0, 5100).get(0)[0].x, 10);
  swarm.add(0, [], 6100);
  assert.equal(swarm.trails.size, 0, "New batches expire attempts even when the view is hidden");
  assert.equal(swarm.bytes, 0);
});

test("attempts follow each branch's activity clock through pauses and reduced motion", () => {
  const samples = new Uint16Array([0, 0, 10, 20, 0, 60, 0, 110, 20, 2]);
  const swarm = new Swarm(1000, 10), original = new Heatmap(), child = new Heatmap();
  original.setRunning(true, 0);
  swarm.add(0, [samples], original.clock(0));
  original.setRunning(false, 500);
  const frozen = swarm.frame(0, original.clock(500));
  child.setRunning(true, 500);
  swarm.add(1, [samples], child.clock(500));
  assert.equal(swarm.frame(1, child.clock(10500)).size, 0);
  assert.equal(swarm.bytes, samples.byteLength, "Another branch's clock cannot expire a paused attempt");
  assert.deepEqual(swarm.frame(0, original.clock(10500)), frozen);
  assert.equal(swarm.frame(0, original.clock(10500), true).get(0)[0].x, 10);
  original.setRunning(true, 10500);
  assert.equal(swarm.frame(0, original.clock(10750)).get(0)[0].x, 85);
  assert.equal(swarm.frame(0, original.clock(11000), true).size, 0);
  assert.equal(swarm.bytes, 0);
});

test("deleting a search releases only its movement recordings", () => {
  const swarm = new Swarm(4096, 20);
  const samples = new Uint16Array([0,0,16,100,0,24,0,48,100,0]);
  swarm.add(1, [samples], 0);
  swarm.add(2, [samples], 0);
  swarm.remove(1);
  assert.equal(swarm.bytes, samples.byteLength);
  assert.equal(swarm.frame(1, 100).size, 0);
  assert.equal(swarm.frame(2, 100).get(0).length, 1);
});


test("bounded frozen previews retain real positions after recordings expire", () => {
  const swarm = new Swarm(4096, 20);
  const trail = new Uint16Array([0,0,16,100,0,60,0,76,100,2]);
  swarm.add(1, [trail, trail, trail], 0);
  swarm.add(2, [trail], 0);
  const frozen = swarm.frame(1, 500, false, 2);
  assert.equal(frozen.get(0).length,2);
  assert.deepEqual(frozen.get(0)[0], {level:0,x:46,y:100,pose:0});
  assert.equal(swarm.frame(1,1500).size,0);
  assert.equal(frozen.get(0)[0].x,46,'The preview cannot move or disappear when live recordings expire');
  assert.equal(swarm.frame(2,500).get(0).length,1,'Previewing another branch does not advance its clock');
});

test("detailed frames report rollout age and same-room ghosts", () => {
  const swarm = new Swarm(1 << 20, 16);
  swarm.add(0, [new Uint16Array([0, 0, 10, 100, 0, 30, 0, 70, 100, 0])], 0);
  const [point] = swarm.frame(0, 250, false, 16, true).get(0);
  assert.ok(point.life > 0.4 && point.life < 0.6);
  assert.equal(ghosts(point).length, 2);
  assert.ok(ghosts(point).every((ghost) => ghost.x < point.x));
  assert.deepEqual(ghosts(swarm.frame(0, 250, true, 16, true).get(0)[0]), []);
});

test("a sprite cap samples trails across the swarm and keeps each sampled trail across frames", () => {
  const swarm = new Swarm(1 << 20, 100);
  const trails = Array.from({ length: 40 }, (_, i) => new Uint16Array([0, 0, i, 20, 0, 60, 0, i + 50, 20, 0]));
  swarm.add(0, trails, 0);
  assert.deepEqual(swarm.frame(0, 0, false, 10).get(0).map((p) => p.x), [0, 4, 8, 12, 16, 20, 24, 28, 32, 36]);
  assert.deepEqual(swarm.frame(0, 500, false, 10).get(0).map((p) => p.x), [25, 29, 33, 37, 41, 45, 49, 53, 57, 61]);
});

test("level-clearing trails stay drawn past the sprite cap", () => {
  const trails = Array.from({ length: 5 }, (_, i) => new Uint16Array([0, 0, i, 20, 0, 30, 0, i + 40, 20, 0]));
  const swarm = new Swarm(1 << 20, 100);
  swarm.add(0, trails, 0, [1, 3]);
  const drawn = swarm.frame(0, 0, false, 1).get(0);
  assert.deepEqual(drawn.map((p) => [p.x, !!p.finish]), [[0, false], [1, true], [3, true]]);
});

test("a clearing trail holds its last position for the finish hold", () => {
  const samples = new Uint16Array([0, 0, 10, 20, 0, 60, 0, 110, 20, 2]);
  const swarm = new Swarm(1000, 10, 500);
  swarm.add(0, [samples, samples], 0, [1]);
  assert.equal(swarm.frame(0, 999).get(0).length, 2);
  const held = swarm.frame(0, 1200).get(0);
  assert.equal(held.length, 1);
  assert.equal(held[0].finish, true);
  assert.equal(held[0].x, 110);
  assert.equal(swarm.frame(0, 1500).size, 0);
});

test("live cohorts acquire separate births without changing trajectory speed", () => {
  const trails = Array.from({ length: 4 }, (_, i) => new Uint16Array([0, 0, i * 100, 20, 0, 60, 0, i * 100 + 60, 20, 0]));
  const swarm = new Swarm(4096, 20);
  swarm.add(0, trails, 100, [], 100);
  assert.equal(swarm.frame(0, 99).size, 0);
  assert.deepEqual(swarm.frame(0, 100).get(0).map(p => p.x), [0]);
  assert.deepEqual(swarm.frame(0, 125).get(0).map(p => p.x), [1.5, 100]);
  assert.deepEqual(swarm.frame(0, 150).get(0).map(p => p.x), [3, 101.5, 200]);
  assert.deepEqual(swarm.frame(0, 175).get(0).map(p => p.x), [4.5, 103, 201.5, 300]);
  assert.deepEqual(swarm.frame(0, 600).get(0).map(p => p.x), [30, 128.5, 227, 325.5]);
  assert.equal(swarm.frame(0, 1175).size, 0);
  assert.equal(swarm.bytes, 0);
  swarm.add(0, trails, 1200);
  assert.deepEqual(swarm.frame(0, 1200).get(0).map(p => p.x), [0, 100, 200, 300], "Ordinary browser batches retain immediate births");
});

test("scheduled births share caps and finish hold, and a pause freezes their clock", () => {
  const samples = new Uint16Array([0, 0, 10, 20, 0, 6, 0, 16, 20, 0]);
  const clock = new Heatmap(), swarm = new Swarm(samples.byteLength * 2, 2, 500);
  clock.setRunning(true, 0);
  swarm.add(0, [samples, samples, samples, samples], clock.clock(0), [3], 10000);
  assert.equal(swarm.trails.size, 2);
  assert.equal(swarm.bytes, samples.byteLength * 2);
  assert.deepEqual(swarm.frame(0, 49).get(0).map(p => [p.x, !!p.finish]), [[12.94, true]], "Finish evidence is immediate; evicted attempts cannot leak their scheduled births");
  clock.setRunning(false, 60);
  assert.equal(swarm.frame(0, clock.clock(5000)).get(0).length, 2);
  clock.setRunning(true, 5000);
  const points = swarm.frame(0, clock.clock(5020)).get(0);
  assert.equal(points.length, 2);
  assert.equal(points[1].finish, true);
  assert.equal(points[1].x, 14.8);
  assert.equal(swarm.frame(0, 175).get(0).length, 1);
  assert.equal(swarm.frame(0, 600).size, 0);
  assert.equal(swarm.bytes, 0);
});


test("terminal presentation deadline uses retained actual durations and finish holds", () => {
  const swarm = new Swarm(4096, 10, 500);
  const long = new Uint16Array([0, 0, 10, 20, 0, 65535, 0, 18, 28, 2]);
  const short = new Uint16Array([0, 0, 10, 20, 0, 60, 0, 18, 28, 2]);
  swarm.add(0, [short, short], 100, [1], 100);
  assert.equal(swarm.endTime(0, 100), 1600);
  assert.equal(swarm.endTime(1, 100), 100);
  swarm.add(1, [long], 200);
  assert.equal(swarm.endTime(1, 200), 200 + 65535 / 0.06);
  assert.equal(swarm.bytes, short.byteLength * 2 + long.byteLength);
  swarm.frame(0, 1600);
  assert.equal(swarm.endTime(0, 1600), 1600);
  swarm.remove(1);
  assert.equal(swarm.bytes, 0);
});
