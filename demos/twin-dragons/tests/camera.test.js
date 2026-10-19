// SPDX-License-Identifier: AGPL-3.0-or-later
import { test } from "node:test";
import assert from "node:assert/strict";
import { resumeFollowing, resetRetreat, trackingGoal } from "../src/camera.js";

test("following a selected level clears the previous camera's interaction holds", () => {
  const camera = { follow: false, manual: 10000, held: 3000, only: 1,
    overview: true, focus: { x: 20 }, targets: new Map([[1, {}]]) };
  resumeFollowing(camera);
  assert.equal(camera.follow, true);
  assert.equal(camera.manual, 0);
  assert.equal(camera.held, 0);
  assert.equal(camera.only, null);
  assert.equal(camera.focus, null);
  assert.equal(camera.overview, false);
  assert.equal(camera.targets.size, 0);
});

test("a result gap resets retreat evidence, including repeated sparse bursts", () => {
  const goal = { x: 1000, y: 100 }, back = { x: 200, y: 100 };
  const held = { goal, behind: 0 }, progress = (p) => p.x;
  const points = Array.from({ length: 4 }, () => ({ x: 192, y: 100 }));
  const track = (point, time) => trackingGoal(held, point, points, time, progress);
  assert.equal(track(back, 100), goal);
  assert.equal(track(null, 5000), goal);
  assert.equal(held.behind, 0);
  assert.equal(track(back, 5100), goal, "A long empty interval cannot trigger a retreat on the next burst");
  assert.equal(track(null, 5200), goal);
  assert.equal(track(back, 10000), goal);
  assert.equal(track(back, 11499), goal);
  assert.equal(track(back, 11500), back, "Continuous visible evidence still allows a real retreat");
  assert.equal(track({ x: 1200, y: 100 }, 11600).x, 1200);
  assert.equal(held.behind, 0);
});

test("stale sprites cannot prevent the camera following a real retreat", () => {
  const goal = { x: 1000, y: 100 }, back = { x: 200, y: 100 };
  const held = { goal, behind: 100 };
  const points = [{ x: 992, y: 100, stale: true }, { x: 992, y: 100, stale: true }];
  assert.equal(trackingGoal(held, back, points, 2000, (p) => p.x), back);
});

test("suspending camera updates discards retreat evidence in every room", () => {
  const goal = { x: 1000, y: 100 }, back = { x: 200, y: 100 };
  const held = { goal, behind: 100 }, other = { behind: 200 };
  resetRetreat({ targets: new Map([[0, held], [1, other]]) });
  assert.equal(other.behind, 0);
  assert.equal(trackingGoal(held, back, [], 3300, (p) => p.x), goal);
});
