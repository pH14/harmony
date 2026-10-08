// SPDX-License-Identifier: AGPL-3.0-or-later
import init, { Explorer } from "../rust/pkg/nova_browser.js";
import { createEngine } from "./emulator.js";
import { SearchLoop } from "./loop.js";
let explorer,
  initialized = false,
  generation = 0;
const loop = new SearchLoop(() => {
  if (!initialized) return false;
  try {
    const batch = JSON.parse(explorer.advance(2));
    postMessage({ type: "batch", ...batch });
    if (batch.stopped) postMessage({ type: "limit", won: batch.won });
    return !batch.stopped;
  } catch (e) {
    error(e);
    return false;
  }
});
function error(e) {
  loop.pause();
  postMessage({ type: "error", message: String(e?.message || e) });
}
onmessage = async ({ data }) => {
  try {
    if (data.type === "init") {
      const gen = ++generation;
      loop.pause();
      initialized = false;
      const engine = await createEngine(data.base);
      if (gen !== generation) return;
      globalThis.harmonyEngine = engine;
      engine.boot();
      await init();
      if (gen !== generation) return;
      explorer = new Explorer(data.seed ?? 1);
      initialized = true;
      postMessage({ type: "ready", state: JSON.parse(explorer.state(0)) });
      loop.resume();
    } else if (data.type === "pause") {
      loop.pause();
      postMessage({ type: "paused" });
    } else if (data.type === "resume" && initialized) loop.resume();
    else if (data.type === "states" && initialized) {
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
