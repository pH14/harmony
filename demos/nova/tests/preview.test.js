// SPDX-License-Identifier: AGPL-3.0-or-later
import { test } from "node:test";
import assert from "node:assert/strict";
import { RoutePreviews } from "../src/preview.js";
const snapshot = (value) => new Uint8Array(new Uint32Array([value]).buffer);
function fakeEngine() {
  let value = 0, rendered = 0;
  return {
    boot() { value = 0; return snapshot(value); },
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
