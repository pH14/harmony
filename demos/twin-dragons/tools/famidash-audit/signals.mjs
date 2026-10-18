// SPDX-License-Identifier: AGPL-3.0-or-later
import { tileAt } from "./runtime.mjs";

export function collisionPoint(previous, current, linearCoordinates = false) {
  if (!previous || !current || previous.mode !== current.mode || previous.mini !== current.mini) return null;
  const width = current.mode === 6 || current.mini ? 8 : 15;
  const height = current.mode === 6 && current.player !== 1 ? 8 : current.mini ? 7 : 15;
  const offset = (height >> 1) + (current.mini ? (16 - height) >> 1 : 0);
  return { x: (current.player === 1 ? current.x : previous.x) + (width >> 1) - 1,
    y: linearCoordinates ? current.y + offset : current.camera + ((current.y - current.camera + offset) & 255) };
}

export function advanceCollisionPoint(previous, current, previousPoint, linearCoordinates = false) {
  if (!previous || !current) return null;
  if (["x", "y", "camera", "vy", "mode", "gravity", "mini", "dual", "state", "dead", "paused"]
    .every(key => previous[key] === current[key])) return previousPoint;
  return collisionPoint(previous, current, linearCoordinates);
}

export function players(m) {
  if (m.players) return m.players();
  const primary = { ...m.observe(), player: 0 }, r = m.ram();
  if (!primary.dual) return [primary];
  const view = new DataView(r.buffer, r.byteOffset);
  return [primary, { ...primary, player: 1,
    x: view.getInt32(0x4a2, true) + r[0x440], y: primary.camera + r[0x444],
    vy: view.getInt16(0x44b, true), gravity: r[0x44e], mini: r[0x450] }];
}

export function signals(o, layout, metatiles) {
  if (o.x < 128 || o.state !== 2 || o.dead || o.debug || o.invincible || o.paused) return [];
  const result = [], tile = tileAt(layout, o.x + 8, o.y + 8);
  const definition = tile && metatiles[tile.id];
  const add = (kind, t = tile, extra = {}) => result.push({ kind, ...t, ...extra });
  if (o.x > layout.width * 16 + 64) add("missed-finish", null);
  if (definition) {
    const x = (o.x + 8) % 16, y = (o.y + 8) % 16;
    if (definition.collision === "COL_NONE" && ["CHECKERBOARD_15", "X_NO_OUTLINE", "BLACK_NONE"].includes(definition.name))
      add("decorative-interior", tile, { name: definition.name });
    if (definition.collision === "COL_ALL" && x >= 3 && x <= 12 && y >= 3 && y <= 12) add("solid-overlap");
    if (definition.collision === "COL_DEATH" && x >= 4 && x <= 8 && y >= 4 && y <= 11) add("hazard-overlap");
    if (["COL_DEATH_BOTTOM", "COL_PIT"].includes(definition.collision) && x >= 5 && x <= 7 && y > 10) add("pit-hazard-overlap");
  } else if (o.y < layout.top - 32 || o.y >= layout.top + layout.height * 16) add("outside-source-rows");
  for (const distance of [16, 32, 48]) {
    const above = tileAt(layout, o.x + 8, o.y + 8 - distance);
    if (above && ["COL_DEATH_BOTTOM", "COL_PIT", "COL_DEATH", "COL_TOP_CENTER_SPIKE", "COL_BOTTOM_CENTER_SPIKE"].includes(metatiles[above.id].collision)) {
      add("hazard-underpass", above, { distance });
      break;
    }
  }
  return result;
}
