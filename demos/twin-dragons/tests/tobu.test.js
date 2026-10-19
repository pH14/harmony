// SPDX-License-Identifier: AGPL-3.0-or-later
import test from "node:test";
import assert from "node:assert/strict";
import tobu, { decode, MAP_HEIGHTS } from "../src/games/tobu-tobu-girl.js";
import { validateTape } from "../src/heat.js";

function memory({ gamestate = 4, scene = 0, level = 1, x = 88, y = 84, progress = 0, step = 16, scrolled = 0, time = 32 } = {}) {
  const ram = new Uint8Array(0x400);
  Object.assign(ram, { 0xa5: gamestate, 0xa6: scene, 0xa7: level, 0xc2: x, 0xc3: y, 0x233: progress, 0x237: step, 0x230: scrolled, 0xb0: time });
  return ram;
}

test("Tobu Tobu Girl decode places the player on the level map, measured up from the bottom", () => {
  const start = decode(memory());
  assert.equal(start.level, 0);
  assert.equal(start.loading, false);
  assert.equal(start.health, 1);
  assert.equal(start.x, 88 - 24);
  assert.equal(start.y, MAP_HEIGHTS[0] - 155 + 84);
  const higher = decode(memory({ progress: 10, scrolled: 5 }));
  assert.equal(higher.climb, 165);
  assert.equal(start.y - higher.y, 165);
  assert.equal(decode(memory({ level: 3, step: 32 })).y, MAP_HEIGHTS[2] - 155 + 84);
  assert.throws(() => decode(new Uint8Array(0x800)));
});

test("Tobu Tobu Girl rollouts end on death and on level completion, and the ending counts every level", () => {
  assert.equal(tobu.ended(memory()), false);
  assert.equal(tobu.ended(memory({ scene: 1 })), true);
  assert.equal(tobu.levelComplete(memory({ scene: 1 })), false);
  assert.equal(tobu.levelComplete(memory({ scene: 2 })), true);
  const dead = decode(memory({ scene: 1 }));
  assert.equal(dead.loading, true);
  assert.equal(dead.health, 0);
  const ending = decode(memory({ gamestate: 10, scene: 1, level: 3 }));
  assert.equal(ending.cleared, tobu.levelCount);
});

test("Tobu Tobu Girl histories accept menu buttons and long holds that Twin Dragons rejects", () => {
  const tape = {
    format: "harmony-tobu-tobu-girl-browser-v1",
    rom_sha256: tobu.rom.sha256,
    core_revision: "core",
    endpoint_sha256: "a".repeat(64),
    actions: [{ buttons: 8, frames: 1 }, { buttons: 0, frames: 255 }],
  };
  assert.equal(validateTape(tape, tobu, "core"), 256);
  for (const changes of [
    { format: "harmony-twin-dragons-browser-v1" },
    { core_revision: "other" },
    { boot_level: 3 },
    { actions: [{ buttons: 0, frames: 256 }] },
    { actions: [{ buttons: 48, frames: 1 }] },
  ])
    assert.throws(() => validateTape({ ...tape, ...changes }, tobu, "core"));
});
