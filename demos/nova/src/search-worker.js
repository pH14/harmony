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
  bootLevel = 0,
  active = 0,
  nextBranch = 1,
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
  state.boot_level = bootLevel;
  return state;
}
function choices() {
  return [...searches].map(([id, s]) => ({
    id,
    origin: s.origin ?? null,
    branch: s.branch,
    parent: s.parent ?? null,
  }));
}
function ready(paused = false, deleted) {
  postMessage({
    type: "ready",
    deleted,
    active,
    searches: choices(),
    state: stateFor(active, 0),
    paused,
  });
}
function updateMemory(batch, search) {
  batch.snapshot_bytes = usedSnapshots();
  batch.wasm_bytes =
    wasm.memory.buffer.byteLength + engine.mod.HEAPU8.byteLength;
  batch.stopped = !!search.localStopped || memoryLimit();
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
    search.localStopped = batch.stopped;
    updateMemory(batch, search);
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
      bootLevel = data.boot_level ?? 0;
      genesis = engine.boot(bootLevel);
      const catalog = await (await fetch(new URL("maps.json", data.base))).json();
      if (gen !== generation) return;
      const owners = new Map(catalog.levels.flatMap((level) => level.rooms.map((room) => [room, level.id])));
      recorder = new RolloutRecorder(engine, () => motionPoint(engine, owners));
      wasm = await init();
      if (gen !== generation) return;
      const explorer = bootLevel === 0 ? new Explorer(data.seed ?? 1) : Explorer.from_history(data.seed ?? 1, "[]");
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
      if (stats) updateMemory(stats, searches.get(active));
      const stopped = !!stats?.stopped || memoryLimit();
      ready(stopped);
      if (stats) postMessage({ type: "batch", active, ...stats, points: [] });
      if (stopped) postMessage({ type: "limit", won: !!stats?.won });
      else loop.resume();
    } else if (data.type === "delete" && initialized && !busy) {
      const removed = searches.get(data.id);
      if (!data.id || !removed) throw new Error("Unknown deletable search branch");
      loop.pause();
      const wasActive = active === data.id;
      if (wasActive) active = removed.parent ?? 0;
      for (const search of searches.values()) if (search.parent === data.id) search.parent = removed.parent ?? 0;
      searches.delete(data.id);
      removed.explorer.free();
      const search = searches.get(active), stats = search.stats;
      if (stats) updateMemory(stats, search);
      const stopped = !!stats?.stopped || memoryLimit();
      const paused = stopped || (!wasActive && !!data.paused);
      ready(paused, data.id);
      if (stats) postMessage({ type: "batch", active, ...stats, points: [] });
      if (stopped) postMessage({ type: "limit", won: !!stats?.won });
      else if (!paused) loop.resume();
    } else if (data.type === "fork" && initialized && !busy) {
      loop.pause();
      busy = true;
      try {
        if (searches.size >= 8)
          throw new Error(
            "Eight searches retained. Delete a branch or Restart Search before creating another.",
          );
        const tape = data.tape;
        const frames = validateTape(tape);
        if ((tape.boot_level ?? 0) !== bootLevel) throw new Error("History starts in a different level");
        if (frames >= 200000 || tape.actions.length >= 10000)
          throw new Error(
            "This history has reached its input limit. Choose an earlier frame.",
          );
        if (
          memoryLimit() ||
          usedSnapshots() + genesis.length + 64 > budget.snapshotsMiB * 1048576
        )
          throw new Error(
            "Search memory limit reached. Delete a branch or Restart Search to release retained snapshots.",
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
        const id = nextBranch++;
        searches.set(id, { explorer, branch: branchInfo(tape.branch, frames), parent: active,
          origin: { frames, level: JSON.parse(explorer.state(0)).observation.level } });
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
