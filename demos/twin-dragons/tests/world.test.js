// SPDX-License-Identifier: AGPL-3.0-or-later
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  followDiscovery,
  project,
  completedLevels,
  mergeProgress,
  isMapEvidence,
} from "../src/world.js";
import { viewCenter } from "../src/view.js";
test("the catalog holds one map for each of the three levels", () => {
  const { maps, levels } = JSON.parse(
    readFileSync(new URL("../public/maps.json", import.meta.url)),
  );
  assert.deepEqual(maps.map((map) => [map.name, map.width, map.height]), [
    ["test", 2304, 224],
    ["level1", 2304, 224],
    ["level2", 1536, 224],
  ]);
  assert.deepEqual(levels.map((level) => [level.label, level.rooms]), [
    ["Level 1", [0]],
    ["Level 2", [1]],
    ["Level 3", [2]],
  ]);
});
test("projection centers the player without changing the game state", () => {
  const o = { x: 528, y: 175, health: 3, level: 1 };
  assert.deepEqual(project(o, { width: 2304 }), { ...o, x: 536, y: 183 });
  assert.equal(o.x, 528);
  const view = viewCenter(2304, 2, { x: 536, y: 175 }, 224, { width: 1280, height: 320 });
  assert.deepEqual(view, { x: 576, y: 112 });
});
test("completion counts cleared levels from the starting level and stays latched", () => {
  assert.deepEqual(completedLevels({ cleared: 0 }), []);
  assert.deepEqual(completedLevels({ cleared: 2 }), [0, 1]);
  assert.deepEqual(completedLevels({ cleared: 3 }, 1), [1, 2]);
  const progress = new Set();
  mergeProgress(progress, { cleared: 1 });
  mergeProgress(progress, { cleared: 0 });
  assert.deepEqual([...progress], [0]);
});
test("level loading, deaths and unknown levels are not map evidence", () => {
  const { levels } = JSON.parse(
    readFileSync(new URL("../public/maps.json", import.meta.url)),
  );
  const gameplay = { level: 1, health: 3, loading: false };
  assert.equal(isMapEvidence(gameplay, levels), true);
  assert.equal(isMapEvidence({ ...gameplay, loading: true }, levels), false);
  assert.equal(isMapEvidence({ ...gameplay, health: 0 }, levels), false);
  assert.equal(isMapEvidence({ ...gameplay, level: 3 }, levels), false);
});

test("a next-level discovery follows a clear even when the preceding level was inspected", () => {
  const clear = new Set([0]);
  assert.equal(followDiscovery({ level: 1 }, true, 0, true, clear), true);
  assert.equal(followDiscovery({ level: 0 }, true, 0, true, clear), false);
  assert.equal(followDiscovery({ level: 1 }, true, 0, true, new Set()), false);
  assert.equal(followDiscovery({ level: 1 }, false, 0, true, clear), false);
});
