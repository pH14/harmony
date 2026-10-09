// SPDX-License-Identifier: AGPL-3.0-or-later
export const CELL_SIZE = 32;
export const cellKey = (o) =>
  `${o.level}:${Math.floor(o.x / CELL_SIZE)}:${Math.floor(o.y / CELL_SIZE)}`;
export class Heatmap {
  constructor() {
    this.cells = new Map();
  }
  visit(point, now) {
    const key = cellKey(point.observation);
    let cell = this.cells.get(key);
    if (!cell) {
      cell = {
        key,
        level: point.observation.level,
        x: Math.floor(point.observation.x / 32),
        y: Math.floor(point.observation.y / 32),
        heat: 0,
        time: now,
        visits: 0,
        ids: [],
        routes: new Map(),
      };
      this.cells.set(key, cell);
    }
    cell.heat = this.value(cell, now) + 1;
    cell.time = now;
    cell.visits++;
    if (point.retained !== null && !cell.ids.includes(point.retained)) {
      if (!cell.routes.has(point.retained))
        cell.routes.set(point.retained, cell.routes.size + 1);
      cell.ids.unshift(point.retained);
      cell.ids = cell.ids.slice(0, 12);
    }
    return cell;
  }
  value(cell, now) {
    return cell.heat * Math.pow(0.5, Math.max(0, now - cell.time) / 6000);
  }
  color(cell, now) {
    const value = this.value(cell, now);
    if (value < 0.08) return null;
    const v = Math.min(1, value / 22),
      stops = [
        [64, 124, 181],
        [74, 212, 132],
        [249, 174, 65],
        [245, 89, 62],
      ],
      scaled = v * 3,
      i = Math.min(2, Math.floor(scaled)),
      f = scaled - i;
    return stops[i].map((x, j) => Math.round(x + (stops[i + 1][j] - x) * f));
  }
}
export function routeIds(cell, selectedId) {
  const ids = [...(cell?.ids || [])];
  if (cell?.routes?.has(selectedId) && !ids.includes(selectedId))
    ids.push(selectedId);
  return ids.sort((a, b) => cell.routes.get(a) - cell.routes.get(b));
}
export function validateTape(tape) {
  if (
    !tape ||
    tape.format !== "harmony-nova-browser-v1" ||
    tape.rom_sha256 !==
      "9107be62a08a0ae51a01f900bd18a95a52fe043f53e9cae4a64d1e8e73114b08" ||
    tape.core_revision !== "26bb785c9deddb66a17717b21bb4e328f03ade32" ||
    !/^[0-9a-f]{64}$/.test(tape.endpoint_sha256 || "") ||
    !Array.isArray(tape.actions) ||
    tape.actions.length > 10000
  )
    throw new Error("Unsupported Nova history");
  let frames = 0;
  for (const a of tape.actions) {
    if (
      !Number.isInteger(a.buttons) ||
      a.buttons < 0 ||
      a.buttons > 255 ||
      a.buttons & 12 ||
      (a.buttons & 48) === 48 ||
      (a.buttons & 192) === 192 ||
      !Number.isInteger(a.frames) ||
      a.frames < 1 ||
      a.frames > 120
    )
      throw new Error("Invalid controller action");
    frames += a.frames;
  }
  if (frames > 200000) throw new Error("History too long");
  return frames;
}
