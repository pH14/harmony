// SPDX-License-Identifier: AGPL-3.0-or-later
import { ReplayTimeline } from "./replay.js";
export class RoutePreviews {
  constructor(create, maxImages = 16, checkpointBytes = 2 * 1048576) {
    this.create = create;
    this.maxImages = maxImages;
    this.timeline = new ReplayTimeline(checkpointBytes);
    this.images = new Map();
    this.epoch = 0;
  }
  cancel() { this.epoch++; }
  clear() {
    this.cancel();
    this.images.clear();
    this.timeline.clear();
  }
  remember(id, pixels) {
    this.images.delete(id);
    this.images.set(id, pixels);
    while (this.images.size > this.maxImages)
      this.images.delete(this.images.keys().next().value);
  }
  async get(state) {
    const epoch = ++this.epoch;
    const level = state.boot_level ?? 0;
    if (this.bootLevel !== level) {
      this.bootLevel = level;
      this.images.clear();
      this.timeline.clear();
    }
    if (this.images.has(state.id)) {
      const pixels = this.images.get(state.id);
      this.remember(state.id, pixels);
      return pixels;
    }
    this.runtime ||= this.create().then((engine) => ({
      engine, level: null, origin: null, pixels: null,
    })).catch((error) => { this.runtime = null; throw error; });
    const runtime = await this.runtime;
    if (epoch !== this.epoch) return null;
    if (runtime.level !== level) {
      runtime.origin = runtime.engine.boot(level);
      runtime.pixels = runtime.engine.pixels();
      runtime.level = level;
    }
    const { engine, origin } = runtime;
    this.timeline.reset(state.actions, state.frames);
    const checkpoint = this.timeline.before(state.frames);
    engine.restore(checkpoint.snapshot || origin);
    let frame = checkpoint.frame;
    while (frame < state.frames) {
      const deadline = performance.now() + 8;
      do {
        const action = this.timeline.actionAt(frame);
        if (!action) throw new Error("Incomplete preview history");
        const n = Math.min(action.frames, state.frames - frame,
          this.timeline.interval - frame % this.timeline.interval);
        engine.run(action.buttons, n, frame + n === state.frames);
        frame += n;
        if (frame % this.timeline.interval === 0)
          this.timeline.put(frame, engine.capture());
      } while (frame < state.frames && performance.now() < deadline);
      if (frame < state.frames) await new Promise((resolve) => setTimeout(resolve, 0));
      if (epoch !== this.epoch) return null;
    }
    const actual = engine.capture();
    if (actual.length !== state.snapshot.length || actual.some((byte, i) => byte !== state.snapshot[i]))
      throw new Error("Preview snapshot mismatch");
    const pixels = state.frames === 0 ? runtime.pixels : engine.pixels();
    this.remember(state.id, pixels);
    return pixels;
  }
}
