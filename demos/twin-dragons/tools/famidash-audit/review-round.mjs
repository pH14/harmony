// SPDX-License-Identifier: AGPL-3.0-or-later
import { readFile, writeFile, mkdir, readdir } from "node:fs/promises";
import { join } from "node:path";
import { gunzipSync } from "node:zlib";
import { machine, hash, tileAt } from "./runtime.mjs";
import { players, advanceCollisionPoint } from "./signals.mjs";

const [indexPath, roms, runtime, roundDirectory, output] = process.argv.slice(2);
if (!output) throw new Error("review-round.mjs INDEX ROMS RUNTIME ROUND_DIRECTORY OUTPUT");
const index = JSON.parse(await readFile(indexPath));
await mkdir(output, { recursive: true });
let ledger;
try { ledger = JSON.parse(await readFile(join(output, "review.json"))); }
catch { ledger = { schema: 1, cases: {}, entries: {}, mismatches: [], reviewed_candidates: 0, status: "review-in-progress" }; }
if (ledger.detector_revision !== 7) { ledger.cases = {}; ledger.entries = {}; ledger.mismatches = []; ledger.reviewed_candidates = 0; ledger.detector_revision = 7; }
ledger.status = "replay-in-progress";
ledger.source_review_complete = false;
let entryCount = Object.keys(ledger.entries).length, lastSavedAt = performance.now();
const machines = new Map();
const coveredLevels = new Map();
function fill(o, layout) {
  if (!o || o.dead || o.state !== 2 || o.debug || o.invincible || o.paused || o.x < 128) return null;
  const tile = tileAt(layout, o.x + 8, o.y + 8);
  return tile && ["CHECKERBOARD_15", "X_NO_OUTLINE", "BLACK_NONE"].includes(index.metatiles[tile.id].name) ? tile : null;
}
function intersection(a, b, left, top, right, bottom) {
  let low = 0, high = 1;
  for (const [start, delta, min, max] of [[a.x, b.x - a.x, left, right], [a.y, b.y - a.y, top, bottom]]) {
    if (delta === 0) { if (start < min || start > max) return false; continue; }
    const t1 = (min - start) / delta, t2 = (max - start) / delta;
    low = Math.max(low, Math.min(t1, t2)); high = Math.min(high, Math.max(t1, t2));
    if (high < low) return false;
  }
  return true;
}
function hazards(a, b, previous, current, layout) {
  if (!previous || current.x < 128 || previous.mode !== current.mode) return [];
  if (Math.abs(current.x - previous.x) > 32 || Math.abs(current.y - previous.y) > 32) return [];
  if (current.mode === 5 && current.gravity !== previous.gravity) return [];
  if (!a || !b) return [];
  if (Math.abs(b.x - a.x) > 32 || Math.abs(b.y - a.y) > 32) return [];
  const result = [];
  for (let y = Math.floor(Math.min(a.y, b.y) / 16) * 16; y <= Math.max(a.y, b.y); y += 16)
    for (let x = Math.floor(Math.min(a.x, b.x) / 16) * 16; x <= Math.max(a.x, b.x); x += 16) {
      const tile = tileAt(layout, x, y); if (!tile) continue;
      const collision = index.metatiles[tile.id].collision;
      const box = collision === "COL_DEATH" ? [4, 4, 8, 11]
        : ["COL_DEATH_BOTTOM", "COL_PIT"].includes(collision) ? [5, 11, 7, 15]
        : collision === "COL_DEATH_TOP" ? [5, 0, 7, 5] : null;
      if (box && intersection(a, b, x + box[0], y + box[1], x + box[2], y + box[3]))
        result.push({ ...tile, collision, x: current.x, y: current.y });
    }
  return result;
}
function crossedHazardRow(trace, frame, player, layout) {
  const current = trace[frame][player], previous = trace[frame - 1]?.[player];
  if (!previous || current.y <= previous.y || current.x < 128) return null;
  const tile = tileAt(layout, current.x + 8, current.y + 8 - 16);
  if (!tile) return null;
  const dangerous = id => ["COL_DEATH", "COL_DEATH_BOTTOM", "COL_PIT", "COL_DEATH_TOP"].includes(index.metatiles[id].collision);
  if (!dangerous(tile.id)) return null;
  for (const x of [current.x + 8 - 16, current.x + 8 + 16]) {
    const neighbor = tileAt(layout, x, current.y + 8 - 16);
    if (!neighbor || neighbor.row !== tile.row || !dangerous(neighbor.id)) return null;
  }
  const top = layout.top + tile.row * 16, plane = top + 24;
  if (previous.y + 8 >= plane || current.y + 8 < plane) return null;
  if (!trace.slice(Math.max(0, frame - 16), frame).some(states => states[player] && states[player].y + 8 < top + 4)) return null;
  return tile;
}
function addEntry(key, evidence, duration, aliveAfter) {
  let entry = ledger.entries[key];
  if (!entry) {
    entry = ledger.entries[key] = { key, disposition: "pending-source-review", evidence_count: 0,
      max_duration: 0, max_alive_after: 0, representative: evidence };
    entryCount++;
  }
  entry.evidence_count++;
  if (aliveAfter > entry.max_alive_after || duration > entry.max_duration) entry.representative = evidence;
  entry.max_duration = Math.max(entry.max_duration, duration);
  entry.max_alive_after = Math.max(entry.max_alive_after, aliveAfter);
}
const save = async () => { await writeFile(join(output, "review.json.tmp"), JSON.stringify(ledger, null, 2));
  const { rename } = await import("node:fs/promises"); await rename(join(output, "review.json.tmp"), join(output, "review.json")); };
