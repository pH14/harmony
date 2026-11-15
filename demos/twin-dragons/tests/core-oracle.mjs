// SPDX-License-Identifier: AGPL-3.0-or-later
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { loadQuickNES } from "../src/cores/quicknes.js";

const demo = fileURLToPath(new URL("..", import.meta.url));
const root = join(demo, "../..");
const base = new URL("../public/", import.meta.url);
const roms = `${homedir()}/workspace/roms`;
const games = [
  { id: "twin-dragons", rom: process.env.TWIN_DRAGONS_ROM || join(demo, "public/twin-dragons.nes") },
  { id: "famidash", rom: process.env.FAMIDASH_ROM || join(demo, "public/famidash/famidash.nes") },
  { id: "super-mario-bros", rom: process.env.SMB_ROM || `${roms}/Super Mario Bros. (World).nes` },
  { id: "mega-man-2", rom: process.env.MM2_ROM || `${roms}/mm2.nes` },
  { id: "metroid", rom: process.env.METROID_ROM || `${roms}/Metroid (USA).nes` },
  { id: "pac-man", rom: process.env.PAC_MAN_ROM || `${roms}/pac-man.nes` },
];
const modes = [
  { name: "search", render: false, audio: false },
  { name: "play", render: true, audio: true },
];
const seed = process.env.ORACLE_SEED || "1";
const frames = process.env.ORACLE_FRAMES || "8000";
const WRAM = 2048,
  CART = 8192;

function nativeCore() {
  if (process.env.HARMONY_QUICKNES_CORE) return process.env.HARMONY_QUICKNES_CORE;
  const dir = join(demo, ".build/native-quicknes"),
    core = join(dir, process.platform === "darwin" ? "quicknes_libretro.dylib" : "quicknes_libretro.so");
  if (!existsSync(core)) {
    mkdirSync(dir, { recursive: true });
    execFileSync("sh", [join(root, "scripts/build-quicknes-core.sh"), core], { stdio: ["ignore", "ignore", "inherit"] });
  }
  return core;
}

function blockAt(state, offset) {
  const view = new DataView(state.buffer, state.byteOffset, state.byteLength);
  for (let at = 8; at + 8 <= state.length; ) {
    const length = view.getUint32(at + 4, true);
    if (offset < at + 8 + length)
      return `${String.fromCharCode(...state.subarray(at, at + 4))}+${offset - at - 8}`;
    at += 8 + length;
  }
  return "header";
}

function firstDifference(expected, actual) {
  for (let i = 0; i < expected.length; i++) if (expected[i] !== actual[i]) return i;
  return -1;
}

function hex(value) {
  return "0x" + value.toString(16).padStart(2, "0");
}

function compare(mode, manifest, nativeFrames, boundaries, rom, wasmBinary) {
  return loadQuickNES(base, rom, { wasmBinary, cartridgeMemory: manifest.cartridge_ram }).then((core) => {
    const { mod } = core;
    const cart = manifest.cartridge_ram ? CART : 0,
      record = WRAM + cart + manifest.state_len;
    if (core.stateSize !== manifest.state_len)
      return `state size ${core.stateSize} differs from native ${manifest.state_len}`;
    core.enableAudio(mode.audio);
    const wram = () => mod.HEAPU8.subarray(mod._nes_ram(), mod._nes_ram() + WRAM);
    const check = (index, frame) => {
      const at = index * record;
      const regions = [["WRAM", boundaries.subarray(at, at + WRAM), wram().slice(), 0]];
      if (cart) {
        const memory = core.ramPointer();
        regions.push(["cartridge RAM", boundaries.subarray(at + WRAM, at + WRAM + cart), mod.HEAPU8.slice(memory + WRAM, memory + WRAM + CART), 0x6000]);
      }
      const state = core.capture();
      regions.push(["state", boundaries.subarray(at + WRAM + cart, at + record), state, null]);
      for (const [region, expected, actual, address] of regions) {
        const i = firstDifference(expected, actual);
        if (i < 0) continue;
        const where = address === null ? `offset ${i} (${blockAt(expected, i)})` : `address ${hex(address + i)}`;
        return `action ${index} (frame ${frame}): ${region} ${where} native ${hex(expected[i])} browser ${hex(actual[i])}`;
      }
      return null;
    };
    let failure = check(0, 0);
    if (failure) return failure;
    core.restore(core.capture());
    let frame = 0;
    for (let index = 0; index < manifest.actions.length; index++) {
      const [buttons, hold] = manifest.actions[index];
      for (let f = 0; f < hold; f++) {
        core.runRendered(buttons, 1, mode.render);
        if (mode.audio) core.audio();
        const expected = nativeFrames.subarray(frame * WRAM, (frame + 1) * WRAM),
          actual = wram(),
          i = firstDifference(expected, actual);
        frame++;
        if (i >= 0)
          return `frame ${frame} (action ${index + 1}): WRAM address ${hex(i)} native ${hex(expected[i])} browser ${hex(actual[i])}`;
      }
      failure = check(index + 1, frame);
      if (failure) return failure;
    }
    return null;
  });
}

const core = nativeCore();
execFileSync("cargo", ["build", "--release", "--locked", "--manifest-path", join(root, "workloads/nes/Cargo.toml"), "--bin", "nes-core-oracle"], {
  stdio: ["ignore", "ignore", "inherit"],
});
const oracle = join(root, "workloads/nes/target/release/nes-core-oracle");
const wasmBinary = readFileSync(new URL("engine/quicknes.wasm", base));
let failed = false;
for (const game of games) {
  if (!existsSync(game.rom)) {
    console.log(`Skipping ${game.id}: no ROM at ${game.rom}.`);
    continue;
  }
  const rom = new Uint8Array(readFileSync(game.rom));
  for (const mode of modes) {
    const out = mkdtempSync(join(tmpdir(), `core-oracle-${game.id}-`));
    try {
      execFileSync(oracle, [...(mode.render ? ["--play"] : []), game.id, game.rom, core, seed, frames, out], {
        stdio: ["ignore", "ignore", "inherit"],
      });
      const manifest = JSON.parse(readFileSync(join(out, "tape.json"), "utf8")),
        nativeFrames = readFileSync(join(out, "frames.bin")),
        boundaries = readFileSync(join(out, "boundaries.bin"));
      const failure = await compare(mode, manifest, nativeFrames, boundaries, rom, wasmBinary);
      const summary = `${game.id} ${mode.name}: ${manifest.frames} frames, ${manifest.actions.length} actions (${manifest.setup_actions} setup)`;
      if (failure) {
        failed = true;
        console.log(`${summary}: diverged at ${failure}`);
      } else console.log(`${summary}: WRAM${manifest.cartridge_ram ? ", cartridge RAM" : ""} and state match`);
    } finally {
      rmSync(out, { recursive: true, force: true });
    }
  }
}
if (failed) process.exit(1);
