// SPDX-License-Identifier: AGPL-3.0-or-later
import { readFile, writeFile } from "node:fs/promises";
import { gunzipSync } from "node:zlib";
import { png } from "../png.mjs";
import { machine } from "./runtime.mjs";

const [casePath, romPath, runtime, frameText, output] = process.argv.slice(2);
if (!output) throw new Error("render-entry.mjs CASE ROM RUNTIME FRAME OUTPUT.png");
const bytes = await readFile(casePath), record = JSON.parse(casePath.endsWith(".gz") ? gunzipSync(bytes) : bytes);
const m = await machine(romPath, runtime), frame = Number(frameText);
m.boot(record.side ?? record.options.side, record.level ?? record.boot_level);
let position = 0;
outer: for (const { buttons, frames } of record.actions) for (let i = 0; i < frames; i++) {
  m.core.runRendered(buttons, 1, position >= frame - 2);
  if (++position === frame) break outer;
}
if (position !== frame) throw new Error("frame exceeds the input history");
const pixels = m.core.pixels();
await writeFile(output, png(pixels.width, pixels.height, pixels.data));
console.log(JSON.stringify(m.observe()));
