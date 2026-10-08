// SPDX-License-Identifier: AGPL-3.0-or-later
import { readFile, writeFile } from "node:fs/promises";
import { pathToFileURL } from "node:url";
import { createEngine } from "../src/emulator.js";
import { png } from "./png.mjs";
import { creditPNG } from "../src/media.js";
const base = pathToFileURL(process.cwd() + "/public/");
for (const [level, width, file] of [
  [0, 1280, "level-one.png"],
  [40, 3584, "level-one-main.png"],
]) {
  const e = await createEngine(base, {
    rom: new Uint8Array(await readFile(new URL("nova.nes", base))),
    wasmBinary: await readFile(new URL("engine/quicknes.wasm", base)),
  });
  e.boot();
  if (level) {
    const ram = e.mod.HEAPU8,
      p = e.mod._nova_ram();
    ram[p + 0xa7] = level;
    ram[p + 0xa9] = 1;
    e.run(0, 120, true);
    if (e.observation().level !== level) throw new Error("Wrong camera room");
  }
  const height = 224,
    data = new Uint8Array(width * height * 4),
    done = new Set();
  // Offline artwork only. Live exploration never uses these camera writes.
  for (let x = 52; x <= width + 80; x += 2) {
    const ram = e.mod.HEAPU8,
      p = e.mod._nova_ram();
    for (const [lo, hi] of [
      [0x25, 0x26],
      [0x3e, 0x3f],
    ]) {
      ram[p + lo] = (x * 16) & 255;
      ram[p + hi] = (x * 16) >> 8;
    }
    ram.fill(0, p + 0x4ec, p + 0x4fb);
    ram[p + 0x27] = 1;
    ram[p + 0x28] = 0;
    ram[p + 0x41] = 1;
    ram[p + 0x40] = 0;
    ram[p + 0x4b] = 4;
    e.run(0, 1, true);
    const r = e.memory(),
      scroll = Math.floor((r[0x1f] + r[0x20] * 256) / 16),
      pix = e.pixels();
    for (let col = 0; col < pix.width; col++) {
      const world = scroll + col;
      if (world < 0 || world >= width || done.has(world)) continue;
      for (let y = 0; y < height; y++)
        data.set(
          pix.data.subarray(
            (y * pix.width + col) * 4,
            (y * pix.width + col + 1) * 4,
          ),
          (y * width + world) * 4,
        );
      done.add(world);
    }
  }
  if (done.size < width)
    throw new Error(`Incomplete panorama: ${done.size}/${width}`);
  const image = await creditPNG(
    new Blob([png(width, height, data)]),
    "Static level panorama captured with an offline camera",
  );
  await writeFile(
    new URL(file, base),
    new Uint8Array(await image.arrayBuffer()),
  );
  console.log("Captured original level panorama", width, height);
}
