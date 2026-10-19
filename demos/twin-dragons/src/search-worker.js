// SPDX-License-Identifier: AGPL-3.0-or-later
import init, { Explorer } from "../rust/pkg/twin_dragons_browser.js";
import { parsePlan, rolloutBytes, stepBytes, ROUND_PATHS, ROUNDS_AHEAD } from "./rollout.js";
import { validateTape } from "./heat.js";
import { branchInfo } from "./branch.js";
import { snapshotHash } from "./media.js";
import { campaignSearch, coreIdentity } from "./campaign.js";
const emulator = import("./emulator.js");
let initialized = false,
  wasm,
  budget,
  engine,
  genesis,
  bootLevel = 0,
  identity,
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
  return wasm.memory.buffer.byteLength + engine.mod.HEAPU8.byteLength >= budget.searchMiB * 1048576;
}
function shareSnapshotBudget(search) {
  if (search.campaign) return;
  const others = usedSnapshots() - search.explorer.snapshot_bytes();
  search.explorer.set_snapshot_budget(Math.max(0, budget.snapshotsMiB * 1048576 - others));
}
function stateFor(branch, id) {
  const search = searches.get(branch);
  if (!search) throw new Error("Unknown search branch");
  const state = JSON.parse(search.explorer.state(id));
  state.id = externalId(branch, id);
  state.digest = search.explorer.digest(id);
  try {
    state.snapshot = search.explorer.snapshot(id);
  } catch {
    state.snapshot = null;
  }
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
const CHUNK_PATHS = 1,
  CHUNKS_AHEAD = 2,
  PARENT_CACHE = 64,
  FLUSH_MS = 16;
const pool = [],
  chunks = new Map();
let running = false,
  nextChunk = 0,
  flushTimer = null,
  pace = 0,
  allowance = 0,
  refilled = 0,
  paceTimer = null;
const parentKey = (search, id) => search.key * 4294967296 + id;
function planJobs(search) {
  if (search.explorer.stopped()) return;
  for (let bytes; (bytes = search.explorer.plan()); ) search.queue.push({ search, bytes });
}
function planRound(search) {
  if (search.explorer.stopped()) return;
  const jobs = parsePlan(search.explorer.plan(ROUND_PATHS));
  const round = { jobs, next: 0 };
  for (const job of jobs) {
    job.search = search;
    job.round = round;
    job.key = parentKey(search, job.parent);
    job.snapshot = search.explorer.snapshot(job.parent);
  }
  search.rounds.push(round);
  search.queue.push(...jobs);
}
function cancel(search) {
  for (const round of search.rounds) for (const job of round.jobs) job.cancelled = true;
  search.rounds = [];
  search.queue = [];
}
function drain(search) {
  while (search.rounds.length) {
    const round = search.rounds[0];
    if (round.next < round.jobs.length) shareSnapshotBudget(search);
    while (round.next < round.jobs.length && round.jobs[round.next].result) {
      const job = round.jobs[round.next++];
      search.explorer.merge(job.result, new Uint32Array(job.transition));
      if (job.trail) {
        if (job.nextLevel) {
          search.finishes.push(search.motion.length);
          search.exits.push(job.nextLevel);
        }
        search.motion.push(job.trail);
      }
      job.result = job.trail = job.transition = job.snapshot = null;
      if (search.explorer.stopped()) {
        cancel(search);
        return;
      }
    }
    if (round.next < round.jobs.length) return;
    search.rounds.shift();
    planRound(search);
  }
}
function sendCampaign(slot, job) {
  const chunk = ++nextChunk;
  chunks.set(chunk, { slot, jobs: [job] });
  slot.chunks++;
  slot.worker.postMessage({ type: "job", chunk, job: job.bytes.buffer }, [job.bytes.buffer]);
  job.bytes = null;
}
function admitCampaign(job, { outcome, note }) {
  const { search } = job;
  if (job.cancelled || !searches.has(search.id) || search.explorer.stopped()) return;
  search.explorer.admit(new Uint8Array(outcome), new Uint8Array(note));
  if (search.explorer.stopped()) cancel(search);
  else planJobs(search);
}
function send(slot, jobs) {
  const added = [], evicted = new Set(), size = engine.stateSize;
  for (const job of jobs) {
    if (slot.cache.has(job.key)) {
      slot.cache.delete(job.key);
      slot.cache.set(job.key, true);
    } else {
      added.push(job);
      evicted.delete(job.key);
      slot.cache.set(job.key, true);
      if (slot.cache.size > PARENT_CACHE) {
        const oldest = slot.cache.keys().next().value;
        slot.cache.delete(oldest);
        evicted.add(oldest);
      }
    }
  }
  const snapshots = new Uint8Array(added.length * size);
  added.forEach((job, i) => snapshots.set(job.snapshot, i * size));
  const chunk = ++nextChunk;
  chunks.set(chunk, { slot, jobs });
  slot.chunks++;
  slot.worker.postMessage({
    type: "run",
    chunk,
    added: added.map((job) => job.key),
    evicted: [...evicted],
    snapshots: snapshots.buffer,
    jobs: jobs.map((job) => ({ key: job.key, actions: job.actions, record: true })),
  }, [snapshots.buffer]);
}
function pump() {
  if (!running || busy) return;
  const search = searches.get(active);
  if (!search) return;
  for (let pass = 0; pass < CHUNKS_AHEAD; pass++)
    for (const slot of pool) {
      if (!search.queue.length) return;
      if (slot.chunks >= CHUNKS_AHEAD || !slot.ready) continue;
      if (!paced()) return;
      if (search.campaign) sendCampaign(slot, search.queue.shift());
      else send(slot, search.queue.splice(0, CHUNK_PATHS));
    }
}
function paced() {
  if (!pace) return true;
  const now = performance.now();
  allowance = Math.min(Math.max(1, pace / 8), allowance + ((now - refilled) * pace) / 1000);
  refilled = now;
  if (allowance >= CHUNK_PATHS) {
    allowance -= CHUNK_PATHS;
    return true;
  }
  if (paceTimer === null)
    paceTimer = setTimeout(() => {
      paceTimer = null;
      pump();
    }, ((CHUNK_PATHS - allowance) * 1000) / pace);
  return false;
}
function finished(data) {
  const { chunk, buffer, steps, trails, transitions, nextLevels } = data;
  const entry = chunks.get(chunk);
  chunks.delete(chunk);
  entry.slot.chunks--;
  if ("outcome" in data) {
    admitCampaign(entry.jobs[0], data);
    pump();
    scheduleFlush();
    return;
  }
  const stride = stepBytes(engine);
  let offset = 0;
  const touched = new Set();
  entry.jobs.forEach((job, i) => {
    const bytes = new Uint8Array(buffer, offset, steps[i] * stride);
    offset += rolloutBytes(engine, job.actions);
    if (job.cancelled || !searches.has(job.search.id)) return;
    job.result = bytes;
    job.trail = trails[i];
    job.transition = transitions[i];
    job.nextLevel = nextLevels[i];
    touched.add(job.search);
  });
  for (const search of touched) drain(search);
  pump();
  scheduleFlush();
}
function scheduleFlush() {
  if (flushTimer === null) flushTimer = setTimeout(flush, FLUSH_MS);
}
function flush() {
  flushTimer = null;
  if (!running || busy) return;
  const search = searches.get(active);
  try {
    const batch = JSON.parse(search.explorer.take_batch());
    if (!search.campaign) {
      Object.assign(batch, { motion: search.motion, finishes: search.finishes, exits: search.exits });
      search.motion = [];
      search.finishes = [];
      search.exits = [];
    }
    batch.motion = batch.motion.map((trail) => Uint16Array.from(trail));
    batch.points = batch.points.map((p) => ({
      ...p,
      retained: p.retained === null ? null : externalId(active, p.retained),
    }));
    search.localStopped = batch.stopped;
    updateMemory(batch, search);
    search.stats = { ...batch, points: [], motion: [], finishes: [], exits: [] };
    batch.workers = pool.length;
    postMessage({ type: "batch", active, ...batch }, batch.motion.map((trail) => trail.buffer));
    if (batch.stopped) {
      running = false;
      postMessage({ type: "limit", won: batch.won });
    }
  } catch (e) {
    error(e);
  }
}
function pause() {
  running = false;
}
function resume() {
  if (running) return;
  running = true;
  refilled = performance.now();
  pump();
  scheduleFlush();
}
function addSearch(id, search) {
  search.id = id;
  search.key = id;
  search.rounds = [];
  search.queue = [];
  search.motion = [];
  search.finishes = [];
  search.exits = [];
  searches.set(id, search);
  if (search.campaign) planJobs(search);
  else for (let r = 0; r < ROUNDS_AHEAD; r++) planRound(search);
}
async function startPool(ports, base) {
  const rom = engine.rom,
    wasm = new Uint8Array(await (await fetch(new URL(`engine/${engine.game.core}.wasm`, base))).arrayBuffer());
  identity = coreIdentity(await (await emulator).sha256Hex(wasm));
  await Promise.all(ports.map((worker) => new Promise((resolve) => {
    const slot = { worker, cache: new Map(), chunks: 0, ready: false };
    worker.onmessage = ({ data }) => {
      try {
        if (data.type === "ready") {
          slot.ready = true;
          resolve();
        } else if (data.type === "done") finished(data);
        else if (data.type === "error") throw new Error(data.message);
      } catch (e) {
        error(e);
      }
    };
    worker.postMessage({ type: "init", base, game: engine.game.id, rom, wasm, identity });
    pool.push(slot);
  })));
}
function createSearch(seed, root, prefix) {
  if (engine.game.campaign)
    return {
      explorer: campaignSearch(engine, identity, { seed, root, prefix, memoryMiB: budget.snapshotsMiB }),
      campaign: true,
    };
  const explorer = root === undefined ? new Explorer(seed) : Explorer.from_history(seed, JSON.stringify(prefix));
  explorer.set_snapshot_budget(budget.snapshotsMiB * 1048576);
  return { explorer, campaign: false };
}
function error(e) {
  pause();
  postMessage({ type: "error", message: String(e?.message || e) });
}
onmessage = async ({ data }) => {
  try {
    if (data.type === "init") {
      const gen = ++generation;
      pause();
      initialized = false;
      const { createEngine, GAMES } = await emulator;
      engine = await createEngine(data.base, GAMES.get(data.game), data.rom ? { rom: data.rom } : {});
      budget = data.budget;
      pace = data.pace ?? 0;
      allowance = 0;
      if (gen !== generation) return;
      globalThis.harmonyEngine = engine;
      bootLevel = data.boot_level ?? 0;
      genesis = engine.boot(bootLevel);
      wasm = await init();
      if (gen !== generation) return;
      await startPool(data.ports, data.base);
      if (gen !== generation) return;
      addSearch(0, { ...createSearch(data.seed ?? 1, bootLevel === 0 ? undefined : genesis, []), branch: null });
      initialized = true;
      ready();
      resume();
    } else if (data.type === "pace") {
      pace = data.pace;
      allowance = 0;
      refilled = performance.now();
      clearTimeout(paceTimer);
      paceTimer = null;
      pump();
    } else if (data.type === "pause") {
      pause();
      postMessage({ type: "paused" });
    } else if (data.type === "resume" && initialized && !busy) resume();
    else if (data.type === "switch" && initialized && !busy) {
      if (!searches.has(data.active)) throw new Error("Unknown search branch");
      pause();
      active = data.active;
      const stats = searches.get(active).stats;
      if (stats) updateMemory(stats, searches.get(active));
      const stopped = !!stats?.stopped || memoryLimit();
      ready(stopped);
      if (stats) postMessage({ type: "batch", active, ...stats, points: [] });
      if (stopped) postMessage({ type: "limit", won: !!stats?.won });
      else resume();
    } else if (data.type === "delete" && initialized && !busy) {
      const removed = searches.get(data.id);
      if (!data.id || !removed) throw new Error("Unknown deletable search branch");
      pause();
      const wasActive = active === data.id;
      if (wasActive) active = removed.parent ?? 0;
      for (const search of searches.values()) if (search.parent === data.id) search.parent = removed.parent ?? 0;
      cancel(removed);
      searches.delete(data.id);
      removed.explorer.free();
      const search = searches.get(active), stats = search.stats;
      if (stats) updateMemory(stats, search);
      const stopped = !!stats?.stopped || memoryLimit();
      const paused = stopped || (!wasActive && !!data.paused);
      ready(paused, data.id);
      if (stats) postMessage({ type: "batch", active, ...stats, points: [] });
      if (stopped) postMessage({ type: "limit", won: !!stats?.won });
      else if (!paused) resume();
    } else if (data.type === "fork" && initialized && !busy) {
      pause();
      busy = true;
      try {
        if (searches.size >= 8)
          throw new Error(
            "Eight searches retained. Delete a branch or Restart Search before creating another.",
          );
        const tape = data.tape;
        const frames = validateTape(tape, engine.game, engine.revision);
        if ((tape.boot_level ?? 0) !== bootLevel) throw new Error("History starts in a different level");
        if (frames >= 200000 || tape.actions.length >= 10000)
          throw new Error(
            "This history has reached its input limit. Choose an earlier frame.",
          );
        if (memoryLimit())
          throw new Error(
            "Search memory limit reached. Delete a branch or Restart Search to release memory.",
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
        const { explorer, campaign } = createSearch(data.seed, engine.capture(), tape.actions);
        const id = nextBranch++;
        addSearch(id, { explorer, campaign, branch: branchInfo(tape.branch, frames), parent: active,
          origin: { frames, level: JSON.parse(explorer.state(0)).observation.level } });
        active = id;
        busy = false;
        ready();
        resume();
      } catch (e) {
        busy = false;
        postMessage({ type: "branch-error", message: String(e?.message || e) });
      }
    } else if (data.type === "states" && initialized) {
      const states = [], missing = [];
      for (const key of data.ids.slice(0, 12)) {
        try {
          const parts = String(key).split(":"),
            branch = parts.length === 1 ? 0 : Number(parts[0]),
            id = Number(parts.at(-1));
          states.push(stateFor(branch, id));
        } catch {
          missing.push(key);
        }
      }
      postMessage({ type: "states", request: data.request, states, missing });
    }
  } catch (e) {
    error(e);
  }
};
