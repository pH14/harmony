// SPDX-License-Identifier: AGPL-3.0-or-later
import { readFile, writeFile } from "node:fs/promises";
import { png } from "./png.mjs";
import { creditPNG } from "../src/media.js";
const versions = await readFile(new URL("../../../workloads/nes/nova-versions.env", import.meta.url), "utf8");
const commit = versions.match(/^NOVA_COMMIT=([a-f0-9]{40})$/m)?.[1];
if (!commit) throw new Error("Missing pinned Nova source revision");
const chr = await readFile(`.build/NovaTheSquirrel-${commit}/chr/spnova.chr`);
if (chr.length !== 1024) throw new Error("Unexpected Nova sprite tiles");
// Original player.s tile layouts and global.s's $12, $2a, $30 palette,
// using the pinned QuickNES default RGB table and its RGB565 video output.
const colors = [[0, 0, 0, 0], [65, 64, 255, 255], [90, 230, 49, 255], [255, 255, 255, 255]];
const frames = [[0, 1, 2, 3, 4, 5],
  [0, 1, 2, 0x30, 4, 5], [0, 1, 2, 0x31, 0x33, 0x34],
  [0, 1, 2, 0x30, 0x35, 0x36], [0, 1, 2, 0x32, 0x33, 0x37],
  [0, 1, 0x0d, 0x0e, 0x0a, 0x0b]];
const width = 96, height = 48, data = new Uint8Array(width * height * 4);
for (let facing = 0; facing < 2; facing++)
  for (let frame = 0; frame < frames.length; frame++)
    for (let y = 0; y < 24; y++)
      for (let x = 0; x < 16; x++) {
        const sx = facing ? 15 - x : x, tile = frames[frame][Math.floor(y / 8) * 2 + Math.floor(sx / 8)];
        const bit = 7 - (sx % 8), row = y % 8;
        const color = ((chr[tile * 16 + row] >> bit) & 1) | (((chr[tile * 16 + row + 8] >> bit) & 1) << 1);
        data.set(colors[color], ((facing * 24 + y) * width + frame * 16 + x) * 4);
      }
const image = await creditPNG(new Blob([png(width, height, data)]), "Original Nova player tiles; six poses and both directions. Extracted from the pinned game source without changing the artwork.");
await writeFile("public/nova-sprites.png", new Uint8Array(await image.arrayBuffer()));
