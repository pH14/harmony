// SPDX-License-Identifier: AGPL-3.0-or-later
import test from "node:test";
import { NO_ROOM } from "../src/swarm.js";
import assert from "node:assert/strict";
import { Heatmap, cellKey, routeIds, validateTape } from "../src/heat.js";
import {
  ROM_SHA256,
  CORE_REVISION,
  decode,
  canonicalize,
} from "../src/emulator.js";
test("heat cools without discarding histories, and separates rooms", () => {
  const heat = new Heatmap(),
    point = { observation: { level: 0, x: 52, y: 184 }, retained: 7 };
  const cell = heat.visit(point, 100);
  assert.equal(cellKey(point.observation), "0:1:5");
  assert.equal(heat.value(cell, 6100), 0.5);
  heat.visit(point, 6100);
  assert.equal(cell.heat, 1.5);
  assert.deepEqual(cell.ids, [7]);
  assert.equal(cell.visits, 2);
  assert.equal(heat.value(cell, 12100), 0.75);
  heat.visit(
    { ...point, observation: { ...point.observation, level: 40 } },
    12100,
  );
  assert.equal(heat.cells.size, 2);
  assert.deepEqual(cell.ids, [7]);
  for (let n = 0; n < 16; n++) heat.visit({ ...point, retained: n }, 12100);
  assert.equal(cell.ids.length, 12);
  assert.equal(cell.ids[0], 15);
});
test("paused and inactive searches retain heat until their own clock resumes", () => {
  const original = new Heatmap(),
    child = new Heatmap(),
    point = { observation: { level: 0, x: 52, y: 184 }, retained: 7 },
    cell = original.visit(point, 100);
  original.setRunning(false, 6100);
  const frozen = original.value(cell, 6100);
  child.visit({ ...point, retained: "1:0" }, 6100);
  assert.equal(original.clock(603100), 6100);
  assert.equal(original.value(cell, 603100), frozen);
  assert.notDeepEqual(
    child.color(child.cells.get("0:1:5"), 603100),
    original.color(cell, 603100),
  );
  original.setRunning(true, 603100);
  original.setRunning(true, 606100);
  assert.equal(original.clock(609100), 12100);
  assert.equal(original.value(cell, 609100), frozen / 2);
  assert.deepEqual(cell.ids, [7]);
  assert.equal(cell.visits, 1);
});
test("cooled exploration remains visible without inventing visits or states", () => {
  const heat = new Heatmap(),
    cell = heat.visit(
      { observation: { level: 0, x: 52, y: 184 }, retained: null },
      0,
    );
  assert.deepEqual(heat.color(cell, 600000), [64, 124, 181]);
  assert.equal(cell.visits, 1);
  assert.deepEqual(cell.ids, []);
  assert.equal(heat.cells.has("0:2:5"), false);
});
test("route numbers survive new arrivals, the rolling window and repeat visits", () => {
  const heat = new Heatmap(),
    point = { observation: { level: 0, x: 52, y: 184 }, retained: "1:0" },
    cell = heat.visit(point, 0);
  heat.visit({ ...point, retained: "1:1" }, 1);
  heat.visit(point, 2);
  assert.equal(cell.routes.get("1:0"), 1);
  assert.equal(cell.routes.get("1:1"), 2);
  assert.deepEqual(routeIds(cell), ["1:0", "1:1"]);
  for (let id = 2; id < 15; id++)
    heat.visit({ ...point, retained: `1:${id}` }, id + 1);
  assert.equal(cell.ids.length, 12);
  assert.equal(cell.routes.size, 15);
  assert.equal(cell.routes.get(routeIds(cell)[0]), 4);
  assert.equal(cell.routes.get("1:0"), 1);
  const pinned = routeIds(cell, "1:0");
  assert.equal(pinned.length, 13);
  assert.equal(pinned[0], "1:0");
  assert.deepEqual(routeIds(cell, "unknown"), routeIds(cell));
  assert.equal(cell.ids.length, 12);
  heat.visit(point, 20);
  assert.equal(cell.routes.size, 15);
  assert.equal(cell.routes.get("1:0"), 1);
  assert.equal(routeIds(cell)[0], "1:0");
  const another = heat.visit(
    {
      ...point,
      observation: { ...point.observation, level: 49 },
      retained: "1:99",
    },
    21,
  );
  assert.equal(another.routes.get("1:99"), 1);
  const otherSearch = new Heatmap().visit(point, 0);
  assert.equal(otherSearch.routes.get("1:0"), 1);
  assert.deepEqual(routeIds({ ids: [] }), []);
});
test("history admission rejects wrong identities and malformed inputs", () => {
  const tape = {
    format: "harmony-nova-browser-v1",
    rom_sha256: ROM_SHA256,
    core_revision: CORE_REVISION,
    endpoint_sha256: "a".repeat(64),
    actions: [{ buttons: 129, frames: 60 }],
  };
  assert.equal(validateTape(tape), 60);
  for (const changes of [
    { endpoint_sha256: "invalid" },
    { format: "other" },
    { rom_sha256: "other" },
    { core_revision: "other" },
    { boot_level: -1 },
    { boot_level: 44 },
    { boot_level: 0.5 },
    { actions: [{ buttons: 12, frames: 60 }] },
    { actions: [{ buttons: 48, frames: 60 }] },
    { actions: [{ buttons: 192, frames: 60 }] },
    { actions: [{ buttons: 129, frames: 121 }] },
    { actions: [{ buttons: 1, frames: NaN }] },
    { actions: Array(2000).fill({ buttons: 1, frames: 120 }) },
  ])
    assert.throws(() => validateTape({ ...tape, ...changes }));
});
test("Nova memory decode reads resources and refuses truncated buffers", () => {
  const ram = new Uint8Array(10240);
  ram[0x25] = 128;
  ram[0x26] = 3;
  ram[0x27] = 9;
  ram[0x28] = 64;
  ram[0x4b] = 4;
  ram[0x1a00] = 2;
  ram[0x271f] = 5;
  const state = decode(ram);
  assert.deepEqual(
    [state.x, state.y, state.health, state.ability, state.cleared],
    [56, 148, 4, 2, 2],
  );
  assert.throws(() => decode(ram.subarray(0, 2048)));
});
test("canonical snapshots reject missing and oversized blocks", () => {
  assert.throws(() => canonicalize(new Uint8Array(8)));
  const bytes = new Uint8Array(16);
  bytes.set(new TextEncoder().encode("PPUR"), 8);
  new DataView(bytes.buffer).setUint32(12, 100, true);
  assert.throws(() => canonicalize(bytes));
});

