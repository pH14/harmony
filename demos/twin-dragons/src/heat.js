// SPDX-License-Identifier: AGPL-3.0-or-later
import { MOTION_STRIDE, NO_ROOM } from "./swarm.js";

export const CELL_SIZE = 32;
const HALF_LIFE_ROLLOUTS = 2000;
const HALF_LIFE_MS = 2500;
export const cellRow = (y) => Math.floor((y - 8) / CELL_SIZE);
export const cellTop = (row) => row * CELL_SIZE + 8;
export const cellKey = (o) =>
  `${o.level}:${Math.floor(o.x / CELL_SIZE)}:${cellRow(o.y)}`;
export class Heatmap {
  constructor() {
    this.cells = new Map();
    this.elapsed = 0;
    this.runningSince = 0;
    this.work = 0;
    this.peaks = null;
    this.pins = new Map();
  }
  clock(now) {
    return (
      this.elapsed +
      (this.runningSince === null ? 0 : Math.max(0, now - this.runningSince))
    );
  }
  setRunning(running, now) {
    this.elapsed = this.clock(now);
    this.runningSince = running ? now : null;
  }
  retain(point, now) {
    const key = cellKey(point.observation);
    let cell = this.cells.get(key);
    if (!cell) {
      cell = {
        key,
        level: point.observation.level,
        x: Math.floor(point.observation.x / 32),
        y: cellRow(point.observation.y),
        heat: 0,
        work: this.work,
        time: this.clock(now),
        visits: 0,
        ids: [],
        routes: new Map(),
        nextRoute: 0,
      };
      this.cells.set(key, cell);
    }
    if (point.retained !== null && !cell.ids.includes(point.retained)) {
      if (!cell.routes.has(point.retained))
        cell.routes.set(point.retained, ++cell.nextRoute);
      cell.ids.unshift(point.retained);
      cell.ids = cell.ids.slice(0, 12);
      this.trimRoutes(cell);
    }
    return cell;
  }
  trimRoutes(cell) {
    const keep = new Set(cell.ids);
    for (const pin of this.pins.values()) if (pin.cell === cell) keep.add(pin.id);
    for (const id of cell.routes.keys()) if (!keep.has(id)) cell.routes.delete(id);
  }
  setPin(kind, cell, id, request, ordinal) {
    if (cell && this.cells.get(cell.key) !== cell) return;
    if (cell && Number.isSafeInteger(ordinal) && ordinal >= 1 && ordinal <= cell.nextRoute)
      cell.routes.set(id, ordinal);
    const previous = this.pins.get(kind);
    if (cell?.routes.has(id)) this.pins.set(kind, { cell, id, request });
    else this.pins.delete(kind);
    if (previous) this.trimRoutes(previous.cell);
    if (cell) this.trimRoutes(cell);
  }
  select(cell, id, ordinal) {
    this.setPin("selected", cell, id, undefined, ordinal);
  }
  pending(cell, id, request, ordinal) {
    this.setPin("pending", cell, id, request, ordinal);
  }
  releasePending(request) {
    const pin = this.pins.get("pending");
    if (pin && (request === undefined || request === pin.request))
      this.setPin("pending", null);
  }
  clearPins() {
    const cells = [...this.pins.values()].map((pin) => pin.cell);
    this.pins.clear();
    for (const cell of cells) this.trimRoutes(cell);
  }
  forget(ids, request) {
    const gone = new Set(ids), replacements = new Map();
    const pending = this.pins.get("pending");
    if (pending && pending.request === request && gone.has(pending.id))
      this.releasePending(request);
    for (const cell of this.cells.values()) {
      const dropped = cell.ids.filter((id) => gone.has(id));
      if (!dropped.length) continue;
      cell.ids = cell.ids.filter((id) => !gone.has(id));
      for (const id of dropped) replacements.set(id, cell.ids[0] ?? null);
      this.trimRoutes(cell);
    }
    return replacements;
  }
  visit(point, now) {
    const cell = this.retain(point, now);
    cell.heat = this.value(cell, now) + 1;
    cell.work = this.work;
    cell.time = this.clock(now);
    cell.visits++;
    this.peaks = null;
    return cell;
  }
  visitMotion(trails, maps, now, exactRows = false) {
    for (const samples of trails) {
      this.work++;
      const visited = new Set();
      for (let i = 0; i < samples.length; i += MOTION_STRIDE) {
        const level = samples[i + 1], map = maps.get(level);
        if (level === NO_ROOM || !map || samples[i + 2] >= map.width) continue;
        const x = samples[i + 2] + 8, y = samples[i + 3] + (exactRows ? 0 : 8);
        if (x >= map.width || y >= map.height) continue;
        const key = (level * 4096 + Math.floor(x / CELL_SIZE)) * 64 + cellRow(y);
        if (visited.has(key)) continue;
        visited.add(key);
        this.visit({ observation: { level, x, y }, retained: null }, now);
      }
    }
  }
  value(cell, now) {
    const age = (this.work - cell.work) / HALF_LIFE_ROLLOUTS + Math.max(0, this.clock(now) - cell.time) / HALF_LIFE_MS;
    return cell.heat * Math.pow(0.5, age);
  }
  color(cell, now) {
    if (this.peaks === null) {
      this.peaks = new Map();
      this.peakTime = this.clock(now);
      for (const c of this.cells.values()) this.peaks.set(c.level, Math.max(this.peaks.get(c.level) || 0, this.value(c, now)));
    }
    const peak = this.peaks.get(cell.level) * Math.pow(0.5, (this.clock(now) - this.peakTime) / HALF_LIFE_MS),
      v = peak > 0 ? Math.min(1, this.value(cell, now) / peak) : 0,
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
export function validateTape(tape, game, revision) {
  if (
    !tape ||
    tape.format !== `harmony-${game.id}-browser-v1` ||
    tape.rom_sha256 !== game.rom.sha256 ||
    tape.core_revision !== revision ||
    !/^[0-9a-f]{64}$/.test(tape.endpoint_sha256 || "") ||
    !Array.isArray(tape.actions) ||
    tape.actions.length > 10000 ||
    !Number.isInteger(tape.boot_level ?? 0) ||
    (tape.boot_level ?? 0) < 0 || (tape.boot_level ?? 0) >= game.levelCount
  )
    throw new Error(`Unsupported ${game.title} history`);
  let frames = 0;
  for (const a of tape.actions) {
    if (
      !Number.isInteger(a.buttons) ||
      a.buttons < 0 ||
      a.buttons > 255 ||
      (a.buttons & 48) === 48 ||
      (a.buttons & 192) === 192 ||
      !Number.isInteger(a.frames) ||
      a.frames < 1 ||
      a.frames > game.maxActionFrames ||
      (a.buttons & 12 && !game.menuButtons)
    )
      throw new Error("Invalid controller action");
    frames += a.frames;
  }
  if (frames > 200000) throw new Error("History too long");
  return frames;
}
