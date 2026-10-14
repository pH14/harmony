// SPDX-License-Identifier: AGPL-3.0-or-later
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  finishedLevel,
  followExit,
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

test("the followed path moves only along exits from the level it is on", () => {
  const levels = Array.from({ length: 32 }, (_, id) => ({ id }));
  const path = { level: 1, levels: [0, 1] };
  assert.equal(followExit(path, 4, 5, levels), false, "an exit from another lineage's level leaves the path alone");
  assert.equal(followExit(path, 1, 12, levels), true, "a warp exit moves the path straight to its destination");
  assert.deepEqual(path, { level: 12, levels: [0, 1, 12] });
  assert.equal(followExit(path, 12, 40, levels), false, "an exit past the last level is ignored");
  assert.equal(followExit(path, 12, 12, levels), false);
});
test("a state finishes only its own level", () => {
  assert.equal(finishedLevel({ cleared: 2 }, 1), 1);
  assert.equal(finishedLevel({ cleared: 12 }, 12), null, "a warp arrival does not finish the levels it skipped");
  assert.equal(finishedLevel({ cleared: 3 }, null), null);
});
