// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { test } from "node:test";
import { compress, expand, routeTape, suspect, tileAt } from "./runtime.mjs";

test("map-builder assistance cannot become an upstream gameplay witness", () => {
  assert.throws(() => routeTape("000a000"), /assisted/);
  assert.throws(() => routeTape(null), /missing/);
  assert.throws(() => expand([{ buttons: 128, frames: 1 }]), /only A/);
  assert.deepEqual(expand(compress([0, ...Array(85).fill(1), 0])), [0, ...Array(85).fill(1), 0]);
});

test("world geometry is bottom-aligned and never wraps outside the source grid", () => {
  const layout = { width: 2, height: 1, top: 896, tiles: [79, 111] };
  assert.deepEqual(tileAt(layout, 16, 900), { column: 1, row: 0, id: 111 });
  assert.equal(tileAt(layout, -1, 900), null);
  assert.equal(tileAt(layout, 32, 900), null);
  assert.equal(tileAt(layout, 0, 912), null);
});

test("decorative-fill signal excludes unsupported and transient gameplay", () => {
  const layout = { width: 2, height: 1, top: 896, tiles: [0, 0] },
    metatiles = [{ name: "X_NO_OUTLINE", collision: "COL_NONE" }],
    bands = { 0: 800 }, point = { x: 0, y: 897, state: 2 };
  assert(suspect(point, layout, metatiles, bands));
  for (const field of ["dead", "debug", "invincible", "paused", "dual"])
    assert.equal(suspect({ ...point, [field]: 1 }, layout, metatiles, bands), null);
  assert.equal(suspect(point, layout, metatiles, { 0: 880 }), null);
  assert.equal(suspect(point, layout, metatiles, {}), null);
  assert.equal(suspect(point, layout, [{ name: "X_NO_OUTLINE", collision: "COL_ALL" }], bands), null);
  assert.equal(suspect(point, layout, [{ name: "FAKE_SPIKE_UP", collision: "COL_NONE" }], bands), null);
});
