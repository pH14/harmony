// SPDX-License-Identifier: AGPL-3.0-or-later
import { test } from "node:test";
import assert from "node:assert/strict";
import { ReplayTimeline, trailPoint } from "../src/replay.js";
test("checkpoint seeks use the preceding state and remain within their byte budget", () => {
  const t = new ReplayTimeline(8);
  t.reset(
    [
      { buttons: 128, frames: 30 },
      { buttons: 1, frames: 12 },
    ],
    42,
  );
  assert.deepEqual(t.actionAt(35), { buttons: 1, frames: 7 });
  assert.equal(t.actionAt(42), null);
  t.put(10, new Uint8Array(4));
  t.put(20, new Uint8Array(4));
  assert.equal(t.before(20).frame, 10);
  t.put(30, new Uint8Array(4));
  assert.equal(t.bytes, 8);
  assert.equal(t.checkpoints.has(20), false);
  t.trim(10);
  assert.equal(t.bytes, 4);
  t.reset([], 0);
  assert.equal(t.bytes, 0);
});
test("requested trail positions interpolate without drawing across reloads or portals", () => {
  const points = [
    { level: 0, x: 10, y: 20, frame: 0 },
    { level: 0, x: 30, y: 40, frame: 24 },
    { gap: true, frame: 48 },
    { level: 49, x: 1, y: 2, frame: 72 },
  ];
  assert.deepEqual(trailPoint(points, 12), {
    level: 0,
    x: 20,
    y: 30,
    frame: 12,
  });
  assert.equal(trailPoint(points, 36), null);
  assert.equal(trailPoint(points, 48), null);
  assert.equal(trailPoint(points, 90), null);
  assert.equal(trailPoint([points[0], points[3]], 40).level, 0);
});
