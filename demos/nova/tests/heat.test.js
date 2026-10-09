// SPDX-License-Identifier: AGPL-3.0-or-later
import test from "node:test";
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
