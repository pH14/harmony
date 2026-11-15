// SPDX-License-Identifier: AGPL-3.0-or-later
import test from "node:test";
import { NO_ROOM } from "../src/swarm.js";
import assert from "node:assert/strict";
import { Heatmap, cellKey, routeIds, validateTape } from "../src/heat.js";
import twinDragons, { decode } from "../src/games/twin-dragons.js";
import { QUICKNES_REVISION as CORE_REVISION, canonicalize } from "../src/cores/quicknes.js";
import { initRustNode } from "../tools/rust-node.mjs";
const ROM_SHA256 = twinDragons.rom.sha256;
test("heat cools with the search's rollouts and running time without discarding histories, and separates rooms", () => {
  const heat = new Heatmap(),
    point = { observation: { level: 0, x: 52, y: 184 }, retained: 7 };
  const cell = heat.visit(point, 100);
  assert.equal(cellKey(point.observation), "0:1:5");
  heat.work = 2000;
  assert.equal(heat.value(cell, 100), 0.5);
  assert.equal(heat.value(cell, 2600), 0.25);
  heat.visit(point, 100);
  assert.equal(cell.heat, 1.5);
  assert.deepEqual(cell.ids, [7]);
  assert.equal(cell.visits, 2);
  heat.work = 4000;
  assert.equal(heat.value(cell, 100), 0.75);
  heat.visit(
    { ...point, observation: { ...point.observation, level: 40 } },
    100,
  );
  assert.equal(heat.cells.size, 2);
  assert.deepEqual(cell.ids, [7]);
  for (let n = 0; n < 16; n++) heat.visit({ ...point, retained: n }, 100);
  assert.equal(cell.ids.length, 12);
  assert.equal(cell.ids[0], 15);
});
test("paused and inactive searches hold their heat until they run again", () => {
  const original = new Heatmap(),
    child = new Heatmap(),
    point = { observation: { level: 0, x: 52, y: 184 }, retained: 7 },
    cell = original.visit(point, 100);
  original.setRunning(false, 100);
  const frozen = original.value(cell, 100);
  child.visit({ ...point, retained: "1:0" }, 100);
  child.work += 10000;
  assert.equal(original.value(cell, 600000), frozen);
  original.setRunning(true, 600000);
  assert.equal(original.value(cell, 602500), frozen / 2);
  assert.deepEqual(cell.ids, [7]);
  assert.equal(cell.visits, 1);
});
test("colors compare each cell with the hottest cell of its own level", () => {
  const heat = new Heatmap(),
    cool = heat.visit({ observation: { level: 0, x: 52, y: 184 }, retained: null }, 0);
  heat.work = 20000;
  const hot = heat.visit({ observation: { level: 0, x: 116, y: 184 }, retained: null }, 0);
  const other = heat.visit({ observation: { level: 3, x: 52, y: 184 }, retained: null }, 0);
  assert.deepEqual(heat.color(hot, 0), [245, 89, 62]);
  assert.deepEqual(heat.color(hot, 5000), [245, 89, 62], "The hottest cell stays red as time passes");
  assert.deepEqual(heat.color(other, 0), [245, 89, 62], "Each level has its own hottest cell");
  const color = heat.color(cool, 0);
  assert.ok(color[2] > color[0], "A cooled cell stays visible and turns blue");
  assert.equal(cool.visits, 1);
  assert.deepEqual(cool.ids, []);
  heat.visit({ observation: { level: 0, x: 52, y: 184 }, retained: null }, 0);
  heat.visit({ observation: { level: 0, x: 52, y: 184 }, retained: null }, 0);
  assert.deepEqual(heat.color(cool, 0), [245, 89, 62], "Colors follow new visits");
});
test("visible and selected route numbers survive arrivals and bounded history", () => {
  const heat = new Heatmap(),
    point = { observation: { level: 0, x: 52, y: 184 }, retained: "1:0" },
    cell = heat.visit(point, 0);
  heat.visit({ ...point, retained: "1:1" }, 1);
  heat.visit(point, 2);
  assert.equal(cell.routes.get("1:0"), 1);
  assert.equal(cell.routes.get("1:1"), 2);
  assert.deepEqual(routeIds(cell), ["1:0", "1:1"]);
  heat.select(cell, "1:0");
  for (let id = 2; id < 15; id++)
    heat.visit({ ...point, retained: `1:${id}` }, id + 1);
  assert.equal(cell.ids.length, 12);
  assert.equal(cell.routes.size, 13);
  assert.equal(cell.routes.get(routeIds(cell)[0]), 4);
  assert.equal(cell.routes.get("1:0"), 1);
  const pinned = routeIds(cell, "1:0");
  assert.equal(pinned.length, 13);
  assert.equal(pinned[0], "1:0");
  assert.deepEqual(routeIds(cell, "unknown"), routeIds(cell));
  assert.equal(cell.ids.length, 12);
  heat.visit(point, 20);
  assert.equal(cell.routes.size, 12);
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
test("ten thousand arrivals retain only recent and selected or pending route metadata", () => {
  const heat = new Heatmap(), point = { observation: { level: 0, x: 52, y: 184 }, retained: 0 };
  const cell = heat.retain(point, 0);
  heat.select(cell, 0);
  heat.retain({ ...point, retained: 1 }, 1);
  heat.pending(cell, 1, 41);
  for (let id = 2; id < 10000; id++) {
    heat.retain({ ...point, retained: id }, id);
    assert.ok(cell.ids.length <= 12);
    assert.ok(cell.routes.size <= 14);
  }
  assert.equal(cell.routes.get(0), 1);
  assert.equal(cell.routes.get(1), 2);
  assert.equal(cell.nextRoute, 10000);
  heat.forget(Array.from({ length: 10000 }, (_, id) => id), 41);
  assert.deepEqual(cell.ids, []);
  assert.deepEqual(routeIds(cell, 0), [0]);
  assert.equal(cell.routes.size, 1, "cached playing history survives backend eviction");
  assert.equal(cell.routes.get(0), 1);
  heat.clearPins();
  assert.equal(cell.routes.size, 0);
  heat.retain(point, 10001);
  assert.equal(cell.routes.get(0), 10001, "forgotten unselected re-arrivals receive a new ordinal");
});

test("pending request rollover and stale responses cannot release a newer route pin", () => {
  const heat = new Heatmap(), point = { observation: { level: 0, x: 52, y: 184 }, retained: 0 };
  const cell = heat.retain(point, 0);
  heat.select(cell, 0);
  heat.retain({ ...point, retained: 1 }, 1);
  heat.pending(cell, 1, 10);
  for (let id = 2; id < 30; id++) heat.retain({ ...point, retained: id }, id);
  heat.pending(cell, 1, 11);
  heat.releasePending(10);
  heat.forget([1], 10);
  assert.equal(cell.routes.get(1), 2);
  assert.equal(cell.routes.get(0), 1);
  heat.select(cell, 1);
  heat.releasePending(11);
  assert.equal(cell.routes.get(1), 2, "fetch completion transfers the pending label to selection");
  assert.equal(cell.routes.has(0), false);
  heat.forget([1], 11);
  assert.equal(cell.routes.get(1), 2);
  heat.select(null);
  assert.equal(cell.routes.has(1), false);
  heat.pending(cell, cell.ids[0], 12);
  heat.clearPins();
  heat.releasePending(12);
  assert.equal(heat.pins.size, 0, "branch reset releases both pins");
});

test("a visible button can pin its old ordinal without retaining an unbounded history", () => {
  const heat = new Heatmap(), point = { observation: { level: 0, x: 52, y: 184 }, retained: 0 };
  const cell = heat.retain(point, 0), visibleOrdinal = cell.routes.get(0);
  for (let id = 1; id < 10000; id++) heat.retain({ ...point, retained: id }, id);
  assert.equal(cell.routes.has(0), false);
  heat.pending(cell, 0, 1, visibleOrdinal);
  assert.equal(cell.routes.get(0), 1);
  assert.equal(cell.routes.size, 13);
  heat.select(cell, 0);
  heat.releasePending(1);
  heat.select(null);
  assert.equal(cell.routes.size, 12);
  heat.pending(cell, 0, 2, 10001);
  assert.equal(cell.routes.has(0), false, "a future ordinal is not a visible past route");
});

test("selection pins are global across cells and release prior cells on rollover", () => {
  const heat = new Heatmap(), cells = [];
  for (let column = 0; column < 4; column++) {
    const point = { observation: { level: 0, x: column * 32 + 8, y: 184 }, retained: `${column}:0` };
    const cell = heat.retain(point, 0);
    cells.push(cell);
    heat.select(cell, point.retained);
    heat.pending(cell, point.retained, column);
    for (let id = 1; id < 30; id++) heat.retain({ ...point, retained: `${column}:${id}` }, id);
    assert.equal(heat.pins.size, 2);
    assert.equal(cells.reduce((sum, c) => sum + c.routes.size - c.ids.length, 0), 1);
    assert.equal(cell.routes.get(point.retained), 1);
  }
  heat.clearPins();
  assert.ok(cells.every(cell => cell.routes.size === 12));
  assert.equal(heat.pins.size, 0);
});

test("foreign or replaced cells cannot apply captured ordinals or replace owned pins", () => {
  const heat = new Heatmap(), other = new Heatmap();
  const point = { observation: { level: 0, x: 52, y: 184 }, retained: 0 };
  const cell = heat.retain(point, 0), foreign = other.retain(point, 0);
  heat.select(cell, 0);
  heat.pending(cell, 0, 4);
  const selected = heat.pins.get("selected"), pending = heat.pins.get("pending");
  heat.select(foreign, 99, 1);
  heat.pending(foreign, 99, 5, 1);
  assert.equal(heat.pins.get("selected"), selected);
  assert.equal(heat.pins.get("pending"), pending);
  assert.deepEqual([...foreign.routes], [[0, 1]]);
  heat.cells.delete(cell.key);
  const replacement = heat.retain({ ...point, retained: 1 }, 1);
  heat.pending(cell, 99, 6, 1);
  assert.equal(heat.pins.get("pending"), pending);
  assert.equal(cell.routes.has(99), false);
  assert.deepEqual([...replacement.routes], [[1, 1]]);
  heat.clearPins();
});

test("forget leaves untouched cell storage intact and still clears a missing pending pin", () => {
  const heat = new Heatmap(), point = { observation: { level: 0, x: 52, y: 184 }, retained: 0 };
  const cell = heat.retain(point, 0), ids = cell.ids;
  heat.forget([99]);
  assert.equal(cell.ids, ids);
  heat.pending(cell, 0, 7);
  for (let id = 1; id < 20; id++) heat.retain({ ...point, retained: id }, id);
  const currentIds = cell.ids;
  heat.forget([0], 7);
  assert.equal(cell.ids, currentIds);
  assert.equal(cell.routes.has(0), false);
});

test("history admission rejects wrong identities and malformed inputs", () => {
  const tape = {
    format: "harmony-twin-dragons-browser-v1",
    rom_sha256: ROM_SHA256,
    core_revision: CORE_REVISION,
    endpoint_sha256: "a".repeat(64),
    actions: [{ buttons: 129, frames: 60 }],
  };
  assert.equal(validateTape(tape, twinDragons, CORE_REVISION), 60);
  for (const changes of [
    { endpoint_sha256: "invalid" },
    { format: "other" },
    { rom_sha256: "other" },
    { core_revision: "other" },
    { boot_level: -1 },
    { boot_level: 3 },
    { boot_level: 0.5 },
    { actions: [{ buttons: 12, frames: 60 }] },
    { actions: [{ buttons: 48, frames: 60 }] },
    { actions: [{ buttons: 192, frames: 60 }] },
    { actions: [{ buttons: 129, frames: 121 }] },
    { actions: [{ buttons: 1, frames: NaN }] },
    { actions: Array(2000).fill({ buttons: 1, frames: 120 }) },
  ])
    assert.throws(() => validateTape({ ...tape, ...changes }, twinDragons, CORE_REVISION));
});
test("Twin Dragons memory decode reads world position and progress and refuses other layouts", async () => {
  await initRustNode();
  const ram = new Uint8Array(2048);
  ram[0x52] = 1;
  ram[0x57] = 0x20;
  ram[0x62] = 0x40;
  ram[0x63] = 100;
  ram[0x75] = 2;
  ram[0x42] = 1;
  ram[0x35] = 2;
  ram[0x9e] = 1;
  const state = decode(ram);
  assert.deepEqual(
    [state.x, state.y, state.health, state.level, state.loading, state.cleared],
    [352, 100, 2, 1, false, 1],
  );
  ram[0x4e3] = 1;
  assert.equal(decode(ram).cleared, 2);
  ram[0x35] = 1;
  assert.deepEqual([decode(ram).loading, decode(ram).cleared], [true, 1]);
  ram[0x9e] = 2;
  assert.deepEqual([decode(ram).loading, decode(ram).cleared], [true, 1]);
  assert.throws(() => decode(new Uint8Array(10240)));
  assert.throws(() => decode(ram.subarray(0, 1024)));
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
    [0, { width: 320, height: 224 }],
    [49, { width: 320, height: 224 }],
  ]);
  const trail = new Uint16Array([
    0, 0, 8, 184, 0, 4, 0, 24, 184, 0, 8, 0, 40, 184, 0,
    12, 0, 56, 184, 0, 16, 0, 72, 184, 0,
    20, NO_ROOM, 0, 0, 0, 24, 49, 8, 184, 0,
    28, 49, 24, 184, 0, 32, 49, 40, 184, 0,
  ]);
  const restored = new Uint16Array([0, 49, 200, 184, 0, 4, 49, 216, 184, 0]);
  heat.visitMotion([trail, restored], maps, 100);
  assert.deepEqual([...heat.cells.keys()], ["0:0:5", "0:1:5", "0:2:5", "49:0:5", "49:1:5", "49:6:5", "49:7:5"]);
  for (const cell of heat.cells.values()) {
    assert.equal(cell.visits, 1);
    assert.equal(cell.heat, 1);
    assert.deepEqual(cell.ids, []);
  }
  assert.equal(heat.cells.has("49:3:5"), false, "A restore must not paint a connecting path");
  const endpoint = { observation: { level: 49, x: 224, y: 192 }, retained: "1:7" };
  heat.retain(endpoint, 100);
  heat.retain(endpoint, 100);
  const cell = heat.cells.get("49:7:5");
  assert.equal(cell.heat, 1, "Retained endpoints must not count activity twice");
  assert.deepEqual(cell.ids, ["1:7"]);
  heat.visitMotion([restored], maps, 100);
  assert.ok(Math.abs(cell.heat - 2) < 0.001, "A fresh attempt counts even if it visits the same cell");
});

test("motion heat excludes menus, unknown levels and off-map positions", () => {
  const heat = new Heatmap(), maps = new Map([[2, { width: 1536, height: 224 }]]);
  const samples = new Uint16Array([
    0, 2, 528, 168, 0, 4, NO_ROOM, 0, 0, 0,
    8, 2, 1536, 168, 0, 12, 2, 16, 4095, 0,
    16, 99, 16, 168, 0,
  ]);
  heat.visitMotion([samples], maps, 0);
  assert.deepEqual([...heat.cells.keys()], ["2:16:5"]);
  const retained = heat.retain({ observation: { level: 2, x: 536, y: 176 }, retained: 9 }, 0);
  assert.equal(retained.heat, 1);
  assert.deepEqual(retained.ids, [9]);
});
