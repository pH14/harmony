// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { readFile, writeFile, mkdir, rename } from "node:fs/promises";
import { join } from "node:path";
import { gzipSync, gunzipSync } from "node:zlib";
import { machine, expand, compress, replay, hash } from "./runtime.mjs";

const [componentsPath, discoveryIndexPath, originalRoms, repairedIndexPath, repairedRoms, runtime, output] = process.argv.slice(2);
if (!output) throw new Error("triage-closed-components.mjs COMPONENTS DISCOVERY_INDEX ORIGINAL_ROMS REPAIRED_INDEX REPAIRED_ROMS RUNTIME OUTPUT");
const componentBytes = await readFile(componentsPath), discoveryBytes = await readFile(discoveryIndexPath);
const repairedBytes = await readFile(repairedIndexPath);
const components = JSON.parse(componentBytes), discovery = JSON.parse(discoveryBytes), repaired = JSON.parse(repairedBytes);
assert.equal(components.index_sha256, hash(discoveryBytes));
await mkdir(output, { recursive: true });
const machines = new Map();
async function getMachine(directory, layout, verify) {
  const path = join(directory, layout.rom);
  if (!machines.has(path)) machines.set(path, await machine(path, runtime));
  const m = machines.get(path);
  if (verify) assert.equal(m.rom_sha256, layout.rom_sha256);
  return m;
}
const report = {
  schema: 1, status: "running", components_sha256: hash(componentBytes),
  discovery_index_sha256: hash(discoveryBytes), repaired_index_sha256: hash(repairedBytes),
  selection: "closed fill components without sprite origins; one longest-surviving representative per component",
  limitation: "Frame counts belong to the discovery build. Lag can change input timing. Survival is triage, not collision or intent proof. Original source/ROM geometry still needs independent validation.",
  cases: [],
};
async function save() {
  await writeFile(join(output, "triage.tmp"), JSON.stringify(report, null, 2) + "\n");
  await rename(join(output, "triage.tmp"), join(output, "triage.json"));
}
for (const [key, component] of Object.entries(components.components)) {
  if (component.open_faces.length || component.sprites.length) continue;
  const representative = component.entries.reduce((a, b) => a.max_alive_after >= b.max_alive_after ? a : b).representative;
  const [side, levelText] = key.split(":"), level = Number(levelText);
  const layout = discovery.levels.find(l => l.side === side && l.level === level);
  const newLayout = repaired.levels.find(l => l.side === side && l.level === level);
  assert.ok(layout && newLayout);
  const bytes = await readFile(representative.path);
  assert.equal(hash(bytes), representative.sha);
  const candidate = JSON.parse(gunzipSync(bytes));
  assert.equal(candidate.rom_sha256, layout.rom_sha256);
  assert.equal(candidate.side, side);
  assert.equal(candidate.level, level);
  const tape = expand(candidate.actions), row = { component: key, bbox: component.bbox,
    candidate_sha256: hash(bytes), discovery_frame: representative.frame, outcomes: {} };
  for (const [variant, directory, selected, verify] of [
    ["original", originalRoms, layout, false], ["repaired", repairedRoms, newLayout, true],
  ]) {
    const m = await getMachine(directory, selected, verify);
    assert.equal(m.core_sha256, candidate.core_sha256);
    const trace = await replay(m, side, level, tape, candidate.setup.menu_delay, candidate.setup.startup_released_frames);
    const death = trace.findIndex(o => o.dead) + 1;
    row.outcomes[variant] = { rom_sha256: m.rom_sha256, core_sha256: m.core_sha256,
      frames: trace.length, first_dead: death || null, endpoint: trace.at(-1),
      alive_at_discovery_frame: trace.length >= representative.frame && !trace[representative.frame - 1].dead };
    if (variant === "original" && row.outcomes[variant].alive_at_discovery_frame) {
      const name = key.replaceAll(":", "-") + "-original.json.gz";
      const witness = { schema: 1, engine: "dissonance-campaign", purpose: "closed-component-triage",
        discovery: { component: key, candidate_sha256: hash(bytes), discovery_frame: representative.frame },
        options: { side, menu_delay: candidate.setup.menu_delay,
          startup_released_frames: candidate.setup.startup_released_frames,
          fceumm_startup_released_frames: candidate.setup.startup_released_frames === 22 ? 180 : candidate.setup.startup_released_frames },
        boot_level: level, target_level: level, rom_sha256: m.rom_sha256, core_sha256: m.core_sha256,
        actions: compress(tape.slice(0, trace.length)), trace };
      const saved = gzipSync(JSON.stringify(witness));
      await writeFile(join(output, name), saved);
      row.outcomes.original.witness = name;
      row.outcomes.original.witness_sha256 = hash(saved);
    }
  }
  report.cases.push(row);
  await save();
  if (report.cases.length % 10 === 0) console.log("replayed components", report.cases.length);
}
report.status = "replays-complete-source-review-required";
await save();
console.log(JSON.stringify({ status: report.status, components: report.cases.length }));
