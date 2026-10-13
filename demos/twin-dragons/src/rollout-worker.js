// SPDX-License-Identifier: AGPL-3.0-or-later
import { createEngine } from "./emulator.js";
import { runRollout, stepBytes } from "./rollout.js";
let engine;
const parents = new Map();
onmessage = async ({ data }) => {
  try {
    if (data.type === "init") {
      engine = await createEngine(data.base, { rom: data.rom, wasmBinary: data.wasm });
      postMessage({ type: "ready" });
      return;
    }
    if (data.type !== "run") return;
    const size = engine.stateSize, stride = stepBytes(engine);
    const snapshots = new Uint8Array(data.snapshots);
    data.added.forEach((key, i) => parents.set(key, snapshots.slice(i * size, (i + 1) * size)));
    let total = 0;
    for (const job of data.jobs) total += job.actions.length;
    const out = new Uint8Array(total * stride), steps = [], trails = [];
    let offset = 0;
    for (const job of data.jobs) {
      const result = runRollout(engine, parents.get(job.key), job.actions, out, offset, job.record);
      steps.push(result.steps);
      trails.push(result.trail);
      offset += job.actions.length * stride;
    }
    for (const key of data.evicted) parents.delete(key);
    postMessage({ type: "done", chunk: data.chunk, buffer: out.buffer, steps, trails },
      [out.buffer, ...trails.filter(Boolean).map((t) => t.buffer)]);
  } catch (e) {
    postMessage({ type: "error", message: String(e?.message || e) });
  }
};
