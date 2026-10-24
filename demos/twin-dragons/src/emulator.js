// SPDX-License-Identifier: AGPL-3.0-or-later
import { loadRust } from "./rust.js";
import { loadQuickNES } from "./cores/quicknes.js";
import { loadSameBoy } from "./cores/sameboy.js";
import { campaignGenesis } from "./campaign.js";
import twinDragons from "./games/twin-dragons.js";
import { FAMIDASH } from "./games/famidash.js";
import tobuTobuGirl from "./games/tobu-tobu-girl.js";
import superMarioBros from "./games/super-mario-bros.js";
import megaMan2 from "./games/mega-man-2.js";
import metroid from "./games/metroid.js";
import donkeyKong from "./games/donkey-kong.js";

const CORES = { quicknes: loadQuickNES, sameboy: loadSameBoy };
export const GAMES = new Map([twinDragons, ...FAMIDASH, tobuTobuGirl, superMarioBros, megaMan2, metroid, donkeyKong].map((game) => [game.id, game]));

export async function sha256Hex(bytes) {
  return [...new Uint8Array(await crypto.subtle.digest("SHA-256", bytes))]
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

export async function createEngine(base, game, inputs = {}) {
  if (!inputs.rom && game.rom.upload) throw new Error(`${game.title} needs a ROM`);
  const rom =
    inputs.rom ||
    new Uint8Array(await (await checkedFetch(new URL(game.rom.file, base))).arrayBuffer());
  if (rom.byteLength !== game.rom.bytes) throw new Error(`Unexpected ${game.title} ROM size`);
  if ((await sha256Hex(rom)) !== game.rom.sha256) throw new Error(`${game.title} ROM checksum mismatch`);
  if (game.campaign) await loadRust();
  const core = await CORES[game.core](base, rom, { ...inputs, cartridgeMemory: !!game.cartridgeMemory });
  const engine = {
    ...core,
    game,
    rom,
    run(buttons, frames, render = false) {
      if (
        !Number.isInteger(frames) || frames < 0 || frames > 200000 ||
        !Number.isInteger(buttons) || buttons < 0 || buttons > 255
      )
        throw new Error("Invalid emulator action");
      core.runRendered(buttons, frames, render);
    },
    memory() {
      const ram = core.ramPointer();
      return core.mod.HEAPU8.slice(ram, ram + game.ramBytes);
    },
    ramView() {
      const ram = core.ramPointer();
      return core.mod.HEAPU8.subarray(ram, ram + game.ramBytes);
    },
    observation() {
      return game.decode(this.memory());
    },
    reset() {
      this.restore(powerOn);
    },
    frames(buttons, count) {
      const ram = core.ramPointer(), out = new Uint8Array(count * game.ramBytes);
      for (let frame = 0; frame < count; frame++) {
        core.step(buttons, 1);
        out.set(core.mod.HEAPU8.subarray(ram, ram + game.ramBytes), frame * game.ramBytes);
      }
      return out;
    },
    boot(level = 0) {
      if (level === 0 && game.campaign) genesis ??= JSON.parse(campaignGenesis(this));
      this.restore(powerOn);
      if (level === 0 && game.campaign) for (const { buttons, frames } of genesis) core.runRendered(buttons, frames, true);
      else game.boot(this, level);
      return this.capture();
    },
  };
  const powerOn = engine.capture();
  let genesis = null;
  return engine;
}
async function checkedFetch(url) {
  const r = await fetch(url);
  if (!r.ok) throw new Error("Could not load " + url.pathname + " (" + r.status + ")");
  return r;
}
