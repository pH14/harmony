// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { crossedBase } from "./signals.mjs";
import { hash } from "./runtime.mjs";

const [path] = process.argv.slice(2);
if (!path) throw new Error("check-base-review.mjs EXACT_ORACLE.bin");
const expected = await readFile(path), deltas = [-16, -8, -5, -4, -3, -1, 0, 1, 3, 4, 5, 8, 16];
let cases = 0;
for (const mask of [2, 1, 4, 8]) for (let x = -16; x < 32; x++) for (let y = -16; y < 32; y++)
  for (const dx of deltas) for (const dy of deltas) {
    const actual = Number(!!crossedBase({ x, y }, { x: x + dx, y: y + dy }, 0, 0, mask));
    assert.equal(actual, expected[cases], `mask=${mask} x=${x} y=${y} dx=${dx} dy=${dy}`);
    cases++;
  }
assert.equal(cases, expected.length);
console.log(JSON.stringify({ status: "review-base-detector-matches-exact-oracle", cases, oracle_sha256: hash(expected) }));
