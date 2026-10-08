// SPDX-License-Identifier: AGPL-3.0-or-later
import init, { Explorer } from "../rust/pkg/nova_browser.js";
import { createEngine } from "./emulator.js";
let explorer,
  paused = false,
  initialized = false,
  generation = 0;
function error(e) {
  paused = true;
  postMessage({ type: "error", message: String(e?.message || e) });
}
async function tick(gen) {
  if (gen !== generation || paused || !initialized) return;
  try {
    const batch = JSON.parse(explorer.advance(2));
    postMessage({ type: "batch", ...batch });
    if (batch.stopped) {
      paused = true;
      postMessage({ type: "limit" });
    } else setTimeout(() => tick(gen), 20);
  } catch (e) {
    error(e);
  }
}
onmessage = async ({ data }) => {
  try {
    if (data.type === "init") {
      generation++;
      initialized = false;
      globalThis.harmonyEngine = await createEngine(data.base);
      globalThis.harmonyEngine.boot();
      await init();
      explorer = new Explorer(data.seed ?? 1);
      initialized = true;
      paused = false;
      postMessage({ type: "ready", state: JSON.parse(explorer.state(0)) });
      tick(generation);
    } else if (data.type === "pause") {
      paused = true;
      postMessage({ type: "paused" });
    } else if (data.type === "resume" && initialized) {
      if (paused) {
        paused = false;
        tick(generation);
      }
    } else if (data.type === "states" && initialized) {
      const states = [];
      for (const id of data.ids.slice(0, 12)) {
        try {
          const state = JSON.parse(explorer.state(id));
          state.snapshot = explorer.snapshot(id);
          states.push(state);
        } catch {}
      }
      postMessage({ type: "states", request: data.request, states });
    }
  } catch (e) {
    error(e);
  }
};
