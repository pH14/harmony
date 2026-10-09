// SPDX-License-Identifier: AGPL-3.0-or-later
import { test } from "node:test";
import assert from "node:assert/strict";
import { tourPosition, tourSeen, rememberTour, TOUR_KEY } from "../src/tour.js";
const rect = (left, top, width, height) => ({
  left,
  top,
  width,
  height,
  right: left + width,
  bottom: top + height,
});
const overlap = (p, width, height, r) =>
  Math.max(0, Math.min(p.x + width, r.right) - Math.max(p.x, r.left)) *
  Math.max(0, Math.min(p.y + height, r.bottom) - Math.max(p.y, r.top));
test("tour callouts keep controls visible on phone and desktop", () => {
  for (const [viewport, targets, size] of [
    [{ width: 390, height: 844 }, [rect(170, 590, 160, 36)], [340, 230]],
    [{ width: 1440, height: 1100 }, [rect(1020, 700, 360, 120)], [340, 230]],
    [{ width: 390, height: 844 }, [rect(20, 260, 350, 80)], [340, 230]],
  ]) {
    const [w, h] = size,
      p = tourPosition(targets, w, h, viewport);
    assert.ok(p.x >= 12 && p.y >= 12);
    assert.ok(
      p.x + w <= viewport.width - 12 && p.y + h <= viewport.height - 12,
    );
    assert.equal(overlap(p, w, h, targets[0]), 0);
  }
});
test("tour callouts avoid both the branch action and its menu", () => {
  const targets = [rect(980, 420, 350, 40), rect(120, 100, 210, 36)],
    p = tourPosition(targets, 340, 230, { width: 1440, height: 900 });
  for (const target of targets) assert.equal(overlap(p, 340, 230, target), 0);
});
test("tour dismissal is versioned and unavailable storage cannot block the demo", () => {
  const saved = new Map(),
    storage = () => ({
      getItem: (k) => saved.get(k),
      setItem: (k, v) => saved.set(k, v),
    });
  assert.equal(tourSeen(storage), false);
  rememberTour(storage);
  assert.equal(saved.get(TOUR_KEY), "seen");
  assert.equal(tourSeen(storage), true);
  const blocked = () => {
    throw new Error("Storage denied");
  };
  assert.equal(tourSeen(blocked), false);
  assert.doesNotThrow(() => rememberTour(blocked));
});
