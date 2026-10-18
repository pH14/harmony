// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { pathToFileURL } from "node:url";
import { join } from "node:path";
import { machine, expand, compress } from "./runtime.mjs";

const [oldPackage, newPackage, rom, runtime, clearingInput] = process.argv.slice(2);
if (!clearingInput) throw new Error("check-search.mjs OLD_PACKAGE NEW_PACKAGE ROM RUNTIME CLEARING_INPUT");
const packages = [];
for (const path of [oldPackage, newPackage]) {
  const bindings = await import(pathToFileURL(join(path, "twin_dragons_browser.js")));
  await bindings.default({ module_or_path: await readFile(join(path, "twin_dragons_browser_bg.wasm")) });
  packages.push(bindings);
}
const m = await machine(rom, runtime), powerOn = m.core.capture(), romBytes = await readFile(rom);
globalThis.harmonyCores = { 0: { ...m.core, rom: romBytes, reset: () => m.core.restore(powerOn),
  memory: () => m.ram().slice(), ramView: m.ram,
  frames(buttons, frames) { const data = new Uint8Array(frames * 2048); for (let i = 0; i < frames; i++) { m.core.step(buttons, 1); data.set(m.ram(), i * 2048); } return data; } } };
const identity = `quicknes-browser:${m.core.revision}`, streams = [];
for (const bindings of packages) {
  const search = new bindings.CampaignSearch("famidash", 0, romBytes, identity, m.core_sha256, 91, undefined, "[]", 64, 128, true);
  const worker = new bindings.CampaignWorker("famidash", 0, romBytes, identity, m.core_sha256);
  for (let job; (job = search.plan());) search.admit(worker.run(job), worker.note());
  streams.push(search.take_stream().split("\n").slice(1).join("\n"));
  assert.equal(JSON.parse(search.take_batch()).executions, 128);
  search.free(); worker.free();
}
assert.equal(streams[0], streams[1], "the production campaign stream must remain unchanged");
const tape = expand(JSON.parse(await readFile(clearingInput)).actions), prefix = tape.slice(0, -32);
m.boot("main", 0);
for (const button of prefix) m.core.step(button, 1);
const bindings = packages[1], budget = 128;
const search = bindings.famidash_level_search("famidash", 0, romBytes, identity, m.core_sha256, 0, 1,
  m.core.capture(), JSON.stringify(compress(prefix)), 64, budget, "energy_splice:6");
const worker = bindings.famidash_level_worker("famidash", 0, romBytes, identity, m.core_sha256, 0);
let jobs = 0, continuedAfterVictory = false;
for (let job; (job = search.plan());) {
  const alreadyWon = search.won();
  search.admit(worker.run(job), worker.note());
  jobs++;
  if (alreadyWon) continuedAfterVictory = true;
  if (search.won() && jobs < budget) assert.equal(search.stopped(), false);
  const batch = JSON.parse(search.take_batch());
  for (const point of batch.points) if (point.retained !== null) {
    m.core.restore(search.snapshot(point.retained));
    assert.equal(m.observe().level, 0, "audit search must remain on the selected level");
  }
}
assert.equal(jobs, budget);
assert.equal(search.stopped(), true);
assert.equal(continuedAfterVictory, true);
const victory = expand(JSON.parse(search.victory()));
m.boot("main", 0);
for (const button of victory) m.core.step(button, 1);
assert.equal(m.observe().state, 3, "exported victory must replay from ordinary level boot");
assert.equal(m.observe().level, 0);
search.free(); worker.free();
console.log("production stream unchanged; single-level audit continues after victory, exhausts its budget, and exports a replayable ordinary-input victory");
