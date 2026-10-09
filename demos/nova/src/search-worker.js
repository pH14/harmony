// SPDX-License-Identifier: AGPL-3.0-or-later
import init, { Explorer } from "../rust/pkg/nova_browser.js";
import { createEngine } from "./emulator.js";
import { SearchLoop } from "./loop.js";
import { validateTape } from "./heat.js";
import { branchInfo } from "./branch.js";
import { snapshotHash } from "./media.js";
import { RolloutRecorder, motionPoint } from "./swarm.js";
let initialized = false,
  wasm,
  budget,
  engine,
  recorder,
  genesis,
  active = 0,
  generation = 0,
  busy = false;
const searches = new Map();
const externalId = (branch, id) => (branch === 0 ? id : `${branch}:${id}`);
function usedSnapshots() {
  return [...searches.values()].reduce(
    (sum, s) => sum + s.explorer.snapshot_bytes(),
    0,
  );
}
function memoryLimit() {
  return (
    usedSnapshots() >= budget.snapshotsMiB * 1048576 ||
    wasm.memory.buffer.byteLength + engine.mod.HEAPU8.byteLength >=
      budget.searchMiB * 1048576
  );
}
function stateFor(branch, id) {
  const search = searches.get(branch);
  if (!search) throw new Error("Unknown search branch");
  const state = JSON.parse(search.explorer.state(id));
  state.id = externalId(branch, id);
  state.snapshot = search.explorer.snapshot(id);
  state.branch = search.branch;
  return state;
}
function choices() {
  return [...searches].map(([id, s]) => ({
    id,
    label: id === 0 ? "Original search" : `Branch ${id}`,
    branch: s.branch,
  }));
}
function ready(paused = false) {
  postMessage({
    type: "ready",
    active,
    searches: choices(),
    state: stateFor(active, 0),
    paused,
  });
}
function updateMemory(batch) {
  batch.snapshot_bytes = usedSnapshots();
  batch.states = [...searches.values()].reduce(
    (sum, s) => sum + s.explorer.state_count(),
    0,
  );
  batch.wasm_bytes =
    wasm.memory.buffer.byteLength + engine.mod.HEAPU8.byteLength;
  batch.stopped ||= memoryLimit();
}
const loop = new SearchLoop(() => {
  if (!initialized || busy) return false;
  try {
    const search = searches.get(active);
    recorder.begin();
    let batch, motion;
    try {
      batch = JSON.parse(search.explorer.advance(2));
    } finally {
      motion = recorder.finish();
    }
    batch.points = batch.points.map((p) => ({
      ...p,
      retained: p.retained === null ? null : externalId(active, p.retained),
    }));
    updateMemory(batch);
    search.stats = batch;
    postMessage({ type: "batch", active, ...batch, motion }, motion.map((trail) => trail.buffer));
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
      engine = await createEngine(data.base);
      budget = data.budget;
      if (gen !== generation) return;
      globalThis.harmonyEngine = engine;
      genesis = engine.boot();
      const catalog = await (await fetch(new URL("maps.json", data.base))).json();
      if (gen !== generation) return;
      const owners = new Map(catalog.levels.flatMap((level) => level.rooms.map((room) => [room, level.id])));
      recorder = new RolloutRecorder(engine, () => motionPoint(engine, owners));
      wasm = await init();
      if (gen !== generation) return;
      const explorer = new Explorer(data.seed ?? 1);
      explorer.set_snapshot_budget(budget.snapshotsMiB * 1048576);
      searches.set(0, { explorer, branch: null });
      initialized = true;
      ready();
      loop.resume();
    } else if (data.type === "pause") {
      loop.pause();
      postMessage({ type: "paused" });
    } else if (data.type === "resume" && initialized && !busy) loop.resume();
    else if (data.type === "switch" && initialized && !busy) {
      if (!searches.has(data.active)) throw new Error("Unknown search branch");
      loop.pause();
      active = data.active;
      const stats = searches.get(active).stats;
      if (stats) updateMemory(stats);
      const stopped = !!stats?.stopped || memoryLimit();
      ready(stopped);
      if (stats) postMessage({ type: "batch", active, ...stats, points: [] });
      if (stopped) postMessage({ type: "limit", won: !!stats?.won });
      else loop.resume();
    } else if (data.type === "fork" && initialized && !busy) {
      loop.pause();
      busy = true;
      try {
        if (searches.size >= 8)
          throw new Error(
            "Eight searches retained. Restart Search to release them before creating another.",
          );
        const tape = data.tape;
        const frames = validateTape(tape);
        if (frames >= 200000 || tape.actions.length >= 10000)
          throw new Error(
            "This history has reached its input limit. Choose an earlier frame.",
          );
        if (
          memoryLimit() ||
          usedSnapshots() + genesis.length + 64 > budget.snapshotsMiB * 1048576
        )
          throw new Error(
            "Search memory limit reached. Save this history and Restart Search to release the retained searches.",
          );
        engine.restore(genesis);
        let chunk = 0;
        for (const a of tape.actions) {
          engine.run(a.buttons, a.frames);
          chunk += a.frames;
          if (chunk >= 600) {
            chunk = 0;
            await new Promise((r) => setTimeout(r, 0));
          }
        }
        if ((await snapshotHash(engine.capture())) !== tape.endpoint_sha256)
          throw new Error("Branch history does not reproduce its snapshot");
        const explorer = Explorer.from_history(
          data.seed,
          JSON.stringify(tape.actions),
        );
        explorer.set_snapshot_budget(budget.snapshotsMiB * 1048576);
        const id = searches.size;
        searches.set(id, { explorer, branch: branchInfo(tape.branch, frames) });
        active = id;
        busy = false;
        ready();
        loop.resume();
      } catch (e) {
        busy = false;
        postMessage({ type: "branch-error", message: String(e?.message || e) });
      }
    } else if (data.type === "states" && initialized) {
      const states = [];
      for (const key of data.ids.slice(0, 12)) {
        try {
          const parts = String(key).split(":"),
            branch = parts.length === 1 ? 0 : Number(parts[0]),
            id = Number(parts.at(-1));
          states.push(stateFor(branch, id));
        } catch {}
      }
      postMessage({ type: "states", request: data.request, states });
    }
  } catch (e) {
    error(e);
  }
};
