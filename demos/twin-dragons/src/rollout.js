// SPDX-License-Identifier: AGPL-3.0-or-later
import { MOTION_STRIDE, motionPoint } from "./swarm.js";

export const RAM_BYTES = 2048;
export const ROUND_PATHS = 16;
export const ROUNDS_AHEAD = 4;

export function parsePlan(plan) {
  const jobs = [];
  for (let i = 0; i < plan.length;) {
    const parent = plan[i], count = plan[i + 1];
    jobs.push({ parent, actions: plan.slice(i + 2, i + 2 + count) });
    i += 2 + count;
  }
  return jobs;
}

export function stepBytes(engine) {
  return RAM_BYTES + engine.stateSize;
}

export function runRollout(engine, snapshot, actions, out, offset, record) {
  const { mod } = engine, stride = stepBytes(engine), ram = mod._nes_ram();
  engine.restore(snapshot);
  const samples = record ? [0, ...motionPoint(engine)] : null;
  let frame = 0, steps = 0, ended = false;
  for (const packed of actions) {
    const buttons = packed >> 8, frames = packed & 255;
    if (samples)
      for (let done = 0; done < frames;) {
        const n = Math.min(4, frames - done);
        mod._nes_run(buttons, n, 0);
        done += n;
        frame += n;
        samples.push(frame, ...motionPoint(engine));
      }
    else mod._nes_run(buttons, frames, 0);
    const at = offset + steps * stride;
    out.set(mod.HEAPU8.subarray(ram, ram + RAM_BYTES), at);
    engine.captureInto(out.subarray(at + RAM_BYTES, at + stride));
    steps++;
    if (out[at + 0x75] === 0 || out[at + 0x9e] === 0) {
      ended = true;
      break;
    }
  }
  const trail = samples && samples.length > MOTION_STRIDE ? new Uint16Array(samples) : null;
  return { steps, trail, points: steps - (ended ? 1 : 0) };
}

export function runPlan(engines, explorer, jobs, record = false) {
  const trails = [];
  jobs.forEach((job, index) => {
    const engine = engines[index % engines.length];
    const out = new Uint8Array(job.actions.length * stepBytes(engine));
    const { steps, trail, points } = runRollout(engine, job.snapshot, job.actions, out, 0, record);
    job.engine = index % engines.length;
    job.points = points;
    if (trail) trails.push(trail);
    explorer.merge(out.subarray(0, steps * stepBytes(engine)));
  });
  return trails;
}

export class RoundSearch {
  constructor(explorer, engines, { record = false, paths = ROUND_PATHS, ahead = ROUNDS_AHEAD } = {}) {
    this.explorer = explorer;
    this.engines = engines;
    this.record = record;
    this.paths = paths;
    this.rounds = [];
    while (this.rounds.length < ahead) this.plan();
  }
  plan() {
    const jobs = parsePlan(this.explorer.plan(this.paths));
    for (const job of jobs) job.snapshot = this.explorer.snapshot(job.parent);
    this.rounds.push(jobs);
  }
  round() {
    const jobs = this.rounds.shift() ?? [];
    const trails = runPlan(this.engines, this.explorer, jobs, this.record);
    this.plan();
    return { batch: JSON.parse(this.explorer.take_batch()), trails, jobs };
  }
}
