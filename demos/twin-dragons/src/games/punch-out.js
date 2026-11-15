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
  FULL_HEALTH = 0x60,
  TRACK_HEIGHT = 16,
  TRACK_BOTTOM = 216,
  SPRITE_WIDTH = 32,
  SPRITE_HEIGHT = 66;

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
  const at = fighting ? placement(state) : { x: 0, y: 0 };
  return {
    ...state,
    level: state.opponent,
    loading: !fighting,
    health: fighting ? state.mac_health : 0,
    x: at.x,
    y: at.y,
  };
}

function scaled(lost, max) {
  return Math.min(max - 1, Math.max(0, Math.round((lost * max) / FULL_HEALTH)));
}

// The trail has no screen position for Mac, so a sample is placed by damage:
// x is the opponent's damage across the screen and y is Mac's damage in a band
// along the bottom edge. The Rust side uses the same mapping.
function placement(state) {
  return {
    x: scaled(FULL_HEALTH - state.opponent_health, SCREEN_WIDTH),
    y: TRACK_BOTTOM - scaled(FULL_HEALTH - state.mac_health, TRACK_HEIGHT),
  };
}

export function motion(ram) {
  const state = JSON.parse(punchout_observation(ram));
  if (ram[MODE] !== FIGHT) return [NO_ROOM, 0, 0, 0];
  const at = placement(state);
  return [state.opponent, at.x, at.y, 0];
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

// Mac's idle stance is in the same place on every fight camera, so one crop
// of the boxer serves as the marker. Pixels that change when the sprites are
// hidden belong to a sprite; the blue canvas is drawn by the background layer.
const MAC_BOX = { x: 102, y: 146, width: SPRITE_WIDTH, height: SPRITE_HEIGHT },
  RING_FLOOR = { x: 80, y: 200 };

export function buildSprites(engine) {
  engine.boot(0);
  const ram = engine.ramView(),
    start = engine.capture();
  engine.run(0, 1, true);
  const shown = engine.pixels().data.slice();
  engine.restore(start);
  for (let i = 0x200; i < 0x300; i += 4) ram[i] = 0xf8;
  engine.run(0, 1, true);
  const hidden = engine.pixels().data.slice();
  engine.restore(start);
  const data = new Uint8ClampedArray(MAC_BOX.width * MAC_BOX.height * 4),
    floor = (RING_FLOOR.y * SCREEN_WIDTH + RING_FLOOR.x) * 4;
  const same = (a, b) => shown[a] === b[0] && shown[a + 1] === b[1] && shown[a + 2] === b[2];
  for (let y = 0; y < MAC_BOX.height; y++)
    for (let x = 0; x < MAC_BOX.width; x++) {
      const s = ((MAC_BOX.y + y) * SCREEN_WIDTH + MAC_BOX.x + x) * 4,
        at = (y * MAC_BOX.width + x) * 4;
      if (shown[s] === hidden[s] && shown[s + 1] === hidden[s + 1] && shown[s + 2] === hidden[s + 2]) continue;
      if (same(s, shown.subarray(floor, floor + 3))) continue;
      data.set(shown.subarray(s, s + 4), at);
    }
  return { width: MAC_BOX.width, height: MAC_BOX.height, data };
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
  sprite: { width: SPRITE_WIDTH, height: SPRITE_HEIGHT, originX: 8, originY: SPRITE_HEIGHT - 16, icon: [0, 0, SPRITE_WIDTH, SPRITE_HEIGHT] },
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
