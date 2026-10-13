// SPDX-License-Identifier: AGPL-3.0-or-later
import { createEngine, GAMES } from "./emulator.js";
import { runRollout, rolloutBytes, stepBytes } from "./rollout.js";
let engine;
const parents = new Map();
let channel = self;
onmessage = ({ data }) => {
  channel = data.port;
  channel.onmessage = handle;
};
async function handle({ data }) {
  try {
    if (data.type === "init") {
      engine = await createEngine(data.base, GAMES.get(data.game), { rom: data.rom, wasmBinary: data.wasm });
      channel.postMessage({ type: "ready" });
      return;
    }
    if (data.type !== "run") return;
    const size = engine.stateSize, stride = stepBytes(engine);
    const snapshots = new Uint8Array(data.snapshots);
    data.added.forEach((key, i) => parents.set(key, snapshots.slice(i * size, (i + 1) * size)));
    let total = 0;
    for (const job of data.jobs) total += rolloutBytes(engine, job.actions);
    const out = new Uint8Array(total), steps = [], trails = [], transitions = [];
    let offset = 0;
    for (const job of data.jobs) {
      const result = runRollout(engine, parents.get(job.key), job.actions, out, offset, job.record);
      steps.push(result.steps);
      trails.push(result.trail);
      transitions.push(result.transition);
      offset += rolloutBytes(engine, job.actions);
    }
    for (const key of data.evicted) parents.delete(key);
    channel.postMessage({ type: "done", chunk: data.chunk, buffer: out.buffer, steps, trails, transitions },
      [out.buffer, ...trails.filter(Boolean).map((t) => t.buffer)]);
  } catch (e) {
    channel.postMessage({ type: "error", message: String(e?.message || e) });
  }
}
