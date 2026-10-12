// SPDX-License-Identifier: AGPL-3.0-or-later
import { test } from "node:test";
import assert from "node:assert/strict";
import { StateCache, memoryBudget } from "../src/memory.js";
test("coarse pointers and small devices use the smaller search budget", () => {
  assert.equal(memoryBudget(undefined, true).snapshotsMiB, 32);
  assert.equal(memoryBudget(4, false).snapshotsMiB, 32);
  assert.equal(memoryBudget(8, false).snapshotsMiB, 128);
});
test("preview eviction respects bytes, count, and recent reads", () => {
  const cache = new StateCache(2000, 2);
  const state = { actions: [], snapshot: new Uint8Array(100) };
  cache.set(1, state);
  cache.set(2, state);
  cache.get(1);
  cache.set(3, state);
  assert.equal(cache.get(2), undefined);
  assert.equal(cache.get(1), state);
  cache.set(4, { actions: Array(100).fill({}), snapshot: new Uint8Array(100) });
  assert.equal(cache.get(4), undefined);
  assert.ok(cache.bytes <= 2000 && cache.entries.size <= 2);
});
