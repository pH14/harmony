// SPDX-License-Identifier: AGPL-3.0-or-later
import { test } from "node:test";
import assert from "node:assert/strict";
import { SmoothCounter } from "../src/counter.js";

test("a counter climbs between samples and never exceeds the latest value", () => {
  const counter = new SmoothCounter();
  counter.push(1000, 0);
  counter.push(2000, 100);
  const reads = [100, 120, 140, 180, 225, 400].map((t) => counter.read(t));
  for (let i = 1; i < reads.length; i++) assert.ok(reads[i] >= reads[i - 1]);
  assert.equal(reads[0], 1000);
  assert.equal(reads.at(-1), 2000);
  assert.ok(reads[2] > 1000 && reads[2] < 2000);
});

test("a counter that moves backwards or resets jumps to the new value", () => {
  const counter = new SmoothCounter();
  counter.push(500, 0);
  counter.push(900, 100);
  counter.push(10, 200);
  assert.equal(counter.read(200), 10);
  counter.reset();
  counter.push(7, 300);
  assert.equal(counter.read(310), 7);
});
