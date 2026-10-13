// SPDX-License-Identifier: AGPL-3.0-or-later
import { snapshotDigest } from "./media.js";
import { ReplayTimeline, prefixTrail } from "./replay.js";
export class RoutePreviews {
  constructor(create, maxImages = 16, checkpointBytes = 2 * 1048576, point) {
    this.create = create;
    this.point = point;
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
    this.trailId = null;
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
      this.trailId = null;
    }
    if (this.images.has(state.id) && (!this.point || this.trailId === state.id)) {
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
    const shared = this.timeline.reset(state.actions, state.frames);
    const stride = Math.max(24, Math.ceil(state.frames / 6000));
    this.timeline.trail = prefixTrail(this.timeline.trail, shared, stride);
    this.trailId = null;
    const checkpoint = this.timeline.before(state.frames);
    engine.restore(checkpoint.snapshot || origin);
    let frame = checkpoint.frame;
    const record = () => {
      if (!this.point || this.timeline.trail.at(-1)?.frame >= frame) return;
      const p = this.point(engine, frame);
      if (!p.gap || !this.timeline.trail.at(-1)?.gap) this.timeline.trail.push(p);
    };
    record();
    while (frame < state.frames) {
      const deadline = performance.now() + 8;
      do {
        const action = this.timeline.actionAt(frame);
        if (!action) throw new Error("Incomplete preview history");
        const n = Math.min(action.frames, state.frames - frame,
          this.timeline.interval - frame % this.timeline.interval,
          this.point ? stride - frame % stride : Infinity);
        engine.run(action.buttons, n, frame + n === state.frames);
        frame += n;
        if (frame % stride === 0 || frame === state.frames) record();
        if (frame % this.timeline.interval === 0)
          this.timeline.put(frame, engine.capture());
      } while (frame < state.frames && performance.now() < deadline);
      if (frame < state.frames) await new Promise((resolve) => setTimeout(resolve, 0));
      if (epoch !== this.epoch) return null;
    }
    const actual = engine.capture();
    if (state.snapshot ? actual.length !== state.snapshot.length || actual.some((byte, i) => byte !== state.snapshot[i]) : state.digest && snapshotDigest(actual) !== state.digest)
      throw new Error("Preview snapshot mismatch");
    const pixels = state.frames === 0 ? runtime.pixels : engine.pixels();
    this.remember(state.id, pixels);
    this.trailId = state.id;
    return pixels;
  }
}
