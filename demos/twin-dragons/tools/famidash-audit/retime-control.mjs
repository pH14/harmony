// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { isDeepStrictEqual } from "node:util";
import { readFile, writeFile } from "node:fs/promises";
import { gunzipSync, gzipSync } from "node:zlib";
import { machine, expand, compress, hash } from "./runtime.mjs";
import { symbolMachine } from "./symbol-runtime.mjs";

const [input, rom, runtime, output, option, prefixText, ...extra] = process.argv.slice(2);
if (!output) throw new Error("retime-control.mjs CLEARING_CONTROL ROM RUNTIME OUTPUT.json.gz [--prefix-frames N]");
assert.ok(extra.length === 0 && (option === undefined || option === "--prefix-frames"));
const bytes = await readFile(input), source = JSON.parse(gunzipSync(bytes));
assert.equal(source.purpose, "clearing-control", "retiming cannot validate a failing witness");
assert.equal(source.trace.at(-1).state, 3);
const m = process.env.FAMIDASH_SYMBOLS
  ? await symbolMachine(rom, runtime, process.env.FAMIDASH_SYMBOLS) : await machine(rom, runtime);
assert.equal(m.core_sha256, source.core_sha256);
const fullTape = expand(source.actions), actions = [], trace = [], skipped = [], inserted = [], intro = [];
assert.equal(fullTape.length, source.trace.length);
const prefixFrames = option === undefined ? fullTape.length : Number(prefixText);
assert.ok(Number.isSafeInteger(prefixFrames) && prefixFrames >= 41 && prefixFrames <= fullTape.length);
const tape = fullTape.slice(0, prefixFrames);
m.boot(source.options.side, source.boot_level, source.options.menu_delay ?? 0, source.options.startup_released_frames);
for (let i = 0; i < tape.length; i++) {
  const target = source.trace[i], previous = source.trace[i - 1];
  if (i >= 40 && isDeepStrictEqual(target, previous)) {
    skipped.push(i + 1);
    continue;
  }
  for (let retry = 0; ; retry++) {
    assert.ok(retry < 16, "destination must advance within sixteen repeated controller frames");
    m.core.step(tape[i], 1);
    const actual = m.observe();
    actions.push(tape[i]);
    trace.push(actual);
    assert.ok(!actual.dead && !actual.debug && !actual.paused,
      `destination control stopped at source frame ${i + 1}, destination frame ${trace.length}`);
    if (i < 39) {
      if (!isDeepStrictEqual(actual, target)) intro.push(i + 1);
      break;
    }
    if (isDeepStrictEqual(actual, target)) break;
    assert.ok(i >= 40 && isDeepStrictEqual(actual, previous), `mechanical state changed at source frame ${i + 1}`);
    inserted.push({ source_frame: i + 1, destination_frame: trace.length });
  }
}
assert.equal(trace.at(-1).state, prefixFrames === fullTape.length ? 3 : 2);
const witness = { ...source, rom_sha256: m.rom_sha256, core_sha256: m.core_sha256,
  purpose: prefixFrames === fullTape.length ? "clearing-control" : "positive-control-prefix",
  actions: compress(actions), trace,
  retiming: { source_sha256: hash(bytes), source_prefix_frames: prefixFrames,
    source_clearing_frames: fullTape.length, skipped_duplicate_source_frames: skipped,
    inserted_duplicate_destination_frames: inserted, intro_difference_frames: intro,
    invariant: "Every mechanical transition after frame 40 exactly matches the source clearing control; only repeated observations change duration." } };
delete witness.target_layout;
if (m.symbols) witness.target_layout = { kind: "symbol-based linear coordinates", symbols: m.symbols,
  debug_sha256: m.debug_sha256, startup_released_frames: source.options.startup_released_frames };
await writeFile(output, gzipSync(JSON.stringify(witness)));
console.log(JSON.stringify({ output, frames: trace.length, skipped_source_frames: skipped.length,
  inserted_destination_frames: inserted.length, rom_sha256: m.rom_sha256, endpoint: trace.at(-1) }));
