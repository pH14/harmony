// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { gunzipSync } from "node:zlib";
import { machine, expand } from "../../../demos/twin-dragons/tools/famidash-audit/runtime.mjs";

const [originalPath, fixedPath, rom, runtime] = process.argv.slice(2);
const original = JSON.parse(gunzipSync(await readFile(originalPath)));
const fixed = JSON.parse(gunzipSync(await readFile(fixedPath)));
const tape = expand(fixed.actions);
assert.deepEqual(tape, expand(original.actions).slice(0, tape.length));
const m = await machine(rom, runtime);
assert.equal(m.rom_sha256, fixed.rom_sha256);
assert.equal(m.core_sha256, fixed.core_sha256);
m.boot(fixed.options.side, fixed.boot_level, fixed.options.menu_delay, fixed.options.startup_released_frames);
let firstChange;
for (let i = 0; i < tape.length; i++) {
  m.core.step(tape[i], 1);
  const actual = m.observe();
  assert.deepEqual(actual, fixed.trace[i], `repaired contact replay frame ${i + 1}`);
  if (i >= 40 && JSON.stringify(actual) !== JSON.stringify(original.trace[i]) && firstChange === undefined)
    firstChange = i + 1;
}
assert.equal(firstChange, 1422);
assert.equal(fixed.trace[1421].dead, false);
assert.equal(fixed.trace[1421].vy, 0);
assert.equal(fixed.trace[1421].y, 881);
assert.equal(fixed.trace.findIndex(o => o.dead) + 1, 1425);
assert.equal(original.trace[1424].dead, false);
let reset = false;
for (let after = 1; after <= 180; after++) {
  m.core.step(0, 1);
  const o = m.observe();
  if (after <= 2) assert.ok(o.dead);
  if (after > 2 && !o.dead && Math.abs(o.x - fixed.trace[0].x) <= 256) { reset = true; break; }
}
assert.ok(reset, "contact correction must retain a normal reset");
console.log("ToE: unchanged input and approach; floor contact at 1422, death at 1425, normal reset.");
