// SPDX-License-Identifier: AGPL-3.0-or-later
export class ReplayTimeline {
  constructor(maxBytes = 2 * 1024 * 1024) {
    this.maxBytes = maxBytes;
    this.clear();
  }
  clear() {
    this.checkpoints = new Map();
    this.bytes = 0;
    this.actions = [];
    this.trail = [];
    this.ends = [];
  }
  index(actions, frames) {
    this.actions = actions;
    let end = 0;
    this.ends = actions.map((a) => (end += a.frames));
    this.interval = Math.max(120, Math.ceil(frames / 64 / 24) * 24);
  }
  reset(actions, frames) {
    const shared = sharedFrames(this.actions, actions);
    this.trim(shared);
    this.index(actions, frames);
    return shared;
  }
  actionAt(frame) {
    let lo = 0,
      hi = this.ends.length;
    while (lo < hi) {
      const m = (lo + hi) >>> 1;
      if (this.ends[m] <= frame) lo = m + 1;
      else hi = m;
    }
    const a = this.actions[lo];
    return a ? { buttons: a.buttons, frames: this.ends[lo] - frame } : null;
  }
  put(frame, snapshot) {
    const old = this.checkpoints.get(frame);
    if (old) this.bytes -= old.byteLength;
    this.checkpoints.delete(frame);
    if (snapshot.byteLength > this.maxBytes) return;
    this.checkpoints.set(frame, snapshot);
    this.bytes += snapshot.byteLength;
    while (this.bytes > this.maxBytes) {
      const key = this.checkpoints.keys().next().value;
      this.bytes -= this.checkpoints.get(key).byteLength;
      this.checkpoints.delete(key);
    }
  }
  before(target) {
    let frame = 0,
      snapshot;
    for (const [f, s] of this.checkpoints)
      if (f < target && f > frame) {
        frame = f;
        snapshot = s;
      }
    if (snapshot) {
      this.checkpoints.delete(frame);
      this.checkpoints.set(frame, snapshot);
    }
    return { frame, snapshot };
  }
  trim(frame) {
    for (const [f, s] of this.checkpoints)
      if (f > frame) {
        this.bytes -= s.byteLength;
        this.checkpoints.delete(f);
      }
  }
}
export function sharedFrames(a, b) {
  let i = 0,
    j = 0,
    usedA = 0,
    usedB = 0,
    frames = 0;
  while (i < a.length && j < b.length && a[i].buttons === b[j].buttons) {
    const n = Math.min(a[i].frames - usedA, b[j].frames - usedB);
    frames += n;
    usedA += n;
    usedB += n;
    if (usedA === a[i].frames) {
      i++;
      usedA = 0;
    }
    if (usedB === b[j].frames) {
      j++;
      usedB = 0;
    }
  }
  return frames;
}
export function prefixTrail(points, frames, stride) {
  const prefix = [];
  for (const point of points) {
    if (point.frame > frames) break;
    const last = prefix.at(-1);
    if (!last || point.gap || last.gap || point.frame - last.frame >= stride)
      prefix.push(point);
  }
  return prefix;
}
export function trailPoint(points, frame) {
  let lo = 0,
    hi = points.length;
  while (lo < hi) {
    const m = (lo + hi) >>> 1;
    if (points[m].frame < frame) lo = m + 1;
    else hi = m;
  }
  const b = points[lo],
    a = points[lo - 1];
  if (b?.frame === frame) return b.gap ? null : b;
  if (!a || !b || a.gap || b.gap) return null;
  if (
    a.level !== b.level ||
    Math.hypot(a.x - b.x, a.y - b.y) > Math.max(128, 8 * (b.frame - a.frame))
  )
    return a;
  const t = (frame - a.frame) / (b.frame - a.frame);
  return {
    level: a.level,
    x: a.x + (b.x - a.x) * t,
    y: a.y + (b.y - a.y) * t,
    frame,
  };
}
