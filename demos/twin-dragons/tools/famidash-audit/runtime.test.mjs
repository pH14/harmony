// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { test } from "node:test";
import { readFileSync } from "node:fs";
import { compress, expand, routeTape, suspect, tileAt } from "./runtime.mjs";
import { collisionPoint, advanceCollisionPoint, players } from "./signals.mjs";

test("source review uses the live pose during the captured trail-rendering phase", () => {
  const r = new Uint8Array(readFileSync(new URL("../../../../workloads/nes-games/src/famidash/fixtures/trail-render.wram", import.meta.url)));
  const m = { ram: () => r, observe: () => ({ x: 22314, y: 789, camera: 678, dual: 0 }) };
  assert.deepEqual([players(m)[0].x, players(m)[0].y], [22314, 789]);
  assert.deepEqual([players(m, true)[0].x, players(m, true)[0].y], [22334, 797]);
});

test("lag repeats the last collision sample rather than inventing horizontal motion", () => {
  const before = { x: 5211, y: 771, camera: 601, mode: 0, mini: 1 };
  const moving = { ...before, x: 5215, y: 773 };
  const sample = advanceCollisionPoint(before, moving, null);
  assert.deepEqual(sample, { x: 5214, y: 780 });
  assert.deepEqual(advanceCollisionPoint(moving, { ...moving }, sample), sample);
  assert.deepEqual(advanceCollisionPoint(moving, { ...moving, x: 5218, y: 768 }, sample), { x: 5218, y: 775 });
});

test("centre estimates match independently traced Theory of Everything collision reads", () => {
  const previous = { x: 3891, y: 883, camera: 662, mode: 3, mini: 0 };
  const current = { x: 3894, y: 886, camera: 662, mode: 3, mini: 0 };
  assert.deepEqual(collisionPoint(previous, current), { x: 3897, y: 893 });
  assert.equal(collisionPoint(previous, { ...current, mode: 6 }), null);
  assert.equal(collisionPoint(previous, { ...current, mini: 1 }), null);
});

test("release centre uses the traced eight-bit relative-Y addition before camera translation", () => {
  const previous = { x: 9249, y: 866, camera: 614, mode: 1, mini: 1 };
  const current = { ...previous, x: 9251, y: 867 };
  assert.deepEqual(collisionPoint(previous, current), { x: 9252, y: 618 });
  assert.deepEqual(collisionPoint(previous, current, true), { x: 9252, y: 874 });
});

test("dual wave estimates match the second player's independently traced collision phase", () => {
  const previous = { x: 9397, y: 811, camera: 710, mode: 6, mini: 0, player: 1 };
  const current = { ...previous, x: 9401, y: 807 };
  assert.deepEqual(collisionPoint(previous, current), { x: 9404, y: 814 });
  assert.deepEqual(collisionPoint(previous, { ...current, player: 0 }), { x: 9400, y: 811 });
});

test("fractional camera carry matches Nightmare's traced centre conversion", () => {
  const previous = { x: 4817, y: 667, camera: 603, mode: 0, mini: 0, fractional_y_carry: 0 };
  const current = { ...previous, x: 4820, y: 660, camera: 603, fractional_y_carry: 1 };
  assert.deepEqual(collisionPoint(previous, current, true), { x: 4823, y: 667 });
  assert.deepEqual(collisionPoint(previous, current, true, true), { x: 4823, y: 668 });
});

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
