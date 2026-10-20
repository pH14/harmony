// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { loadQuickNES } from "../../../../../../demos/twin-dragons/src/cores/quicknes.js";

const [romPath, runtimePath, mode = "original"] = process.argv.slice(2);
if (!romPath || !runtimePath || !["original", "fixed"].includes(mode))
  throw new Error("Usage: node replay.mjs ROM RUNTIME_DIRECTORY [original|fixed]; runtime contains engine/quicknes.js and engine/quicknes.wasm");
const input = JSON.parse(await readFile(new URL("input.json", import.meta.url))),
  rom = await readFile(romPath),
  base = pathToFileURL(resolve(runtimePath) + "/"),
  core = await loadQuickNES(base, rom, { wasmBinary: await readFile(new URL("engine/quicknes.wasm", base)) }),
  ram = () => core.mod.HEAPU8.subarray(core.mod._nes_ram(), core.mod._nes_ram() + 2048);
if (mode === "original")
  assert.equal(createHash("sha256").update(rom).digest("hex"), input.rom_sha256);

function observation() {
  const r = ram();
  return {
    x: new DataView(r.buffer, r.byteOffset).getInt32(0x4a2, true) + r[0x43e],
    y: r[0x4a7] * 240 + r[0x4a6] + r[0x442],
    dead: !!((r[0x7b] | r[0x7c]) & 1),
    state: r[0x498],
  };
}
function boot() {
  for (const [buttons, frames] of [[0, 22], [1, 1], [0, 936], [128, 1], [0, 20], [1, 1], [0, 90]])
    core.step(buttons, frames);
  ram()[0x47b] = input.level;
  core.step(1, 1);
  core.step(0, 40);
  assert.equal(ram()[0x47b], input.level);
  assert.equal(observation().state, 2);
  assert.equal(observation().x, -239);
}
boot();
const root = core.capture();
let frames = 0, death = null;
outer: for (const action of input.actions)
  for (let i = 0; i < action.frames; i++) {
    core.step(action.buttons, 1);
    frames++;
    assert.equal(ram()[0x595], 0, "debug mode stays off");
    if (frames >= 100) assert.equal(ram()[0x4a1], 0, "spawn invincibility has expired");
    if (observation().dead) {
      death = frames;
      break outer;
    }
  }
if (mode === "original") {
  assert.equal(death, null);
  assert.equal(frames, input.frames);
  assert.deepEqual(observation(), { ...input.endpoint, dead: false, state: 2 });
  for (let i = 0; i < 300; i++) {
    core.step(0, 1);
    assert.equal(observation().dead, false);
    assert.equal(observation().y, 897);
  }
  assert.equal(observation().x, 5301);
  console.log("Original: escaped to (4268, 897) at frame 1463; alive at (5301, 897) 300 frames later.");
} else {
  assert.equal(death, input.fixed_death_frame);
  console.log(`Fixed: escape input dies at frame ${death}, (${observation().x}, ${observation().y}).`);
}

core.restore(root);
const control = JSON.parse(await readFile(new URL("control.json", import.meta.url)));
frames = 0;
winning: for (const { buttons, frames: hold } of control.actions)
  for (let i = 0; i < hold; i++) {
    core.step(buttons, 1);
    frames++;
    assert.equal(observation().dead, false, "the clearing route remains alive");
    if (observation().state === 3) break winning;
  }
assert.equal(observation().state, 3);
assert.equal(frames, control.frames);
console.log("Control: the existing winning route still clears at frame 3764.");
