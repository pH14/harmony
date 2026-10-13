// SPDX-License-Identifier: AGPL-3.0-or-later
import { createEngine, GAMES } from "./emulator.js";
let engine;
onmessage = async ({ data }) => {
  try {
    if (data.type === "init") {
      engine = await createEngine(data.base, GAMES.get(data.game), { rom: data.rom });
      const sheet = engine.game.buildSprites(engine);
      postMessage({ type: "sprites", ...sheet }, [sheet.data.buffer]);
    } else if (data.type === "map") {
      const map = engine.game.buildMap(engine, data.level);
      postMessage({ type: "map", level: data.level, width: map.width, height: map.height, data: map.data }, [map.data.buffer]);
    }
  } catch (e) {
    postMessage({ type: "error", message: String(e?.message || e) });
  }
};
