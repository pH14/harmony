// SPDX-License-Identifier: AGPL-3.0-or-later
import test from "node:test";
import assert from "node:assert/strict";
import smb, { areaMap, areaRoom, decode, MAP_WIDTHS } from "../src/games/super-mario-bros.js";
import { validateTape } from "../src/heat.js";
import { smb_main_area } from "../rust/pkg/twin_dragons_browser.js";
import { initRustNode } from "../tools/rust-node.mjs";

await initRustNode();
const MAIN_AREAS = Array.from({ length: smb.levelCount }, (_, level) => smb_main_area(level));

function memory({ world = 0, level = 0, area = MAIN_AREAS[world * 4 + level], mode = 1, task = 3, state = 8, flag = 0, page = 1, x = 40, y = 176, camera = 0 } = {}) {
  const ram = new Uint8Array(0x800);
  Object.assign(ram, {
    0x75f: world, 0x75c: level, 0x74e: area >> 5, 0x74f: area & 31, 0x770: mode, 0x772: task, 0x0e: state,
    0x746: flag, 0xb5: page, 0x6d: x >> 8, 0x86: x & 255, 0xce: y, 0x71a: camera >> 8, 0x71c: camera & 255,
    0x7f8: 4, 0x7f9: 0, 0x7fa: 0,
  });
  return ram;
}

test("Super Mario Bros. decode names the level map and the progress through it", () => {
  const start = decode(memory());
  assert.deepEqual([start.level, start.loading, start.health, start.cleared, start.x], [0, false, 1, 0, 40]);
  assert.equal(start.y, 185);
  const later = decode(memory({ world: 3, level: 2, x: 1200, camera: 1100 }));
  assert.deepEqual([later.level, later.world, later.stage, later.x], [14, 3, 2, 1200]);
  const water = decode(memory({ world: 7, level: 3, area: 2 }));
  assert.deepEqual([water.level, water.loading, water.cleared], [areaRoom(31, 2), false, 31]);
  assert.equal(decode(memory({ task: 0 })).loading, true);
  assert.equal(MAP_WIDTHS.length, smb.levelCount);
  assert.throws(() => smb_main_area(smb.levelCount));
  assert.throws(() => decode(new Uint8Array(0x400)));
});

test("Super Mario Bros. separate areas get their own map rooms", () => {
  assert.deepEqual(areaMap(areaRoom(31, 2)), { id: areaRoom(31, 2), level: 31, name: "Underwater", label: "World 8-4 Underwater", height: 224 });
  assert.equal(areaMap(areaRoom(0, 66)).label, "World 1-1 Underground");
  assert.equal(areaMap(areaRoom(0, MAIN_AREAS[0])), null);
  assert.equal(areaMap(5), null);
  assert.equal(areaMap(areaRoom(32, 0)), null);
});

test("Super Mario Bros. decode reports deaths and cleared levels", () => {
  for (const dead of [memory({ state: 0x0b }), memory({ page: 2 })]) assert.equal(decode(dead).health, 0);
  const flag = memory({ flag: 1 });
  assert.deepEqual([decode(flag).goal, decode(flag).cleared], [true, 1]);
  const lastFlagTask = decode(memory({ level: 1, flag: 5 }));
  assert.deepEqual([lastFlagTask.level, lastFlagTask.cleared], [1, 2]);
  const nextLoad = decode(memory({ level: 2, flag: 5, task: 0 }));
  assert.deepEqual([nextLoad.loading, nextLoad.cleared], [true, 2]);
  assert.equal(decode(memory({ level: 1, state: 3, area: MAIN_AREAS[0] })).loading, true);
  assert.equal(decode(memory({ world: 0, level: 3, mode: 2 })).cleared, 4);
  assert.equal(decode(memory({ world: 7, level: 3, mode: 2 })).cleared, 32);
});

test("Super Mario Bros. histories reject menu buttons", () => {
  const tape = {
    format: "harmony-super-mario-bros-browser-v1",
    rom_sha256: smb.rom.sha256,
    core_revision: "core",
    endpoint_sha256: "a".repeat(64),
    actions: [{ buttons: 0x81, frames: 120 }],
  };
  assert.equal(validateTape(tape, smb, "core"), 120);
  assert.throws(() => validateTape({ ...tape, actions: [{ buttons: 8, frames: 1 }] }, smb, "core"));
  assert.throws(() => validateTape({ ...tape, actions: [{ buttons: 0, frames: 121 }] }, smb, "core"));
});
