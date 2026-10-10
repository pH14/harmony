// SPDX-License-Identifier: AGPL-3.0-or-later
import { test } from "node:test";
import assert from "node:assert/strict";
import { RoutePreviews } from "../src/preview.js";
const snapshot = (value) => new Uint8Array(new Uint32Array([value]).buffer);
function fakeEngine() {
  let value = 0, rendered = 0;
  return {
    boot(level = 0) { value = level * 1000; rendered = value; return snapshot(value); },
    restore(bytes) { value = new DataView(bytes.buffer, bytes.byteOffset).getUint32(0, true); },
    capture() { return snapshot(value); },
    run(buttons, frames, render) { value += (buttons + 1) * frames; if (render) rendered = value; },
    pixels() { return { width: 1, height: 1, data: new Uint8ClampedArray([rendered & 255, rendered >> 8 & 255, 0, 255]) }; },
  };
}
const state = (id, actions) => ({ id, actions,
  frames: actions.reduce((n, a) => n + a.frames, 0),
  snapshot: snapshot(actions.reduce((n, a) => n + (a.buttons + 1) * a.frames, 0)),
});
test("hover previews replay divergent histories exactly and bound their caches", async () => {
  const previews = new RoutePreviews(async () => fakeEngine(), 2, 8);
  const a = state('a', [{ buttons: 1, frames: 120 }, { buttons: 2, frames: 120 }]);
  const b = state('b', [{ buttons: 1, frames: 120 }, { buttons: 3, frames: 120 }]);
  assert.equal((await previews.get(a)).data[0], 88);
  assert.equal((await previews.get(b)).data[0], 208);
  assert.ok(previews.timeline.bytes <= 8);
  await assert.rejects(previews.get({ ...state('wrong', [{ buttons: 0, frames: 4 }]), snapshot: snapshot(99) }), /mismatch/);
  assert.ok(!previews.images.has('wrong'));
  await previews.get(state('c', [{ buttons: 0, frames: 5 }]));
  assert.equal(previews.images.size, 2);
  assert.ok(!previews.images.has('a'));
  const cached = await previews.get(b);
  assert.equal(cached, previews.images.get('b'));
  previews.clear();
  assert.equal(previews.images.size, 0);
  assert.equal(previews.timeline.bytes, 0);
});
test("a newer hover cancels stale loading without interrupting the selected emulator", async () => {
  let resolve;
  const previews = new RoutePreviews(() => new Promise((done) => { resolve = done; }));
  const first = previews.get(state('a', [{ buttons: 0, frames: 2 }]));
  const second = previews.get(state('b', [{ buttons: 1, frames: 3 }]));
  resolve(fakeEngine());
  assert.equal(await first, null);
  assert.equal((await second).data[0], 6);
  assert.deepEqual([...previews.images.keys()], ['b']);
});

test("warping invalidates identically numbered preview states and checkpoints", async () => {
  const previews = new RoutePreviews(async () => fakeEngine());
  for (const level of [8, 26, 42, 0, 8]) {
    const route = { ...state(0, [{ buttons: 1, frames: 240 }]), boot_level: level, snapshot: snapshot(level * 1000 + 480) };
    const pixels = await previews.get(route);
    assert.equal(pixels.data[0] + (pixels.data[1] << 8), level * 1000 + 480);
  }
});


test("hover trails follow exact divergent inputs and retain only the current bounded trail", async () => {
  let runs = 0;
  const previews = new RoutePreviews(async () => {
    const engine = fakeEngine(), run = engine.run;
    engine.run = (...args) => { runs++; run(...args); };
    return engine;
  }, 2, 8, (engine, frame) => ({level: 0, x: new DataView(engine.capture().buffer).getUint32(0, true), y: 32, frame}));
  const a = state('a', [{buttons:1,frames:120},{buttons:2,frames:120}]);
  const b = state('b', [{buttons:1,frames:120},{buttons:3,frames:120}]);
  await previews.get(a);
  const prefix = previews.timeline.trail.filter((p) => p.frame <= 120);
  assert.deepEqual(previews.timeline.trail.at(-1), {level:0,x:600,y:32,frame:240});
  await previews.get(b);
  assert.equal(previews.trailId, 'b');
  assert.deepEqual(previews.timeline.trail.filter((p) => p.frame <= 120), prefix);
  assert.equal(previews.timeline.trail.at(-1).x, 720);
  const before = runs;
  await previews.get(b);
  assert.equal(runs, before, 'The latest verified image and trail share the cache');
  await previews.get(a);
  assert.equal(previews.timeline.trail.at(-1).x,600,'An older cached screenshot cannot reuse a different route trail');
  await previews.get(state('long', [{buttons:0,frames:200000}]));
  assert.ok(previews.timeline.trail.length <= 6002);
  previews.clear();
  assert.equal(previews.trailId,null);
  assert.equal(previews.timeline.trail.length,0);
});
