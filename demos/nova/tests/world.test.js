// SPDX-License-Identifier: AGPL-3.0-or-later
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { project, completedLevels, mergeProgress } from "../src/world.js";
import { viewCenter } from "../src/view.js";
test("the pinned source catalogs the real intro door chain and all campaign levels", () => {
  const { maps, levels } = JSON.parse(
    readFileSync(new URL("../public/maps.json", import.meta.url)),
  );
  assert.equal(maps.length, 57);
  assert.equal(levels.length, 44);
  assert.deepEqual(levels[0].rooms, [0, 49, 45]);
  assert.equal(maps[45].name, "intro_b");
  assert.equal(maps[49].name, "garden");
  assert.equal(maps[40].name, "extra1");
  assert.ok(maps[13].height > 224);
  for (const map of maps) {
    assert.ok(map.runtimeWidth <= 4096);
    assert.ok(map.width * map.height >= map.runtimeWidth * 224);
  }
});
test("vertical source pages project to the map while preserving exact game state resources", () => {
  const o = { x: 528, y: 184, health: 3, level: 13 };
  assert.deepEqual(project(o, { width: 256 }), { ...o, x: 16, y: 632 });
  assert.equal(o.x, 528);
  const view = viewCenter(256, 2, { x: 16, y: 632 }, 3584, {
    width: 1280,
    height: 320,
  });
  assert.equal(view.x, 128);
  assert.ok(view.y >= 632 - (160 * 3584) / 320 / 2);
});
test("completion comes from campaign clear bits and remains latched across branches", () => {
  assert.deepEqual(completedLevels([1, 0, 0, 0, 128, 255]), [0, 39]);
  const progress = new Set();
  mergeProgress(progress, { cleared_levels: [1] });
  mergeProgress(progress, { cleared_levels: [0, 2] });
  assert.deepEqual([...progress], [0, 9]);
  assert.equal(completedLevels([255, 255, 255, 255, 255]).length, 40);
});
