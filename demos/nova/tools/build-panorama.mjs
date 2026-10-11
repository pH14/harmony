// SPDX-License-Identifier: AGPL-3.0-or-later
import { readFile, writeFile, mkdir } from "node:fs/promises";
import { pathToFileURL } from "node:url";
import { createEngine } from "../src/emulator.js";
import { png } from "./png.mjs";
import { creditPNG } from "../src/media.js";
const versions = await readFile(
  new URL("../../../workloads/nes/nova-versions.env", import.meta.url),
  "utf8",
);
const commit = versions.match(/^NOVA_COMMIT=([a-f0-9]{40})$/m)?.[1];
if (!commit) throw new Error("Missing pinned Nova source revision");
const base = pathToFileURL(process.cwd() + "/public/"),
  source = `.build/NovaTheSquirrel-${commit}`;
const debug = await readFile(".build/nova/nova.debug.dbg", "utf8");
const invincible = Number(debug.match(/name="PlayerInvincible"[^\n]*val=(0x[a-f0-9]+)/i)?.[1]);
if (invincible !== 0x4c9) throw new Error("Pinned player visibility symbol drifted");
const table = await readFile(source + "/src/levels.s", "utf8");
const names = [
  ...table.split("MasterLevelListH:")[0].matchAll(/<([a-z0-9_]+)/g),
].map((m) => m[1]);
const ids = [
  ...table
    .split(".enum LevelId")[1]
    .split(".endenum")[0]
    .matchAll(/^\s*([A-Za-z][A-Za-z0-9_]*)\s*$/gm),
].map((m) => m[1]);
if (names.length !== ids.length || names.length !== 57)
  throw new Error("Unexpected pinned map catalog");
const e = await createEngine(base, {
  rom: new Uint8Array(await readFile(new URL("nova.nes", base))),
  wasmBinary: await readFile(new URL("engine/quicknes.wasm", base)),
});
const root = e.boot(),
  maps = [];
await mkdir("public/maps", { recursive: true });
for (const [id, name] of names.entries()) {
  const definition = JSON.parse(
      await readFile(`${source}/levels/${name}.json`, "utf8"),
    ),
    assembly = await readFile(`${source}/levels/${name}.s`, "utf8");
  e.restore(root);
  const ram = e.mod.HEAPU8,
    p = e.mod._nova_ram();
  ram[p + 0xa7] = id;
  ram[p + 0xa9] = 1;
  ram[p + 0x394] = 0;
  e.run(0, 120, true);
  if (e.observation().level !== id) throw new Error("Wrong camera map " + name);
  const columns = definition.Meta.Width / 16;
  const objects = definition.Layers.flatMap((l) => l.Data).filter(
    (o) => o && Number.isFinite(o.X) && Number.isFinite(o.Y),
  );
  const last = Math.max(
    0,
    ...objects.map(
      (o) =>
        Math.floor(
          Math.min(definition.Meta.Width - 1, o.X + (o.W || 1) - 1) / 16,
        ) +
        Math.floor(
          Math.min(definition.Meta.Height - 1, o.Y + (o.H || 1) - 1) / 15,
        ) *
          columns,
    ),
  );
  const runtimeWidth = (last + 1) * 256,
    width = Math.min(definition.Meta.Width * 16, runtimeWidth),
    rows = Math.ceil(runtimeWidth / width),
    height = rows * 224;
  if (runtimeWidth > 4096)
    throw new Error("Map exceeds original page buffer " + name);
  const label =
    name === "intro_a"
      ? "Introduction"
      : name === "intro_b"
        ? "Main level"
        : name.replaceAll("_", " ").replace(/\b\w/g, (s) => s.toUpperCase());
  const links = [...assembly.matchAll(/LWriteCol[^\n]*LevelId::(\w+)/g)].map(
    (m) => ids.indexOf(m[1]),
  );
  if (links.some((i) => i < 0)) throw new Error("Unknown door " + name);
  maps.push({
    id,
    name,
    label,
    width,
    height,
    runtimeWidth,
    links,
    file: `maps/${id}.png`,
  });
  const data = new Uint8Array(width * height * 4),
    done = new Set();
  let cameraTick = 0;
  // Build-only camera writes never enter live search or replay.
  for (const x of [
    ...Array(512).fill(52),
    ...Array.from(
      { length: Math.ceil((runtimeWidth + 80 - 52) / 4) },
      (_, i) => 52 + i * 4,
    ),
  ]) {
    for (const [lo, hi] of [
      [0x25, 0x26],
      [0x3e, 0x3f],
    ]) {
      ram[p + lo] = (x * 16) & 255;
      ram[p + hi] = (x * 16) >> 8;
    }
    ram.fill(0, p + 0x4ec, p + 0x4fb);
    ram[p + 0x394] = 0;
    ram[p + 0x27] = 8;
    ram[p + 0x28] = 0;
    ram[p + 0x41] = 1;
    ram[p + 0x40] = 0;
    ram[p + 0x4b] = 4;
    ram[p + invincible] = 2;
    e.run(cameraTick++ % 2, 1, true);
    if (cameraTick < 3) continue;
    const r = e.memory(),
      scroll = Math.floor((r[0x1f] + r[0x20] * 256) / 16),
      pix = e.pixels();
    for (let col = 0; col < pix.width; col++) {
      const world = scroll + col;
      if (world < 0 || world >= runtimeWidth || done.has(world)) continue;
      const targetX = world % width,
        row = Math.floor(world / width);
      for (let y = 0; y < 224; y++)
        data.set(
          pix.data.subarray(
            (y * pix.width + col) * 4,
            (y * pix.width + col + 1) * 4,
          ),
          ((row * 224 + y) * width + targetX) * 4,
        );
      done.add(world);
    }
  }
  if (done.size !== runtimeWidth)
    throw new Error(
      `Incomplete ${name}: ${done.size}/${runtimeWidth}, first missing ${Array.from(
        { length: runtimeWidth },
        (_, i) => i,
      )
        .filter((i) => !done.has(i))
        .slice(0, 8)}`,
    );
  const image = await creditPNG(
    new Blob([png(width, height, data)]),
    "Static map panorama captured with an offline camera",
  );
  await writeFile(
    new URL(`maps/${id}.png`, base),
    new Uint8Array(await image.arrayBuffer()),
  );
  const preview = new Uint8Array(256 * 96 * 4), cropWidth = Math.min(640, width), cropHeight = Math.min(224, height);
  for (let y = 0; y < 96; y++) for (let x = 0; x < 256; x++) {
    const source = ((height - cropHeight + Math.floor(y * cropHeight / 96)) * width + Math.floor(x * cropWidth / 256)) * 4;
    preview.set(data.subarray(source, source + 4), (y * 256 + x) * 4);
  }
  const thumbnail = await creditPNG(new Blob([png(256, 96, preview)]), "Level selector thumbnail from the original map panorama");
  await writeFile(new URL(`maps/${id}-preview.png`, base), new Uint8Array(await thumbnail.arrayBuffer()));
  console.log("Captured", id, label, width, height);
}
const levels = maps.slice(0, 44).map((map) => {
  const rooms = new Set([map.id]),
    visit = (id) => {
      for (const next of maps[id].links)
        if (next >= 45 && !rooms.has(next)) {
          rooms.add(next);
          visit(next);
        }
    };
  visit(map.id);
  return {
    id: map.id,
    world: Math.floor(map.id / 8) + 1,
    label: `Level ${map.id + 1}`,
    rooms: [...rooms],
  };
});
await writeFile(new URL("maps.json", base), JSON.stringify({ maps, levels }));
