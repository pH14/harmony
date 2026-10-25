// SPDX-License-Identifier: AGPL-3.0-or-later
import { execFileSync } from "node:child_process";
import { mkdir, readdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { GAMES, sha256Hex } from "../src/emulator.js";

const source = process.argv[2];
if (!source) {
  console.error("usage: node tools/bundle-roms.mjs <directory of .nes files or .zip archives>");
  process.exit(2);
}
const wanted = new Map([...GAMES.values()].filter((game) => game.rom.upload).map((game) => [game.rom.sha256, game]));
const found = new Map();
for (const name of await readdir(source)) {
  const path = join(source, name);
  const files = name.endsWith(".nes")
    ? [await readFile(path)]
    : name.endsWith(".zip")
      ? execFileSync("unzip", ["-Z1", path], { encoding: "utf8" })
          .split("\n")
          .filter((entry) => entry.endsWith(".nes"))
          .map((entry) => execFileSync("unzip", ["-p", path, entry], { maxBuffer: 64 << 20 }))
      : [];
  for (const bytes of files) {
    const game = wanted.get(await sha256Hex(bytes));
    if (game && bytes.length === game.rom.bytes) found.set(game.id, bytes);
  }
}
const out = new URL("../public/roms/", import.meta.url);
await mkdir(out, { recursive: true });
for (const [id, bytes] of found) await writeFile(new URL(`${id}.nes`, out), bytes);
const games = [...wanted.values()].map((game) => game.id).filter((id) => found.has(id));
await writeFile(new URL("manifest.json", out), JSON.stringify({ games }) + "\n");
const missing = [...wanted.values()].map((game) => game.id).filter((id) => !found.has(id));
console.log(`Bundled ${games.length} ROMs: ${games.join(", ")}`);
if (missing.length) {
  console.error(`No matching ROM for: ${missing.join(", ")}`);
  process.exitCode = 1;
}
