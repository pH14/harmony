// SPDX-License-Identifier: AGPL-3.0-or-later
const LEVEL_COUNT = 3,
  NEXT_LEVEL = 0x35,
  TOUCHED_GOAL = 0x4e3,
  BOOT = [
    [0, 60],
    [8, 2],
    [0, 240],
  ];

export function settled(gamestate, level, next) {
  return gamestate === 1 && (next === level + 1 || (level === LEVEL_COUNT - 1 && next === 0xff));
}
export function levelsCleared(o) {
  if (o.gamestate === 1 && !o.loading && o.goal) return o.level + 1;
  return o.gamestate === 1 || o.gamestate === 2 ? o.level : 0;
}
export function decode(ram) {
  if (ram.length !== 2048) throw new Error("Invalid Twin Dragons memory");
  const o = {
    x: ram[0x52] * 256 + ram[0x57] + ram[0x62],
    y: ram[0x63],
    health: ram[0x75],
    level: ram[0x42],
    next_level: ram[NEXT_LEVEL],
    gamestate: ram[0x9e],
    goal: ram[TOUCHED_GOAL] !== 0,
    on_ground: ram[0x6b] !== 0,
    facing: ram[0x74],
  };
  o.loading = !settled(o.gamestate, o.level, o.next_level);
  o.cleared = levelsCleared(o);
  return o;
}
export function playerPose(ram) {
  const animation = ram[0x71], frame = ram[0x72];
  const pose = animation === 1 ? 1 + Math.min(3, frame) : animation === 2 ? 5 : animation === 3 ? 6 : 0;
  return pose * 2 + (ram[0x74] ? 1 : 0);
}

export default {
  id: "twin-dragons",
  title: "Twin Dragons",
  core: "quicknes",
  rom: {
    file: "twin-dragons.nes",
    bytes: 40976,
    sha256: "7b64267068caab5379b833336e836d6de49c7d37cc5a4341b83d07cbccf0ef73",
  },
  ramBytes: 2048,
  levelCount: LEVEL_COUNT,
  pace: 85,
  decode,
  ended: (ram) => ram[0x75] === 0 || ram[0x9e] === 0,
  motion(ram) {
    const level = ram[0x42];
    const valid = settled(ram[0x9e], level, ram[0x35]) && ram[0x75] !== 0;
    return [valid ? level : 65535, ram[0x52] * 256 + ram[0x57] + ram[0x62], ram[0x63], playerPose(ram)];
  },
  boot(engine, level) {
    if (!Number.isInteger(level) || level < 0 || level >= LEVEL_COUNT)
      throw new Error("Unknown Twin Dragons starting level");
    for (const step of BOOT) engine.run(...step, true);
    if (level > 0) {
      const ram = engine.ramPointer();
      engine.mod.HEAPU8[ram + NEXT_LEVEL] = level;
      engine.mod.HEAPU8[ram + TOUCHED_GOAL] = 1;
      for (let frames = 0; frames < 600; frames++) {
        engine.run(0, 1, true);
        const o = engine.observation();
        if (!o.loading && o.level === level && !o.goal && o.on_ground) break;
      }
    } else
      for (let frames = 0; frames < 120 && !engine.observation().on_ground; frames++)
        engine.run(0, 1, true);
    const o = engine.observation();
    if (o.loading || o.level !== level || o.health !== 3 || o.goal || !o.on_ground)
      throw new Error("Setup did not reach the selected Twin Dragons level");
  },
};
