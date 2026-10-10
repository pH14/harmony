// SPDX-License-Identifier: AGPL-3.0-or-later
export const MOTION_STRIDE = 5;
export const NO_ROOM = 65535;

export class RolloutRecorder {
  constructor(engine, position) {
    this.position = position;
    const run = engine.run.bind(engine), restore = engine.restore.bind(engine);
    engine.restore = (bytes) => {
      restore(bytes);
      if (this.recording) {
        this.flush();
        this.frame = 0;
        this.samples = [];
        this.sample();
      }
    };
    engine.run = (buttons, frames, render = false) => {
      if (!this.recording || !this.samples) return run(buttons, frames, render);
      for (let done = 0; done < frames;) {
        const n = Math.min(4, frames - done);
        done += n;
        run(buttons, n, render && done === frames);
        this.frame += n;
        this.sample();
      }
    };
  }
  begin() {
    this.recording = true;
    this.trails = [];
    this.samples = null;
  }
  sample() {
    this.samples.push(this.frame, ...this.position());
  }
  flush() {
    if (this.samples?.length > MOTION_STRIDE)
      this.trails.push(new Uint16Array(this.samples));
  }
  finish() {
    this.flush();
    const trails = this.trails;
    this.recording = false;
    this.samples = this.trails = null;
    return trails;
  }
}

export function motionPoint(engine, owners) {
  const ram = engine.mod.HEAPU8, p = engine.mod._nova_ram(), s = engine.mod._nova_sram();
  const level = ram[p + 0xa7], selected = ram[p + 0xa8];
  const valid = ram[p + 0x4b] && !ram[p + 0xa9] && ram[p + 0x39e] === 9 &&
    owners.get(level) === selected && owners.get(ram[s + 0x1259]) === selected;
  const pose = !ram[p + 0x46] ? 5 : (ram[p + 0x29] || ram[p + 0x2a]) ?
    1 + ((ram[p + 0x10] >> 2) & 3) : 0;
  return [valid ? level : NO_ROOM,
    ram[p + 0x26] * 16 + (ram[p + 0x25] >> 4),
    ram[p + 0x27] * 16 + (ram[p + 0x28] >> 4),
    pose * 2 + (ram[p + 0x43] ? 1 : 0)];
}

export function swarmPoint(samples, frame) {
  let lo = 0, hi = samples.length / MOTION_STRIDE - 1;
  while (lo < hi) {
    const mid = Math.ceil((lo + hi) / 2);
    if (samples[mid * MOTION_STRIDE] <= frame) lo = mid;
    else hi = mid - 1;
  }
  const a = lo * MOTION_STRIDE, b = Math.min(a + MOTION_STRIDE, samples.length - MOTION_STRIDE);
  if (samples[a + 1] === NO_ROOM) return null;
  const mix = samples[a + 1] === samples[b + 1] && samples[b] > samples[a] ?
    Math.max(0, Math.min(1, (frame - samples[a]) / (samples[b] - samples[a]))) : 0;
  return { level: samples[a + 1], x: samples[a + 2] + (samples[b + 2] - samples[a + 2]) * mix,
    y: samples[a + 3] + (samples[b + 3] - samples[a + 3]) * mix, pose: samples[a + 4] };
}

export class NovaSwarm {
  constructor(maxBytes, maxTrails) {
    this.maxBytes = maxBytes;
    this.maxTrails = maxTrails;
    this.clear();
  }
  clear() {
    this.trails = new Map();
    this.bytes = 0;
    this.serial = 0;
  }
  remove(branch) {
    for (const [id, trail] of this.trails) if (trail.branch === branch) {
      this.bytes -= trail.samples.byteLength;
      this.trails.delete(id);
    }
  }
  prune(branch, milliseconds) {
    for (const [id, trail] of this.trails) {
      if (trail.branch !== branch || milliseconds - trail.started < trail.duration / 0.06) continue;
      this.bytes -= trail.samples.byteLength;
      this.trails.delete(id);
    }
  }
  add(branch, trails, milliseconds) {
    this.prune(branch, milliseconds);
    for (const samples of trails) {
      if (!(samples instanceof Uint16Array) || samples.length < MOTION_STRIDE * 2 ||
          samples.length % MOTION_STRIDE || samples.byteLength > this.maxBytes) continue;
      const duration = samples.at(-MOTION_STRIDE);
      if (!duration) continue;
      const id = this.serial++;
      this.trails.set(id, { branch, samples, duration, started: milliseconds });
      this.bytes += samples.byteLength;
      while (this.bytes > this.maxBytes || this.trails.size > this.maxTrails) {
        const oldest = this.trails.keys().next().value;
        this.bytes -= this.trails.get(oldest).samples.byteLength;
        this.trails.delete(oldest);
      }
    }
  }
  frame(branch, milliseconds, still = false, maxPoints = this.maxTrails, detail = false) {
    this.prune(branch, milliseconds);
    const rooms = new Map();
    let count = 0;
    for (const trail of this.trails.values()) {
      if (count >= maxPoints) break;
      if (trail.branch !== branch) continue;
      const frame = still ? 0 : Math.max(0, milliseconds - trail.started) * 0.06;
      const point = swarmPoint(trail.samples, frame);
      if (!point) continue;
      if (detail) {
        point.life = still ? 0 : Math.min(1, frame / trail.duration);
        point.ghosts = still ? [] : [6, 12].map((lag) => frame >= lag && swarmPoint(trail.samples, frame - lag))
          .filter((ghost) => ghost && ghost.level === point.level);
      }
      if (!rooms.has(point.level)) rooms.set(point.level, []);
      rooms.get(point.level).push(point);
      count++;
    }
    return rooms;
  }
}
