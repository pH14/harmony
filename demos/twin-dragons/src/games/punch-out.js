// SPDX-License-Identifier: AGPL-3.0-or-later
import { punchout_observation } from "../../rust/pkg/twin_dragons_browser.js";

const MODE = 0x00,
  OPPONENT = 0x01,
  FIGHT = 0x01,
  START = 0x08,
  INTRO_FRAMES = 60,
  NO_ROOM = 65535,
  SCREEN_WIDTH = 256,
  SCREEN_HEIGHT = 224,
  FULL_HEALTH = 0x60;

export const OPPONENTS = [
  "Glass Joe",
  "Von Kaiser",
  "Piston Honda",
  "Don Flamenco",
  "King Hippo",
  "Great Tiger",
  "Bald Bull",
  "Piston Honda 2",
  "Soda Popinski",
  "Bald Bull 2",
  "Don Flamenco 2",
  "Mr. Sandman",
  "Super Macho Man",
  "Mr. Dream",
];

export function decode(ram) {
  const state = JSON.parse(punchout_observation(ram));
  const fighting = ram[MODE] === FIGHT;
  return {
    ...state,
    level: state.opponent,
    loading: !fighting,
    health: fighting ? state.mac_health : 0,
    x: 0,
    y: 0,
  };
}

function scaled(lost, max) {
  return Math.min(max - 1, Math.max(0, Math.round((lost * max) / FULL_HEALTH)));
}

export function motion(ram) {
  const state = JSON.parse(punchout_observation(ram));
  if (ram[MODE] !== FIGHT) return [NO_ROOM, 0, 0, 0];
  return [
    state.opponent,
    scaled(FULL_HEALTH - state.opponent_health, SCREEN_WIDTH),
    scaled(FULL_HEALTH - state.mac_health, SCREEN_HEIGHT),
    0,
  ];
}

function runFrames(engine, buttons, frames, render = false) {
  engine.run(buttons, frames, render);
}

export function buildMap(engine, id) {
  engine.boot(id);
  engine.run(0, 1, true);
  const frame = engine.pixels();
  if (frame.width !== SCREEN_WIDTH || frame.height !== SCREEN_HEIGHT) throw new Error("Unexpected Punch-Out!! video size");
  return { width: frame.width, height: frame.height, data: frame.data.slice() };
}

export function buildSprites() {
  const size = 16,
    data = new Uint8ClampedArray(size * size * 4);
  for (let y = 0; y < size; y++)
    for (let x = 0; x < size; x++) {
      const d = Math.hypot(x + 0.5 - size / 2, y + 0.5 - size / 2);
      if (d > size / 2) continue;
      const at = (y * size + x) * 4;
      data.set([0xea, 0xf6, 0xff, 255], at);
    }
  return { width: size, height: size, data };
}

export default {
  id: "punch-out",
  title: "Punch-Out!!",
  core: "quicknes",
  rom: {
    upload: true,
    bytes: 262160,
    sha256: "2f06867500d5bc105cfa30f041fcb6bf5934d6b464779fbc4b041203af72b052",
  },
  ramBytes: 2048,
  levelCount: OPPONENTS.length,
  maxHealth: FULL_HEALTH,
  pace: 300,
  maxActionFrames: 120,
  menuButtons: true,
  system: "NES",
  assets: "",
  screen: { width: SCREEN_WIDTH, height: SCREEN_HEIGHT },
  sprite: { width: 16, height: 16, originX: 8, originY: 8, icon: [0, 0, 16, 16] },
  controls: { a: "Left punch", b: "Right punch" },
  decode,
  motion,
  campaign: true,
  boot(engine, level) {
    if (!Number.isInteger(level) || level < 0 || level >= OPPONENTS.length) throw new Error("Unknown Punch-Out!! opponent");
    const ram = engine.ramView();
    runFrames(engine, 0, 300);
    runFrames(engine, START, 2);
    runFrames(engine, 0, 400);
    ram[OPPONENT] = level;
    runFrames(engine, START, 2);
    for (let frames = 0; frames < 1200 && ram[MODE] !== FIGHT; frames++) runFrames(engine, 0, 1);
    runFrames(engine, 0, INTRO_FRAMES);
    const o = engine.observation();
    if (o.loading || o.opponent !== level || o.round !== 1 || o.mac_health !== FULL_HEALTH || o.opponent_health !== FULL_HEALTH)
      throw new Error("Setup did not reach the selected Punch-Out!! opponent");
  },
  catalog: {
    maps: OPPONENTS.map((name, id) => ({ id, name, label: name, width: SCREEN_WIDTH, height: SCREEN_HEIGHT })),
    levels: OPPONENTS.map((label, id) => ({ id, label, rooms: [id] })),
  },
  buildMap,
  buildSprites,
};
