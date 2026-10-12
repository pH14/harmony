// SPDX-License-Identifier: AGPL-3.0-or-later
import { readFile, writeFile, mkdir } from "node:fs/promises";
import { pathToFileURL } from "node:url";
import { createEngine, LEVEL_COUNT } from "../src/emulator.js";
import { png } from "./png.mjs";
import { creditPNG } from "../src/media.js";
import {
  constArray,
  loadLevel,
  nesColor,
  pinnedSource,
  tilePixel,
} from "./twin-dragons-source.mjs";

const demo = pathToFileURL(process.cwd() + "/"),
  base = new URL("public/", demo);
const { root } = await pinnedSource(demo);
const chr = await readFile(new URL("src/graphics/twin_dragons.chr", root));
if (chr.length !== 8192) throw new Error("Unexpected Twin Dragons pattern tables");
const metatiles = constArray(
  await readFile(new URL("src/metatiles.mfk", root), "utf8"),
  "metatiles",
);
const palette = constArray(
  await readFile(new URL("src/levels.mfk", root), "utf8"),
  "pallete0",
);
if (palette.length !== 32 || metatiles.length % 4)
  throw new Error("Unexpected upstream palette or metatile table");
const engine = await createEngine(base, {
  rom: new Uint8Array(await readFile(new URL("twin-dragons.nes", base))),
  wasmBinary: await readFile(new URL("engine/quicknes.wasm", base)),
});
const TOP = 8,
  HEIGHT = 224;
await mkdir(new URL("maps/", base), { recursive: true });
const maps = [];
for (let id = 0; id < LEVEL_COUNT; id++) {
  const level = await loadLevel(root, id);
  const colors = palette.slice(0, 16);
  if (level.background !== null) colors[0] = level.background;
  const width = level.width * 16,
    data = new Uint8Array(width * HEIGHT * 4);
  for (let y = TOP; y < TOP + HEIGHT; y++)
    for (let x = 0; x < width; x++) {
      const metaRow = y >> 4,
        column = x >> 4,
        metatile = metaRow < 2 ? 0 : level.meta[column][metaRow];
      let tile = 0xff;
      if (metatile !== 0xff) {
        if (metatile * 4 >= metatiles.length) throw new Error("Unknown metatile");
        tile = metatiles[metatile * 4 + ((x >> 3) & 1) * 2 + ((y >> 3) & 1)];
      }
      const attribute = level.attrs[x >> 5][y >> 5],
        quadrant = ((y >> 4) & 1) * 2 + ((x >> 4) & 1),
        group = (attribute >> (quadrant * 2)) & 3,
        pixel = tilePixel(chr, 1, tile, x & 7, y & 7);
      data.set(nesColor(pixel ? colors[group * 4 + pixel] : colors[0]), ((y - TOP) * width + x) * 4);
    }
  engine.boot(id);
  const frame = engine.pixels(),
    camera = engine.mod.HEAPU8[engine.mod._nes_ram() + 0x52] * 256 +
      engine.mod.HEAPU8[engine.mod._nes_ram() + 0x57];
  let same = 0;
  for (let y = 32; y < frame.height; y++)
    for (let x = 0; x < frame.width; x++) {
      const a = (y * frame.width + x) * 4,
        b = (y * width + camera + x) * 4;
      if (frame.data.slice(a, a + 3).every((v, i) => v === data[b + i])) same++;
    }
  const agreement = same / ((frame.height - 32) * frame.width);
  if (frame.height !== HEIGHT || agreement < 0.97)
    throw new Error(`Level ${id + 1} map differs from the emulator (${agreement})`);
  const image = await creditPNG(
    new Blob([png(width, HEIGHT, data)]),
    "Level map composed from the game's level data, background tiles and palette",
  );
  await writeFile(new URL(`maps/${id}.png`, base), new Uint8Array(await image.arrayBuffer()));
  const preview = new Uint8Array(256 * 96 * 4),
    cropWidth = Math.min(640, width);
  for (let y = 0; y < 96; y++)
    for (let x = 0; x < 256; x++) {
      const source = (Math.floor((y * HEIGHT) / 96) * width + Math.floor((x * cropWidth) / 256)) * 4;
      preview.set(data.subarray(source, source + 4), (y * 256 + x) * 4);
    }
  const thumbnail = await creditPNG(
    new Blob([png(256, 96, preview)]),
    "Level selector thumbnail from the level map",
  );
  await writeFile(new URL(`maps/${id}-preview.png`, base), new Uint8Array(await thumbnail.arrayBuffer()));
  maps.push({ id, name: level.name, label: `Level ${id + 1}`, width, height: HEIGHT, file: `maps/${id}.png` });
  console.log("Composed", id, level.name, width, HEIGHT, `emulator agreement ${(agreement * 100).toFixed(1)}%`);
}
const levels = maps.map((map) => ({ id: map.id, label: map.label, rooms: [map.id] }));
await writeFile(new URL("maps.json", base), JSON.stringify({ maps, levels }));
