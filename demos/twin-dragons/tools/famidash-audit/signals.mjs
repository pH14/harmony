// SPDX-License-Identifier: AGPL-3.0-or-later
import { tileAt } from "./runtime.mjs";

const pointKinds = {
  COL_PIT_TOP: "COL_DEATH_TOP", COL_FULL_FLOOR_SPIKE: "COL_DEATH", COL_FULL_CEILING_SPIKE: "COL_DEATH",
  COL_FULL_LEFT_SPIKE: "COL_DEATH", COL_FULL_RIGHT_SPIKE: "COL_DEATH",
  COL_LEFT_BASE_SPIKE: "COL_DEATH_LEFT", COL_RIGHT_BASE_SPIKE: "COL_DEATH_RIGHT",
};

export const pointCollision = definition => pointKinds[definition.collision] ?? definition.collision;

function halfOpenIntersection(a, b, bounds) {
  let low = { n: 0, d: 1, open: false }, high = { n: 1, d: 1, open: false };
  for (const [start, end, left, right] of [[a.x, b.x, bounds[0], bounds[1]], [a.y, b.y, bounds[2], bounds[3]]]) {
    const delta = end - start;
    if (!delta) { if (start < left || start >= right) return false; continue; }
    const lower = { n: delta > 0 ? left - start : start - right, d: Math.abs(delta), open: delta < 0 };
    const upper = { n: delta > 0 ? right - start : start - left, d: Math.abs(delta), open: delta > 0 };
    const lowerOrder = lower.n * low.d - low.n * lower.d;
    const upperOrder = upper.n * high.d - high.n * upper.d;
    if (lowerOrder > 0) low = lower;
    else if (!lowerOrder) low.open ||= lower.open;
    if (upperOrder < 0) high = upper;
    else if (!upperOrder) high.open ||= upper.open;
  }
  const order = low.n * high.d - high.n * low.d;
  return order < 0 || (!order && !low.open && !high.open);
}

export function crossedBase(a, b, x, y, mask) {
  let crossed = 0;
  for (const [bit, bounds, axis] of [
    [1, [x, x + 16, y, y + 1], "y"], [2, [x, x + 16, y + 15, y + 16], "y"],
    [4, [x, x + 1, y, y + 16], "x"], [8, [x + 15, x + 16, y, y + 16], "x"],
  ]) {
    if (!(mask & bit)) continue;
    const offset = axis === "x" ? 0 : 2;
    if (Math.min(a[axis], b[axis]) <= bounds[offset] && Math.max(a[axis], b[axis]) >= bounds[offset + 1]
      && halfOpenIntersection(a, b, bounds)) crossed |= bit;
  }
  return crossed;
}

export function collisionPoint(previous, current, linearCoordinates = false, fractionalCarry = false) {
  if (!previous || !current || previous.mode !== current.mode || previous.mini !== current.mini) return null;
  const width = current.mode === 6 || current.mini ? 8 : 15;
  const height = current.mode === 6 && current.player !== 1 ? 8 : current.mini ? 7 : 15;
  const offset = (height >> 1) + (current.mini ? (16 - height) >> 1 : 0);
  return { x: (current.player === 1 ? current.x : previous.x) + (width >> 1) - 1,
    y: linearCoordinates ? current.y + offset + (fractionalCarry ? current.fractional_y_carry ?? 0 : 0)
      : current.camera + ((current.y - current.camera + offset) & 255) };
}

export function advanceCollisionPoint(previous, current, previousPoint, linearCoordinates = false, fractionalCarry = false) {
  if (!previous || !current) return null;
  if (["x", "y", "camera", "vy", "mode", "gravity", "mini", "dual", "state", "dead", "paused", "fractional_y_carry"]
    .every(key => previous[key] === current[key])) return previousPoint;
  return collisionPoint(previous, current, linearCoordinates, fractionalCarry);
}

export function players(m, stablePose = false) {
  const r = m.ram(), view = new DataView(r.buffer, r.byteOffset);
  let states;
  if (m.players) states = m.players();
  else {
    const primary = { ...m.observe(), player: 0 };
    states = [primary];
    if (primary.dual) states.push({ ...primary, player: 1,
      x: view.getInt32(0x4a2, true) + r[0x440], y: primary.camera + r[0x444],
      vy: view.getInt16(0x44b, true), gravity: r[0x44e], mini: r[0x450] });
  }
  const a = m.symbols ?? { currplayer: 0x462, currplayer_relx: 0x68,
    currplayer_rely: 0x6a, skipProcessingCubeRotationLogic: 0x5aa, scroll_x: 0x4a2 };
  if (stablePose && !states[0].dual && r[a.currplayer] === 0 && r[a.skipProcessingCubeRotationLogic])
    states[0] = { ...states[0], x: view.getInt32(a.scroll_x, true) + r[a.currplayer_relx + 1],
      y: states[0].camera + r[a.currplayer_rely + 1] };
  if (stablePose) states = states.map((state, player) => ({ ...state, fractional_y_carry:
    (r[!player && !state.dual && r[a.currplayer] === 0 && r[a.skipProcessingCubeRotationLogic]
      ? a.currplayer_rely : (a.player_rely ?? 0x441) + player * 2] + r[a.scroll_y_subpx ?? 0x4a8]) >> 8 }));
  return states;
}

export function signals(o, layout, metatiles) {
  if (o.x < 128 || o.state !== 2 || o.dead || o.debug || o.invincible || o.paused) return [];
  const result = [], tile = tileAt(layout, o.x + 8, o.y + 8);
  const definition = tile && metatiles[tile.id];
  const kind = definition && pointCollision(definition);
  const add = (kind, t = tile, extra = {}) => result.push({ kind, ...t, ...extra });
  if (o.x > layout.width * 16 + 64) add("missed-finish", null);
  if (definition) {
    const x = (o.x + 8) % 16, y = (o.y + 8) % 16;
    if (kind === "COL_NONE" && ["CHECKERBOARD_15", "X_NO_OUTLINE", "BLACK_NONE"].includes(definition.name))
      add("decorative-interior", tile, { name: definition.name });
    if (kind === "COL_ALL" && x >= 3 && x <= 12 && y >= 3 && y <= 12) add("solid-overlap");
    if (kind === "COL_DEATH" && x >= 4 && x <= 8 && y >= 4 && y <= 11) add("hazard-overlap");
    if (["COL_DEATH_BOTTOM", "COL_PIT"].includes(kind) && x >= 5 && x <= 7 && y > 10) add("pit-hazard-overlap");
  } else if (o.y < layout.top - 32 || o.y >= layout.top + layout.height * 16) add("outside-source-rows");
  for (const distance of [16, 32, 48]) {
    const above = tileAt(layout, o.x + 8, o.y + 8 - distance);
    if (above && ["COL_DEATH_BOTTOM", "COL_PIT", "COL_DEATH", "COL_TOP_CENTER_SPIKE", "COL_BOTTOM_CENTER_SPIKE"].includes(pointCollision(metatiles[above.id]))) {
      add("hazard-underpass", above, { distance });
      break;
    }
  }
  for (const [kind, dx, dy, mask] of [["base-overpass", 0, 1, 1], ["base-left-pass", 1, 0, 4], ["base-right-pass", -1, 0, 8]]) {
    for (const distance of [16, 32, 48]) {
      const near = tileAt(layout, o.x + 8 + dx * distance, o.y + 8 + dy * distance);
      if (near && (metatiles[near.id].base_mask & mask)) { add(kind, near, { distance }); break; }
    }
  }
  return result;
}
