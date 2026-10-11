// SPDX-License-Identifier: AGPL-3.0-or-later
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  ReplayTimeline,
  prefixTrail,
  sharedFrames,
  trailPoint,
} from "../src/replay.js";
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

test("route selection reuses only snapshots from the exact shared controller prefix", () => {
  const timeline = new ReplayTimeline(12),
    a = [
      { buttons: 128, frames: 30 },
      { buttons: 1, frames: 12 },
    ];
  assert.equal(timeline.reset(a, 42), 0);
  timeline.put(20, new Uint8Array(4));
  timeline.put(30, new Uint8Array(4));
  timeline.put(40, new Uint8Array(4));
  assert.equal(
    timeline.reset(
      a.map((a) => ({ ...a })),
      42,
    ),
    42,
  );
  assert.equal(timeline.before(42).frame, 40);
  const b = [
    { buttons: 128, frames: 15 },
    { buttons: 128, frames: 15 },
    { buttons: 2, frames: 12 },
  ];
  assert.equal(timeline.reset(b, 42), 30);
  assert.equal(timeline.before(42).frame, 30);
  assert.equal(timeline.checkpoints.has(40), false);
  assert.equal(timeline.bytes, 8);
  assert.equal(
    timeline.reset(
      [
        { buttons: 128, frames: 25 },
        { buttons: 2, frames: 12 },
      ],
      37,
    ),
    25,
  );
  assert.equal(timeline.before(37).frame, 20);
  assert.equal(timeline.bytes, 4);
  timeline.clear();
  assert.equal(timeline.reset(a, 42), 0);
  assert.equal(timeline.bytes, 0);
  assert.equal(sharedFrames(a, [{ buttons: 64, frames: 42 }]), 0);
});
test("shared trail reuse drops the divergent future and preserves reload boundaries", () => {
  const points = [
    { frame: 0, level: 0, x: 0, y: 0 },
    { frame: 24, level: 0, x: 5, y: 0 },
    { frame: 48, gap: true },
    { frame: 72, level: 49, x: 2, y: 0 },
    { frame: 96, level: 49, x: 3, y: 0 },
    { frame: 120, level: 49, x: 4, y: 0 },
  ];
  assert.deepEqual(prefixTrail(points, 100, 48), [
    points[0],
    points[2],
    points[3],
  ]);
  assert.equal(trailPoint(prefixTrail(points, 100, 48), 60), null);
  assert.deepEqual(prefixTrail(points, 0, 24), [points[0]]);
});
