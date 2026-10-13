// SPDX-License-Identifier: AGPL-3.0-or-later
export const MOTION_STRIDE = 5;
export const NO_ROOM = 65535;

export const SPRITE = { width: 32, height: 24, originX: 8, originY: 4 };


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

export class Swarm {
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
    const rooms = new Map(), step = Math.max(1, Math.ceil(this.trails.size / maxPoints));
    let count = 0;
    for (const [id, trail] of this.trails) {
      if (count >= maxPoints) break;
      if (trail.branch !== branch || id % step) continue;
      const frame = still ? 0 : Math.max(0, milliseconds - trail.started) * 0.06;
      const point = swarmPoint(trail.samples, frame);
      if (!point) continue;
      if (detail) {
        point.life = still ? 0 : Math.min(1, frame / trail.duration);
        if (!still) {
          point.samples = trail.samples;
          point.frame = frame;
        }
      }
      if (!rooms.has(point.level)) rooms.set(point.level, []);
      rooms.get(point.level).push(point);
      count++;
    }
    return rooms;
  }
}

export function ghosts(point) {
  if (!point.samples) return [];
  const result = [];
  for (const lag of [6, 12]) {
    const ghost = point.frame >= lag && swarmPoint(point.samples, point.frame - lag);
    if (ghost && ghost.level === point.level) result.push(ghost);
  }
  return result;
}
