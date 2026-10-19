// SPDX-License-Identifier: AGPL-3.0-or-later
import { readFile } from "node:fs/promises";

export const LEVEL_FILES = ["test", "level1", "level2"];

const QUICKNES_COLORS = [
  [102, 102, 102], [0, 42, 136], [20, 18, 168], [59, 0, 164],
  [92, 0, 126], [110, 0, 64], [108, 7, 0], [87, 29, 0],
  [52, 53, 0], [12, 73, 0], [0, 82, 0], [0, 79, 8],
  [0, 64, 78], [0, 0, 0], [0, 0, 0], [0, 0, 0],
  [174, 174, 174], [21, 95, 218], [66, 64, 254], [118, 39, 255],
  [161, 27, 205], [184, 30, 124], [181, 50, 32], [153, 79, 0],
  [108, 110, 0], [56, 135, 0], [13, 148, 0], [0, 144, 50],
  [0, 124, 142], [0, 0, 0], [0, 0, 0], [0, 0, 0],
  [254, 254, 254], [100, 176, 254], [147, 144, 254], [199, 119, 254],
  [243, 106, 254], [254, 110, 205], [254, 130, 112], [235, 159, 35],
  [189, 191, 0], [137, 217, 0], [93, 229, 48], [69, 225, 130],
  [72, 206, 223], [79, 79, 79], [0, 0, 0], [0, 0, 0],
  [254, 254, 254], [193, 224, 254], [212, 211, 254], [233, 200, 254],
  [251, 195, 254], [254, 197, 235], [254, 205, 198], [247, 217, 166],
  [229, 230, 149], [208, 240, 151], [190, 245, 171], [180, 243, 205],
  [181, 236, 243], [184, 184, 184], [0, 0, 0], [0, 0, 0],
];

// The browser core outputs RGB565, which the frontend widens back to 8 bits.
export function nesColor(index) {
  const [r, g, b] = QUICKNES_COLORS[index & 63];
  return [
    Math.floor(((r >> 3) * 255) / 31),
    Math.floor(((g >> 2) * 255) / 63),
    Math.floor(((b >> 3) * 255) / 31),
    255,
  ];
}

export async function pinnedSource(demo) {
  const versions = await readFile(
    new URL("../../workloads/nes/twin-dragons-versions.env", demo),
    "utf8",
  );
  const commit = versions.match(/^TWIN_DRAGONS_COMMIT=([a-f0-9]{40})$/m)?.[1];
  if (!commit) throw new Error("Missing pinned Twin Dragons source revision");
  return {
    commit,
    root: new URL(`.build/twin-dragons-source/twin-dragons-nes-${commit}/`, demo),
  };
}

function numbers(text) {
  return [...text.replace(/\/\/[^\n]*/g, "").matchAll(/-?\$[0-9a-f]+|-?\d+/gi)].map(([n]) =>
    n.replace("-", "").startsWith("$")
      ? (n.startsWith("-") ? -1 : 1) * parseInt(n.replace("-", "").slice(1), 16)
      : Number(n),
  );
}

export function constArray(source, name) {
  const match = source.match(new RegExp(`const array ${name}\\s*=\\s*\\[([^\\]]*)\\]`));
  if (!match) throw new Error("Missing upstream table " + name);
  return numbers(match[1]);
}

function rle(bytes, height) {
  const columns = [];
  let column = [];
  for (let i = 0; i < bytes.length; i += 2) {
    for (let n = 0; n < bytes[i]; n++) column.push(bytes[i + 1]);
    if (column.length > height) throw new Error("Invalid run-length column");
    if (column.length === height) {
      columns.push(column);
      column = [];
    }
  }
  if (column.length) throw new Error("Truncated run-length column");
  return columns;
}

export async function loadLevel(root, id) {
  const name = LEVEL_FILES[id];
  const data = (extension) => new URL(`src/levels/data/${name}.${extension}`, root);
  const meta = rle(await readFile(data("meta")), 16);
  const attrs = rle(await readFile(data("attrs")), 8);
  const tmx = await readFile(new URL(`src/levels/data/${name}.tmx`, root), "utf8");
  const width = Number(tmx.match(/<map [^>]*\swidth="(\d+)"/)[1]),
    height = Number(tmx.match(/<map [^>]*\sheight="(\d+)"/)[1]);
  const tiles = tmx
    .match(/<layer [^>]*name="Tiles"[\s\S]*?<data encoding="csv">([\s\S]*?)<\/data>/)[1]
    .split(",")
    .map((n) => Number(n.trim()));
  if (height !== 15 || tiles.length !== width * height || meta.length !== width ||
      attrs.length * 2 !== width)
    throw new Error("Unexpected level layout " + name);
  for (let x = 0; x < width; x++)
    for (let y = 0; y < height; y++)
      if (((tiles[y * width + x] - 1) & 255) !== meta[x][y])
        throw new Error(`Exported level data differs from ${name}.tmx`);
  const source = await readFile(new URL(`src/levels/level${id}.mfk`, root), "utf8");
  const background = source.match(/set_background_color\(\$([0-9a-f]{2})\)/i);
  return { name, width, meta, attrs, background: background ? parseInt(background[1], 16) : null };
}

export function tilePixel(chr, table, tile, x, y) {
  const offset = table * 4096 + tile * 16 + y,
    bit = 7 - x;
  return ((chr[offset] >> bit) & 1) | (((chr[offset + 8] >> bit) & 1) << 1);
}