for (const directory of (await readdir(roundDirectory, { withFileTypes: true })).filter(d => d.isDirectory())) {
  const root = join(roundDirectory, directory.name);
  let report; try { report = JSON.parse(await readFile(join(root, "report.json"))); } catch { continue; }
  const layout = index.levels.find(l => l.side === report.side && l.level === report.level); if (!layout) continue;
  coveredLevels.set(`${layout.side}:${layout.level}`, { status: report.status, candidates: report.candidates.length });
  if (!machines.has(layout.rom)) machines.set(layout.rom, await machine(join(roms, layout.rom), runtime));
  const m = machines.get(layout.rom);
  m.boot(layout.side, layout.level, report.setup?.menu_delay ?? 0, report.setup?.startup_released_frames);
  const rootState = m.core.capture(), prefixes = new Map();
  for (const summary of report.candidates) {
    const path = join(root, summary.file), bytes = await readFile(path), sha = hash(bytes);
    if (ledger.cases[sha]) continue;
    const candidate = JSON.parse(gunzipSync(bytes));
    if ((candidate.setup?.menu_delay ?? 0) !== (report.setup?.menu_delay ?? 0))
      throw new Error("candidate and report use different menu setups");
    if (candidate.setup?.startup_released_frames !== report.setup?.startup_released_frames)
      throw new Error("candidate and report use different startup waits");
    const keys = []; let key = "root", prefix = null;
    for (let i = 0; i < candidate.actions.length; i++) {
      const action = candidate.actions[i]; key = hash(`${key}:${action.buttons}:${action.frames}`); keys.push(key);
      if (prefixes.has(key)) prefix = { ...prefixes.get(key), action: i };
    }
    m.core.restore(prefix?.state ?? rootState);
    const trace = prefix ? prefix.trace.slice() : [], validatedPrefixFrames = trace.length, created = [];
    for (let i = (prefix?.action ?? -1) + 1; i < candidate.actions.length; i++) {
      const action = candidate.actions[i];
      for (let f = 0; f < action.frames; f++) { m.core.step(action.buttons, 1); trace.push(players(m)); }
      if (i % 32 === 31 || i === candidate.actions.length - 1) {
        const cached = { state: m.core.capture(), trace: trace.slice(), covered: false };
        prefixes.set(keys[i], cached); created.push(cached);
        const limit = Math.max(16, Math.min(256, Math.floor(1000000 / Math.max(1, trace.length))));
        while (prefixes.size > limit) prefixes.delete(prefixes.keys().next().value);
      }
    }
    const endpoint = trace.at(-1)?.[candidate.player];
    const mismatches = Object.keys(candidate.observation).filter(k => candidate.observation[k] !== endpoint?.[k]);
    if (mismatches.length) {
      ledger.mismatches.push({ path, sha, mismatches, expected: candidate.observation, actual: endpoint });
      ledger.cases[sha] = { path, disposition: "replay-mismatch" }; continue;
    }
    let tail = 0;
    for (; tail < 16; tail++) { m.core.step(0, 1); const states = players(m); trace.push(states); if (states[0].dead || states[0].state !== 2) break; }
    const before = entryCount;
    for (const player of [0, 1]) {
      let start = null, centre = null, linearCentre = null;
      for (let frame = 0; frame < trace.length; frame++) {
        const o = trace[frame][player], previous = trace[frame - 1]?.[player];
        const previousCentre = centre;
        centre = advanceCollisionPoint(previous, o, centre, index.centre_coordinates === "linear");
        linearCentre = advanceCollisionPoint(previous, o, linearCentre, true);
        const tile = fill(o, layout), previousTile = fill(previous, layout);
        if (tile && previousTile && tile.id !== previousTile.id) {
          const key = `${layout.side}:${layout.level}:fill:${tile.column}:${tile.row}:style:${player}`;
          addEntry(key, { path, sha, frame: frame + 1, player, tile, observation: o, previous }, 1, Math.min(trace.length - frame, 512));
        }
        if (tile && !previousTile) start = { frame, tile, observation: o, previous };
        if (start && (!tile || frame === trace.length - 1)) {
          const duration = frame - start.frame + (tile ? 1 : 0);
          let aliveAfter = 0; for (let n = start.frame; n < trace.length && aliveAfter < 512; n++) {
            if (!trace[n][player] || trace[n][player].dead || trace[n][player].state !== 2) break; aliveAfter++;
          }
          if (duration >= 2) {
            const direction = !start.previous ? "spawn" : Math.abs(start.observation.x - start.previous.x) > Math.abs(start.observation.y - start.previous.y)
              ? start.observation.x >= start.previous.x ? "right" : "left" : start.observation.y >= start.previous.y ? "down" : "up";
            const key = `${layout.side}:${layout.level}:fill:${start.tile.column}:${start.tile.row}:${direction}:${player}`;
            addEntry(key, { path, sha, frame: start.frame + 1, player, tile: start.tile, observation: start.observation, previous: start.previous }, duration, aliveAfter);
          }
          start = null;
        }
        if (!o || o.dead || o.state !== 2 || o.debug || o.invincible || o.paused) continue;
        const survives = frame + 8 < trace.length && !trace.slice(frame, frame + 9).some(states => !states[player] || states[player].dead || states[player].state !== 2);
        const solid = centre && tileAt(layout, centre.x, centre.y);
        const physicalTile = linearCentre && tileAt(layout, linearCentre.x, linearCentre.y);
        if (survives && centre && physicalTile && Math.abs(linearCentre.y - centre.y) === 256) {
          const type = index.metatiles[physicalTile.id].collision, x = linearCentre.x & 15, y = linearCentre.y & 15;
          const hit = type === "COL_ALL" ? x >= 3 && x <= 12 && y >= 3 && y <= 12
            : type === "COL_DEATH" ? x >= 4 && x <= 8 && y >= 4 && y <= 11
            : ["COL_DEATH_BOTTOM", "COL_PIT"].includes(type) ? x >= 5 && x <= 7 && y >= 11
            : type === "COL_DEATH_TOP" ? x >= 5 && x <= 7 && y <= 5 : false;
          if (hit) {
            const key = `${layout.side}:${layout.level}:centre-wrap:${physicalTile.column}:${physicalTile.row}:${player}`;
            addEntry(key, { path, sha, frame: frame + 1, player, tile: physicalTile, observation: o, previous,
              actual_probe: centre, physical_centre: linearCentre, requires_wave_dblock_review: o.mode === 6 }, 1, Math.min(trace.length - frame, 512));
          }
        }
        if (survives && solid && index.metatiles[solid.id].collision === "COL_ALL" &&
            (centre.x & 15) >= 3 && (centre.x & 15) <= 12 && (centre.y & 15) >= 3 && (centre.y & 15) <= 12) {
          const key = `${layout.side}:${layout.level}:solid:${solid.column}:${solid.row}:${player}`;
          addEntry(key, { path, sha, frame: frame + 1, player, tile: solid, observation: o, previous,
            requires_wave_dblock_review: o.mode === 6 }, 1, Math.min(trace.length - frame, 512));
        }
        if (survives && o.x > layout.width * 16 + 64) {
          const key = `${layout.side}:${layout.level}:outside:right:${player}`;
          addEntry(key, { path, sha, frame: frame + 1, player, observation: o, previous }, 1, Math.min(trace.length - frame, 512));
        }
        const rowCrossing = crossedHazardRow(trace, frame, player, layout);
        if (rowCrossing && frame + 8 < trace.length && !trace.slice(frame, frame + 9).some(states => !states[player] || states[player].dead || states[player].state !== 2)) {
          const key = `${layout.side}:${layout.level}:hazard-row:${rowCrossing.column}:${rowCrossing.row}:${player}`;
          addEntry(key, { path, sha, frame: frame + 1, player, tile: rowCrossing, observation: o, previous }, 1, Math.min(trace.length - frame, 512));
        }
        if (!tileAt(layout, o.x + 8, o.y + 8) && (o.y < layout.top - 32 || o.y >= layout.top + layout.height * 16)) {
          const key = `${layout.side}:${layout.level}:outside:${o.y < layout.top ? "top" : "bottom"}:${player}`;
          addEntry(key, { path, sha, frame: frame + 1, player, observation: o, previous }, 1, Math.min(trace.length - frame, 512));
        }
        for (const tile of hazards(previousCentre, centre, previous, o, layout)) {
          if (frame + 8 >= trace.length || trace.slice(frame, frame + 9).some(states => !states[player] || states[player].dead || states[player].state !== 2)) continue;
          const key = `${layout.side}:${layout.level}:hazard:${tile.column}:${tile.row}:${tile.collision}:${player}`;
          addEntry(key, { path, sha, frame: frame + 1, player, tile, observation: o, previous }, 1, Math.min(trace.length - frame, 512));
        }
      }
    }
    for (const cached of created) cached.covered = true;
    ledger.cases[sha] = { path, disposition: "replayed", validated_prefix_frames: validatedPrefixFrames,
      entries_added: entryCount - before };
    ledger.reviewed_candidates++;
    if (ledger.reviewed_candidates % 25 === 0) {
      if (performance.now() - lastSavedAt >= 10000) { await save(); lastSavedAt = performance.now(); }
      console.log("replayed", ledger.reviewed_candidates, "entry candidates", entryCount);
    }
  }
  await save();
}
await save();
ledger.level_coverage = Object.fromEntries(coveredLevels);
ledger.expected_levels = index.levels.length;
ledger.status = coveredLevels.size === index.levels.length && [...coveredLevels.values()].every(level => level.status === "searched")
  ? "replay-inventory-complete" : "partial-replay-inventory";
ledger.source_review_complete = false;
await save();
console.log("replay inventory", ledger.reviewed_candidates, Object.keys(ledger.entries).length, "mismatches", ledger.mismatches.length);
