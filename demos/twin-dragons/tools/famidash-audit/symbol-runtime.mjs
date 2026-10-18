// SPDX-License-Identifier: AGPL-3.0-or-later
import { readFile } from "node:fs/promises";
import { machine, hash } from "./runtime.mjs";

export async function symbolMachine(rom, runtime, debugFile) {
  const bytes = await readFile(debugFile), symbols = new Map(), segments = new Map(), fileSymbols = new Map();
  for (const line of bytes.toString().split("\n")) {
    const fields = Object.fromEntries([...line.matchAll(/(\w+)=("[^"]*"|[^,\t]+)/g)].map(m => [m[1], m[2]]));
    if (line.startsWith("seg\t") && fields.ooffs)
      segments.set(Number(fields.id), { start: Number(fields.start), offset: Number(fields.ooffs) });
    if (!line.startsWith("sym\t")) continue;
    const name = line.match(/name="([^"]+)"/), value = line.match(/(?:^|,)val=0x([\da-f]+)/i);
    if (name && value) {
      symbols.set(name[1], Number.parseInt(value[1], 16));
      if (fields.seg !== undefined) fileSymbols.set(name[1], { segment: Number(fields.seg), address: Number.parseInt(value[1], 16) });
    }
  }
  const romBytes = await readFile(rom);
  const fileOffset = name => {
    const symbol = fileSymbols.get(name), segment = symbol && segments.get(symbol.segment);
    if (!segment) throw new Error(`missing ROM symbol: ${name}`);
    return segment.offset + symbol.address - segment.start;
  };
  const namedLevels = new Map([...fileSymbols.keys()].filter(name => name.startsWith("level_data_"))
    .map(name => [fileOffset(name), name.slice("level_data_".length)]));
  const levelName = level => {
    const low = romBytes[fileOffset("_level_list_lo") + level];
    const high = romBytes[fileOffset("_level_list_hi") + level];
    const bank = romBytes[fileOffset("_level_list_bank") + level];
    return namedLevels.get(16 + bank * 8192 + low + (high - 0xa0) * 256);
  };
  const address = name => {
    const value = symbols.get(`_${name}`);
    if (!Number.isInteger(value) || value < 0 || value >= 2048) throw new Error(`missing RAM symbol: ${name}`);
    return value;
  };
  const names = ["gameState", "level", "player_relx", "player_rely", "scroll_x", "scroll_y",
    "player_vel_y", "player_gravity", "player_mini", "gamemode", "dual", "cube_data", "DEBUG_MODE",
    "invincible_counter", "pauseStatus", "wrap_mode", "coins", "speed", "scroll_y_subpx",
    "currplayer", "currplayer_relx", "currplayer_rely", "skipProcessingCubeRotationLogic"];
  const locations = Object.fromEntries(names.map(name => [name, address(name)]));
  const m = await machine(rom, runtime), powerOn = m.core.capture();
  const targetLevels = new Map();
  const observe = () => {
    const r = m.ram(), v = new DataView(r.buffer, r.byteOffset), a = locations;
    return { level: r[a.level], state: r[a.gameState],
      x: v.getInt32(a.scroll_x, true) + r[a.player_relx + 1],
      y: v.getUint16(a.scroll_y, true) + r[a.player_rely + 1], camera: v.getUint16(a.scroll_y, true),
      vy: v.getInt16(a.player_vel_y, true), mode: r[a.gamemode], gravity: r[a.player_gravity],
      mini: r[a.player_mini], dual: r[a.dual],
      dead: r[a.player_relx + 1] > 0x20 && !!((r[a.cube_data] | r[a.cube_data + 1]) & 1),
      debug: r[a.DEBUG_MODE], invincible: r[a.invincible_counter], paused: r[a.pauseStatus],
      wrap: r[a.wrap_mode], coins: r[a.coins] };
  };
  const memory = () => {
    const r = m.ram(), result = r.slice(), v = new DataView(r.buffer, r.byteOffset), a = locations;
    for (const [name, target, count] of [["gameState", 0x498, 1], ["level", 0x47b, 1],
      ["player_relx", 0x43d, 4], ["player_rely", 0x441, 4], ["player_vel_y", 0x449, 4],
      ["player_gravity", 0x44d, 2], ["player_mini", 0x44f, 2], ["scroll_x", 0x4a2, 4],
      ["gamemode", 0x7a, 1], ["dual", 0x96, 1], ["cube_data", 0x7b, 2], ["speed", 0x45e, 1],
      ["scroll_y_subpx", 0x4a8, 1], ["DEBUG_MODE", 0x595, 1], ["invincible_counter", 0x4a1, 1],
      ["pauseStatus", 0x5b5, 1]]) result.set(r.slice(a[name], a[name] + count), target);
    for (const [name, target, count] of [["currplayer", 0x462, 1], ["currplayer_relx", 0x68, 2],
      ["currplayer_rely", 0x6a, 2], ["skipProcessingCubeRotationLogic", 0x5aa, 1]])
      result.set(r.slice(a[name], a[name] + count), target);
    result[0x47b] = targetLevels.get(r[a.level]) ?? r[a.level];
    const camera = v.getUint16(a.scroll_y, true);
    result[0x4a6] = camera % 240; result[0x4a7] = Math.floor(camera / 240);
    return result;
  };
  const adapter = { ...m.core, memory,
    reset() { m.core.restore(powerOn); m.core.step(0, 158); },
    ramView: () => new Proxy({}, { set(_target, property, value) {
      if (Number(property) !== 0x47b) throw new Error("symbol adapter permits only level-select setup writes");
      m.ram()[locations.level] = [...targetLevels].find(([, target]) => target === value)?.[0] ?? value; return true;
    } }),
    frames(buttons, frames) {
      const result = new Uint8Array(frames * 2048);
      for (let i = 0; i < frames; i++) { m.core.step(buttons, 1); result.set(memory(), i * 2048); }
      return result;
    } };
  return { ...m, symbols: locations, debug_sha256: hash(bytes), observe, adapter, levelName,
    mapTargetLevel(actual, target) {
      if (![actual, target].every(value => Number.isInteger(value) && value >= 0 && value < 256))
        throw new Error("level aliases must be byte indices");
      targetLevels.set(actual, target);
    },
    players() {
      const primary = { ...observe(), player: 0 }, r = m.ram(), a = locations;
      if (!primary.dual) return [primary];
      const v = new DataView(r.buffer, r.byteOffset);
      return [primary, { ...primary, player: 1,
        x: v.getInt32(a.scroll_x, true) + r[a.player_relx + 3], y: primary.camera + r[a.player_rely + 3],
        vy: v.getInt16(a.player_vel_y + 2, true), gravity: r[a.player_gravity + 1], mini: r[a.player_mini + 1] }];
    },
    boot(side, level, menuDelay = 0, startupFrames = 180) {
      if (!Number.isSafeInteger(menuDelay) || menuDelay < 0 || menuDelay > 3600)
        throw new Error("menu delay must be between 0 and 3600 frames");
      if (!Number.isSafeInteger(startupFrames) || startupFrames < 1 || startupFrames > 3600)
        throw new Error("startup wait must be between 1 and 3600 frames");
      m.core.restore(powerOn);
      const menu = [[0, startupFrames], [1, 1], [0, 936]];
      if (side !== "main" || level >= 22) menu.push([128, 1], [0, 20]);
      menu.push([1, 1], [0, 90]);
      for (const [buttons, frames] of menu) m.core.step(buttons, frames);
      if (m.ram()[locations.gameState] !== 6) throw new Error("symbol-based boot did not reach level select");
      m.ram()[locations.level] = level;
      if (menuDelay) m.core.step(0, menuDelay);
      m.core.step(1, 1); m.core.step(0, 40);
      const o = observe();
      if (o.state !== 2 || o.level !== level || o.dead || o.debug) throw new Error("symbol-based boot did not reach gameplay");
    } };
}
