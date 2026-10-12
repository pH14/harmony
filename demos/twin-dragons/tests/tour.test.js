// SPDX-License-Identifier: AGPL-3.0-or-later
import { test } from "node:test";
import assert from "node:assert/strict";
import { tourPosition, tourSeen, rememberTour, TOUR_KEY, uncoveredRects } from "../src/tour.js";
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

test("replay callouts leave both route maps and transport clear on phones", () => {
  const targets = [rect(130, 540, 247, 80), rect(20, 270, 350, 120), rect(20, 440, 96, 96)],
    p = tourPosition(targets, 340, 200, { width: 390, height: 844 });
  for (const target of targets) assert.equal(overlap(p, 340, 200, target), 0);
});


test("spotlights only expose map portions outside covering panes", () => {
  const map = rect(20, 180, 350, 160);
  const pane = rect(0, 260, 390, 300);
  assert.deepEqual(uncoveredRects(map, [pane]), [{left:20,top:180,right:370,bottom:260}]);
  const pieces = uncoveredRects(rect(0,0,100,100), [rect(30,30,40,40)]);
  assert.equal(pieces.reduce((sum,r) => sum+(r.right-r.left)*(r.bottom-r.top),0),8400);
  for (const piece of pieces) assert.equal(overlap({x:piece.left,y:piece.top},piece.right-piece.left,piece.bottom-piece.top,rect(30,30,40,40)),0);
  assert.deepEqual(uncoveredRects(map, [rect(0,0,400,600)]), []);
});
