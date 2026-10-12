// SPDX-License-Identifier: AGPL-3.0-or-later
import test from "node:test";
import assert from "node:assert/strict";
import { SearchLoop } from "../src/loop.js";
test("quick pause/resume cancels pending work and rejects a stale callback", () => {
  let steps = 0,
    next = 0;
  const timers = new Map(),
    loop = new SearchLoop(
      () => {
        steps++;
        return true;
      },
      {
        schedule: (cb) => {
          const id = ++next;
          timers.set(id, cb);
          return id;
        },
        cancel: (id) => timers.delete(id),
      },
    );
  loop.resume();
  assert.equal(steps, 1);
  const stale = timers.values().next().value;
  loop.pause();
  loop.resume();
  assert.equal(steps, 2);
  assert.equal(timers.size, 1);
  stale();
  assert.equal(steps, 2);
  assert.equal(timers.size, 1);
  for (let i = 0; i < 20; i++) {
    const [id, cb] = timers.entries().next().value;
    timers.delete(id);
    cb();
    assert.equal(timers.size, 1);
  }
  assert.equal(steps, 22);
  loop.pause();
  assert.equal(timers.size, 0);
});
test("budget completion leaves no scheduled work", () => {
  let scheduled = 0;
  const loop = new SearchLoop(() => false, {
    schedule: () => scheduled++,
    cancel: () => {},
  });
  loop.resume();
  assert.equal(loop.active, false);
  assert.equal(scheduled, 0);
});
