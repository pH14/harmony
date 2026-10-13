// SPDX-License-Identifier: AGPL-3.0-or-later
import { readFile, writeFile } from "node:fs/promises";
import { pathToFileURL } from "node:url";
import { createEngine } from "../src/emulator.js";
import twinDragons, { playerPose } from "../src/games/twin-dragons.js";
import { png } from "./png.mjs";
import { creditPNG } from "../src/media.js";
const SPRITE = twinDragons.sprite;
import { constArray, nesColor, pinnedSource, tilePixel } from "./twin-dragons-source.mjs";

const demo = pathToFileURL(process.cwd() + "/"),
  base = new URL("public/", demo);
const { root } = await pinnedSource(demo);
const chr = await readFile(new URL("src/graphics/twin_dragons.chr", root));
const anims = await readFile(new URL("src/anims.mfk", root), "utf8");
const palette = constArray(await readFile(new URL("src/levels.mfk", root), "utf8"), "pallete0");
const frames = [
  "player_idle_frame",
  "player_walk_frame0",
  "player_walk_frame1",
  "player_walk_frame2",
  "player_walk_frame3",
  "player_breath_frame",
  "player_hurt_frame",
].map((name) => constArray(anims, name));
const width = SPRITE.width * frames.length,
  height = SPRITE.height * 2,
  data = new Uint8Array(width * height * 4);
for (let facing = 0; facing < 2; facing++)
  frames.forEach(([chrMirror, center, count, ...sprites], pose) => {
    if (sprites.length !== count * 4) throw new Error("Unexpected player frame");
    const flip = (chrMirror ^ facing) === 0;
    for (let i = 0; i < count; i++) {
      const [dy, tile, attributes, dx] = sprites.slice(i * 4, i * 4 + 4);
      const left = SPRITE.originX + (flip ? center - dx : dx),
        top = SPRITE.originY + dy + 1,
        colors = palette.slice(16 + (attributes & 3) * 4, 20 + (attributes & 3) * 4);
      for (let y = 0; y < 8; y++)
        for (let x = 0; x < 8; x++) {
          const pixel = tilePixel(chr, 0, tile, flip ? 7 - x : x, y);
          if (!pixel) continue;
          const sx = pose * SPRITE.width + left + x,
            sy = facing * SPRITE.height + top + y;
          if (sx < pose * SPRITE.width || sx >= (pose + 1) * SPRITE.width || top + y < 0 || top + y >= SPRITE.height)
            throw new Error("Player frame exceeds sprite cell");
          data.set(nesColor(colors[pixel]), (sy * width + sx) * 4);
        }
    }
  });
const engine = await createEngine(base, twinDragons, {
  rom: new Uint8Array(await readFile(new URL("twin-dragons.nes", base))),
  wasmBinary: await readFile(new URL("engine/quicknes.wasm", base)),
});
for (const [buttons, frames] of [[0, 1], [0x80, 30], [0x40, 30], [0x41, 12]]) {
  engine.boot(0);
  engine.run(buttons, frames - 1);
  const ram = engine.memory(),
    pose = playerPose(ram);
  engine.run(buttons, 1, true);
  const frame = engine.pixels();
  let checked = 0;
  for (let y = 0; y < SPRITE.height; y++)
    for (let x = 0; x < SPRITE.width; x++) {
      const source = ((pose & 1) * SPRITE.height + y) * width + (pose >> 1) * SPRITE.width + x;
      if (!data[source * 4 + 3]) continue;
      const sx = ram[0x62] - SPRITE.originX + x,
        sy = ram[0x63] - SPRITE.originY + y - 8;
      if (sx < 0 || sx >= frame.width || sy < 0 || sy >= frame.height) continue;
      const target = (sy * frame.width + sx) * 4;
      for (let c = 0; c < 3; c++)
        if (frame.data[target + c] !== data[source * 4 + c])
          throw new Error("Player sprite sheet differs from the emulator");
      checked++;
    }
  if (checked < 60) throw new Error("Player sprite was not visible for comparison");
}
const image = await creditPNG(
  new Blob([png(width, height, data)]),
  "Player sprite sheet composed from the game's sprite tiles, frame layouts and palette; seven poses in both directions",
);
await writeFile(new URL("sprites.png", base), new Uint8Array(await image.arrayBuffer()));
console.log("Composed player sprites", width, height);
