// SPDX-License-Identifier: AGPL-3.0-or-later
import { readFile } from "node:fs/promises";
import pacMan from "../src/games/pac-man.js";
import { createEngine } from "../src/emulator.js";

const [romPath, tapePath] = process.argv.slice(2);
if (!romPath || !tapePath) {
  console.error("usage: node tools/pac-man-replay.mjs PAC_MAN_ROM witness-input.json");
  process.exit(2);
}
const START = 8;
const base = new URL("../public/", import.meta.url);
const rom = new Uint8Array(await readFile(romPath));
const engine = await createEngine(base, pacMan, { rom, wasmBinary: await readFile(new URL("engine/quicknes.wasm", base)) });
const tape = JSON.parse(await readFile(tapePath, "utf8"));

const ram = engine.ramView();
engine.run(0, 300, true);
engine.run(START, 2, true);
engine.run(0, 100, true);
engine.run(START, 2, true);
engine.run(0, 252, true);
let frames = 0;
for (const action of tape.actions) {
  engine.run(action.buttons, action.hold_frames, true);
  frames += action.hold_frames;
}
const end = engine.observation();
console.log(JSON.stringify({ actions: tape.actions.length, frames, mazes: end.cleared, score: end.score, dots: end.dots, lives: end.health, mode: ram[0x3f] }));
