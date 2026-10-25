// SPDX-License-Identifier: AGPL-3.0-or-later
import { test } from "node:test";
import assert from "node:assert/strict";
import { resumeFollowing, resetRetreat, trackingGoal, followView } from "../src/camera.js";

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

test("suspending camera updates discards retreat evidence in every room", () => {
  const goal = { x: 1000, y: 100 }, back = { x: 200, y: 100 };
  const held = { goal, behind: 100 }, other = { behind: 200 };
  resetRetreat({ targets: new Map([[0, held], [1, other]]) });
  assert.equal(other.behind, 0);
  assert.equal(trackingGoal(held, back, [], 3300, (p) => p.x), goal);
});

test("rooms off screen cannot retain retreat evidence between visits", () => {
  const first = { behind: 100 }, second = { behind: 200 };
  const camera = { targets: new Map([[0, first], [1, second]]) };
  resetRetreat(camera, 1);
  assert.equal(first.behind, 0);
  assert.equal(second.behind, 200);
  resetRetreat(camera, 0);
  assert.equal(second.behind, 0);
});

function cornerMotion(before, after, panel, viewport) {
  const fit = Math.min(viewport.width / panel.width, viewport.height / panel.height);
  const oldScale = fit * before.zoom, scale = fit * after.zoom;
  return Math.max(...[-0.5, 0.5].flatMap(x => [-0.5, 0.5].map(y => {
    const worldX = before.x + x * viewport.width / oldScale;
    const worldY = before.y + y * viewport.height / oldScale;
    return Math.hypot(((worldX - after.x) * scale - x * viewport.width) / viewport.width,
      ((worldY - after.y) * scale - y * viewport.height) / viewport.height);
  })));
}

test("rapid frontiers and retreats bound the complete viewport transform at60 and120Hz", () => {
  const cases = [
    { panel: { x: 0, y: 0, width: 14283, height: 305 }, viewport: { width: 1280, height: 320 }, zoom: 16 },
    { panel: { x: 50, y: 100, width: 768, height: 6000 }, viewport: { width: 432, height: 900 }, zoom: 8 },
    { panel: { x: 100, y: 200, width: 5000, height: 4000 }, viewport: { width: 960, height: 720 }, zoom: 6 },
  ];
  for (const { panel, viewport, zoom } of cases) for (const hz of [60, 120]) {
    let view = { x: panel.x + panel.width * 0.1, y: panel.y + panel.height * 0.1, zoom };
    for (let i = 0; i < hz * 4; i++) {
      const side = i < hz * 2 ? 0.9 : 0.1;
      const next = followView(view, { x: panel.x + panel.width * side, y: panel.y + panel.height * side }, panel, viewport, zoom, 1 / hz);
      assert.ok(cornerMotion(view, next, panel, viewport) <= 2 / hz + 1e-12,
        "Previously visible landmarks include zoom motion in their speed bound");
      assert.ok(Object.values(next).every(Number.isFinite));
      assert.ok(next.zoom >= 1 && next.zoom <= zoom);
      assert.ok(next.x >= panel.x && next.x <= panel.x + panel.width);
      assert.ok(next.y >= panel.y && next.y <= panel.y + panel.height);
      view = next;
    }
  }
});

test("a distant frontier becomes visible before close-up following settles", () => {
  const panel = { x: 0, y: 0, width: 14283, height: 305 }, viewport = { width: 1280, height: 320 };
  const point = { x: 12000, y: 193 }, zoom = 16;
  for (const hz of [60, 120]) {
    let view = { x: 600, y: 193, zoom }, seen = false;
    for (let i = 0; i < hz * 12; i++) {
      view = followView(view, point, panel, viewport, zoom, 1 / hz);
      const scale = Math.min(viewport.width / panel.width, viewport.height / panel.height) * view.zoom;
      if (i < hz * 2 && Math.abs(point.x - view.x) * scale <= viewport.width / 2 &&
          Math.abs(point.y - view.y) * scale <= viewport.height / 2) seen = true;
    }
    assert.ok(seen, "Widening keeps the new work visible instead of chasing it through an empty close-up");
    assert.ok(Math.abs(view.x - point.x) < 0.1);
    assert.ok(Math.abs(view.zoom - zoom) < 0.01);
    assert.deepEqual(followView(view, point, panel, viewport, zoom, 0), view);
  }
});

test("a fixed-zoom follow keeps the zoom and closes on a far point", () => {
  const panel = { x: 0, y: 0, width: 15000, height: 480 }, viewport = { width: 1280, height: 320 };
  let view = { x: 1000, y: 240, zoom: 14 };
  for (let i = 0; i < 60; i++) view = followView(view, { x: 9000, y: 240 }, panel, viewport, 14, 1 / 60, true);
  assert.ok(Math.abs(view.zoom - 14) < 1e-9);
  assert.ok(view.x > 5000);
});

test("a viewport that shows the whole panel zooms in on a far point", () => {
  const panel = { x: 0, y: 0, width: 256, height: 862 }, viewport = { width: 432, height: 506 };
  let view = { x: 128, y: 431, zoom: 1 };
  for (let i = 0; i < 300; i++) view = followView(view, { x: 128, y: 100 }, panel, viewport, 2.87, 1 / 60);
  assert.ok(view.zoom > 2.5);
  assert.ok(view.y < 200);
});

test("a view a hair under zoom 1 still counts as showing the whole panel", () => {
  const panel = { x: 0, y: 0, width: 256, height: 862 }, viewport = { width: 432, height: 506 };
  let view = { x: 128, y: 431, zoom: 1.0004 };
  for (let i = 0; i < 300; i++) view = followView(view, { x: 128, y: 20 }, panel, viewport, 2.87, 1 / 60);
  assert.ok(view.zoom > 2.5);
});
