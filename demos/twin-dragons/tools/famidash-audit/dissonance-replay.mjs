// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { gunzipSync } from "node:zlib";
import { machine, hash, expand, replay } from "./runtime.mjs";
import { symbolMachine } from "./symbol-runtime.mjs";

const [casePath, romPath, runtimePath, flag] = process.argv.slice(2);
if (!runtimePath || (flag && flag !== "--fixed"))
  throw new Error("usage: dissonance-replay.mjs CASE.json.gz ROM RUNTIME [--fixed]");
const bytes = await readFile(casePath), witness = JSON.parse(casePath.endsWith(".gz") ? gunzipSync(bytes) : bytes);
assert.equal(witness.engine, "dissonance-campaign");
if (witness.target_layout && !process.env.FAMIDASH_SYMBOLS)
  throw new Error("this witness requires FAMIDASH_SYMBOLS pointing to the matching build's DBG file");
const m = process.env.FAMIDASH_SYMBOLS ? await symbolMachine(romPath, runtimePath, process.env.FAMIDASH_SYMBOLS) : await machine(romPath, runtimePath);
const fixed = flag === "--fixed";
assert.equal(m.core_sha256, witness.core_sha256, "recorded QuickNES core required");
if (!fixed) assert.equal(m.rom_sha256, witness.rom_sha256);
m.boot(witness.options.side, witness.boot_level);
let frames = 0, died = false;
outer: for (const { buttons, frames: hold } of witness.actions) {
  assert.ok([0, 1, 128].includes(buttons) && Number.isInteger(hold) && hold > 0 && hold <= 200000);
  for (let i = 0; i < hold; i++) {
    m.core.step(buttons, 1);
    const o = m.observe();
    frames++;
    if (fixed && o.dead) { died = true; break outer; }
    assert.deepEqual(o, witness.trace[frames - 1], `mechanical state at frame ${frames}`);
  }
}
if (fixed) {
  assert.ok(died, "patched input must die");
  assert.ok(frames > witness.first_suspect_frame - 64, "death must be near the reviewed escape");
  assert.ok(frames <= witness.first_suspect_frame + 16, "death must occur at the reviewed escape");
  const originalDeath = witness.trace.findIndex(o => o.dead);
  assert.ok(originalDeath < 0 || frames < originalDeath + 1, "the original later death is not a repair");
} else assert.equal(frames, witness.trace.length);
const endpoint = m.observe();
let controlFrames = null;
if (witness.control_actions) {
  const control = await replay(m, witness.options.side, witness.target_level, expand(witness.control_actions));
  assert.equal(control.at(-1).state, 3);
  assert.equal(control.length, witness.control_frames);
  assert.equal(hash(JSON.stringify(control)), witness.control_sha256, "clearing control must be unchanged");
  controlFrames = control.length;
}
console.log(JSON.stringify({ status: fixed ? "patched-campaign-replay-verified" : "campaign-replay-verified",
  frames, endpoint, control_frames: controlFrames, case_sha256: hash(bytes), rom_sha256: m.rom_sha256 }));
