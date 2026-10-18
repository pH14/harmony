// SPDX-License-Identifier: AGPL-3.0-or-later
import { readFile, writeFile } from "node:fs/promises";
import { gunzipSync } from "node:zlib";
import { machine, expand, hash } from "./runtime.mjs";

const [input, rom, runtime, output, tailText = "0"] = process.argv.slice(2);
if (!output) throw new Error("coin-paths.mjs WITNESS ROM RUNTIME OUTPUT [RELEASED_TAIL]");
const bytes = await readFile(input), witness = JSON.parse(input.endsWith(".gz") ? gunzipSync(bytes) : bytes);
const m = await machine(rom, runtime), side = witness.side ?? witness.options.side, level = witness.level ?? witness.boot_level;
m.boot(side, level, witness.setup?.menu_delay ?? witness.options?.menu_delay ?? 0,
  witness.setup?.startup_released_frames ?? witness.options?.startup_released_frames);
const events = [], tape = expand(witness.actions), tail = Number(tailText);
if (!Number.isInteger(tail) || tail < 0 || tail > 20000) throw new Error("invalid released tail");
let previous = [0, 0, 0], frame = 0;
for (const button of tape.concat(Array(tail).fill(0))) {
  m.core.step(button, 1); frame++;
  const timers = [...m.ram().slice(0x4b3, 0x4b6)];
  for (let coin = 0; coin < 3; coin++)
    if (!previous[coin] && timers[coin]) events.push({ coin: coin + 1, frame, observation: m.observe() });
  previous = timers;
  if (m.observe().dead || m.observe().state !== 2) break;
}
const result = { schema: 1, input, input_sha256: hash(bytes), rom_sha256: m.rom_sha256, core_sha256: m.core_sha256,
  timer_addresses: [0x4b3, 0x4b4, 0x4b5], side, level, events, frames: frame, endpoint: m.observe() };
await writeFile(output, JSON.stringify(result, null, 2));
console.log(JSON.stringify(result));