test("heat marks actual crossed cells once per attempt without creating routes or fake bridges", () => {
  const heat = new Heatmap(), maps = new Map([
    [0, { width: 320, height: 224, runtimeWidth: 320 }],
    [49, { width: 320, height: 224, runtimeWidth: 320 }],
  ]);
  const trail = new Uint16Array([
    0, 0, 8, 184, 0, 4, 0, 24, 184, 0, 8, 0, 40, 184, 0,
    12, 0, 56, 184, 0, 16, 0, 72, 184, 0,
    20, NO_ROOM, 0, 0, 0, 24, 49, 8, 184, 0,
    28, 49, 24, 184, 0, 32, 49, 40, 184, 0,
  ]);
  const restored = new Uint16Array([0, 49, 200, 184, 0, 4, 49, 216, 184, 0]);
  heat.visitMotion([trail, restored], maps, 100);
  assert.deepEqual([...heat.cells.keys()], ["0:0:5", "0:1:5", "0:2:5", "49:0:5", "49:1:5", "49:6:5"]);
  for (const cell of heat.cells.values()) {
    assert.equal(cell.visits, 1);
    assert.equal(cell.heat, 1);
    assert.deepEqual(cell.ids, []);
  }
  assert.equal(heat.cells.has("49:2:5"), false, "A restore must not paint a connecting path");
  const endpoint = { observation: { level: 49, x: 216, y: 184 }, retained: "1:7" };
  heat.retain(endpoint, 100);
  heat.retain(endpoint, 100);
  const cell = heat.cells.get("49:6:5");
  assert.equal(cell.heat, 1, "Retained endpoints must not count activity twice");
  assert.deepEqual(cell.ids, ["1:7"]);
  heat.visitMotion([restored], maps, 100);
  assert.equal(cell.heat, 2, "A fresh attempt counts even if it visits the same cell");
});

test("motion heat projects vertical pages and excludes menus and off-map positions", () => {
  const heat = new Heatmap(), maps = new Map([[13, { width: 256, height: 672, runtimeWidth: 768 }]]);
  const samples = new Uint16Array([
    0, 13, 528, 184, 0, 4, NO_ROOM, 0, 0, 0,
    8, 13, 768, 184, 0, 12, 13, 16, 4095, 0,
    16, 99, 16, 184, 0,
  ]);
  heat.visitMotion([samples], maps, 0);
  assert.deepEqual([...heat.cells.keys()], ["13:0:19"]);
  const retained = heat.retain({ observation: { level: 13, x: 16, y: 632 }, retained: 9 }, 0);
  assert.equal(retained.heat, 1);
  assert.deepEqual(retained.ids, [9]);
});
