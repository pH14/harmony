// SPDX-License-Identifier: AGPL-3.0-or-later
export function prefixAt(actions, frame) {
  const prefix = [];
  for (const a of actions) {
    if (frame <= 0) break;
    const frames = Math.min(a.frames, frame);
    prefix.push({ buttons: a.buttons, frames });
    frame -= frames;
  }
  return prefix;
}
export function appendInput(actions, buttons, frames) {
  while (frames > 0) {
    const last = actions.at(-1);
    if (last?.buttons === buttons && last.frames < 120) {
      const n = Math.min(frames, 120 - last.frames);
      last.frames += n;
      frames -= n;
    } else {
      if (actions.length >= 10000) return false;
      const n = Math.min(frames, 120);
      actions.push({ buttons, frames: n });
      frames -= n;
    }
  }
  return true;
}
export class Controller {
  constructor() {
    this.held = new Map();
  }
  press(token, bit) {
    if (!this.held.has(token)) this.held.set(token, bit);
  }
  release(token) {
    this.held.delete(token);
  }
  clear() {
    this.held.clear();
  }
  buttons() {
    let value = 0;
    for (const bit of this.held.values()) {
      if (bit & 48) value &= ~48;
      if (bit & 192) value &= ~192;
      value |= bit;
    }
    return value;
  }
}
export const keyButtons = {
  ArrowUp: 16,
  KeyW: 16,
  ArrowDown: 32,
  KeyS: 32,
  ArrowLeft: 64,
  KeyA: 64,
  ArrowRight: 128,
  KeyD: 128,
  KeyZ: 1,
  Space: 1,
  KeyX: 2,
};
export function trailSegments(points) {
  const segments = [];
  let segment;
  for (const point of points) {
    if (point.gap) {
      segment = null;
      continue;
    }
    const last = segment?.at(-1);
    if (
      !last ||
      last.level !== point.level ||
      Math.hypot(last.x - point.x, last.y - point.y) >
        Math.max(128, 8 * (point.frame - last.frame))
    ) {
      segment = [];
      segments.push(segment);
    }
    segment.push(point);
  }
  return segments;
}

export function branchInfo(raw, frames) {
  if (!raw || typeof raw !== "object") return null;
  const parent = raw.parent_state;
  if (
    !(
      (typeof parent === "string" && parent.length <= 64) ||
      (Number.isSafeInteger(parent) && parent >= 0)
    ) ||
    !Number.isInteger(raw.parent_frame) ||
    raw.parent_frame < 0 ||
    raw.parent_frame > frames
  )
    return null;
  const result = { parent_state: parent, parent_frame: raw.parent_frame };
  const manual = raw.manual;
  if (
    manual &&
    Number.isInteger(manual.from) &&
    Number.isInteger(manual.to) &&
    manual.from >= 0 &&
    manual.from <= frames &&
    manual.to >= manual.from
  )
    result.manual = { from: manual.from, to: Math.min(frames, manual.to) };
  return result;
}
