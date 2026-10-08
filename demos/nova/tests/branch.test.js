// SPDX-License-Identifier: AGPL-3.0-or-later
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  prefixAt,
  appendInput,
  Controller,
  trailSegments,
  branchInfo,
} from "../src/branch.js";
test("a mid-action branch discards the old future without changing its parent", () => {
  const parent = [
    { buttons: 128, frames: 30 },
    { buttons: 1, frames: 12 },
  ];
  const child = prefixAt(parent, 35);
  assert.deepEqual(child, [
    { buttons: 128, frames: 30 },
    { buttons: 1, frames: 5 },
  ]);
  appendInput(child, 0, 121);
  assert.equal(parent[1].frames, 12);
  assert.deepEqual(child.slice(2), [
    { buttons: 0, frames: 120 },
    { buttons: 0, frames: 1 },
  ]);
});
test("controller resolves opposing directions and independently releases aliases", () => {
  const c = new Controller();
  c.press("left", 64);
  c.press("right", 128);
  c.press("jump", 1);
  assert.equal(c.buttons(), 129);
  c.release("right");
  assert.equal(c.buttons(), 65);
  c.press("second-left", 64);
  c.release("left");
  assert.equal(c.buttons(), 65);
  c.clear();
  assert.equal(c.buttons(), 0);
});
test("traces break at reloads, room changes, and large teleports", () => {
  const p = (level, x, frame) => ({ level, x, y: 100, frame });
  const segments = trailSegments([
    p(0, 0, 0),
    p(0, 10, 24),
    { gap: true, frame: 48 },
    p(0, 20, 72),
    p(49, 1, 96),
    p(49, 1000, 120),
  ]);
  assert.deepEqual(
    segments.map((s) => s.length),
    [2, 1, 1, 1],
  );
});
test("branch metadata is bounded and cannot smuggle arbitrary imported fields", () => {
  assert.deepEqual(
    branchInfo(
      {
        parent_state: "1:9",
        parent_frame: 20,
        manual: { from: 20, to: 80 },
        extra: "x".repeat(1000),
      },
      50,
    ),
    { parent_state: "1:9", parent_frame: 20, manual: { from: 20, to: 50 } },
  );
  assert.equal(
    branchInfo({ parent_state: "x".repeat(65), parent_frame: 1 }, 10),
    null,
  );
  assert.equal(branchInfo({ parent_state: 1, parent_frame: 99 }, 10), null);
});

test("human input admission stops before an unrecorded frame", () => {
  const actions = Array.from({ length: 10000 }, () => ({
    buttons: 0,
    frames: 120,
  }));
  assert.equal(appendInput(actions, 128, 1), false);
  assert.equal(actions.length, 10000);
  actions.at(-1).frames = 119;
  assert.equal(appendInput(actions, 0, 1), true);
  assert.equal(actions.at(-1).frames, 120);
  assert.equal(appendInput(actions, 0, 1), false);
});
