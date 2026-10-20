// SPDX-License-Identifier: AGPL-3.0-or-later
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { loadQuickNES } from "../../src/cores/quicknes.js";

export const hash = (bytes) => createHash("sha256").update(bytes).digest("hex");
export const patterns = [[[0, 8]], [[1, 8]], [[1, 1], [0, 7]], [[1, 4], [0, 4]], [[0, 4], [1, 4]], [[0, 6], [1, 2]]];

export function routeTape(route) {
  if (typeof route === "string") {
    if (!/^[0-5]+$/.test(route)) throw new Error("assisted route: not a normal-play control");
    return [...route].flatMap((code) => patterns[Number(code)].flatMap(([buttons, frames]) => Array(frames).fill(buttons)));
  }
  if (!route || !Array.isArray(route.inputs)) throw new Error("missing route");
  if (route.inputs.some(([, , assisted]) => assisted))
    throw new Error("assisted route: not a normal-play control");
  return expand(route.inputs.map(([buttons, frames]) => ({ buttons, frames })));
}

export function expand(actions) {
  if (!Array.isArray(actions)) throw new Error("invalid actions");
  return actions.flatMap(({ buttons, frames }) => {
    if (![0, 1].includes(buttons) || !Number.isInteger(frames) || frames < 1 || frames > 200000)
      throw new Error("actions must contain only A/release and bounded frame counts");
    return Array(frames).fill(buttons);
  });
}

export function compress(tape) {
  const actions = [];
  for (const buttons of tape)
    if (actions.at(-1)?.buttons === buttons && actions.at(-1).frames < 40) actions.at(-1).frames++;
    else actions.push({ buttons, frames: 1 });
  return actions;
}

export async function machine(romPath, runtimePath) {
  const base = pathToFileURL(resolve(runtimePath) + "/"), rom = await readFile(romPath),
    wasm = await readFile(new URL("engine/quicknes.wasm", base)),
    core = await loadQuickNES(base, rom, { wasmBinary: wasm }), powerOn = core.capture(),
    ram = () => core.mod.HEAPU8.subarray(core.mod._nes_ram(), core.mod._nes_ram() + 2048);
  return {
    core, ram, rom_sha256: hash(rom), core_sha256: hash(wasm),
    boot(side, level, menuDelay = 0, startupFrames = 22) {
      if (!Number.isSafeInteger(menuDelay) || menuDelay < 0 || menuDelay > 3600)
        throw new Error("menu delay must be between 0 and 3600 frames");
      if (!Number.isSafeInteger(startupFrames) || startupFrames < 1 || startupFrames > 3600)
        throw new Error("startup wait must be between 1 and 3600 frames");
      core.restore(powerOn);
      const menu = [[0, startupFrames], [1, 1], [0, 936]];
      if (side !== "main" || level >= 22) menu.push([128, 1], [0, 20]);
      menu.push([1, 1], [0, 90]);
      for (const [buttons, frames] of menu) core.step(buttons, frames);
      if (ram()[0x498] !== 6) throw new Error("setup did not reach level select");
      ram()[0x47b] = level;
      if (menuDelay) core.step(0, menuDelay);
      core.step(1, 1);
      core.step(0, 40);
      const o = this.observe();
      if (o.state !== 2 || o.level !== level || o.dead || o.debug) throw new Error("setup did not reach gameplay");
    },
    observe() {
      const r = ram(), view = new DataView(r.buffer, r.byteOffset),
        camera = r[0x4a7] * 240 + r[0x4a6];
      return { level: r[0x47b], state: r[0x498],
        x: view.getInt32(0x4a2, true) + r[0x43e], y: camera + r[0x442], camera,
        vy: view.getInt16(0x449, true), mode: r[0x7a], gravity: r[0x44d],
        mini: r[0x44f], dual: r[0x96],
        dead: r[0x43e] > 0x20 && !!((r[0x7b] | r[0x7c]) & 1),
        debug: r[0x595], invincible: r[0x4a1], paused: r[0x5b5] };
    },
  };
}

export function tileAt(layout, x, y) {
  const column = Math.floor(x / 16), row = Math.floor((y - layout.top) / 16);
  if (column < 0 || column >= layout.width || row < 0 || row >= layout.height) return null;
  return { column, row, id: layout.tiles[row * layout.width + column] };
}

export function routeFloor(bands, x) {
  let floor = -Infinity;
  for (let band = Math.floor(x / 64) - 1; band <= Math.floor(x / 64) + 1; band++)
    if (bands[band] !== undefined) floor = Math.max(floor, bands[band]);
  return floor;
}

export function suspect(o, layout, metatiles, bands) {
  if (o.dead || o.state !== 2 || o.debug || o.invincible || o.paused || o.dual) return null;
  const floor = routeFloor(bands, o.x), tile = tileAt(layout, o.x + 8, o.y + 8);
  if (!Number.isFinite(floor) || o.y <= floor + 32 || !tile) return null;
  const definition = metatiles[tile.id];
  if (definition.collision !== "COL_NONE" || !["X_NO_OUTLINE", "BLACK_NONE"].includes(definition.name)) return null;
  return { kind: "below-route-decoration", ...tile, tile_name: definition.name, route_floor: floor };
}

export async function replay(m, side, level, tape, menuDelay = 0, startupFrames) {
  m.boot(side, level, menuDelay, startupFrames);
  const trace = [];
  for (const buttons of tape) {
    m.core.step(buttons, 1);
    const o = m.observe();
    if (o.debug || o.paused) throw new Error("debug/paused gameplay is not a witness");
    trace.push(o);
    if (o.dead || o.state !== 2) break;
  }
  return trace;
}
