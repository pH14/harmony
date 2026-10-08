// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { readFile, mkdir, writeFile } from "node:fs/promises";
import { createEngine, ROM_SHA256, CORE_REVISION } from "../src/emulator.js";
import { validateTape } from "../src/heat.js";
import { prefixAt, appendInput } from "../src/branch.js";
import { isMapEvidence } from "../src/world.js";
import { snapshotHash, CREDIT } from "../src/media.js";
import init, { Explorer } from "../rust/pkg/nova_browser.js";
const base = new URL("../public/", import.meta.url);
const engine = await createEngine(base, {
  rom: await readFile(new URL("nova.nes", base)),
  wasmBinary: await readFile(new URL("engine/quicknes.wasm", base)),
});
globalThis.harmonyEngine = engine;
await init({
  module_or_path: await readFile(
    new URL("../rust/pkg/nova_browser_bg.wasm", import.meta.url),
  ),
});
const catalog = JSON.parse(await readFile(new URL("maps.json", base)));
const root = engine.boot();
const extended = process.env.NOVA_PROGRESS_PATHS === "22000";
await mkdir("test-results", { recursive: true });
for (const seed of [1, 2, 3]) {
  engine.restore(root);
  const search = new Explorer(seed),
    arrivals = new Map();
  let nextLevel = false;
  for (
    let tries = 0;
    tries < (extended ? 22000 : 6000) &&
    (extended ? !nextLevel : arrivals.size < 2);
    tries += 2
  ) {
    const batch = JSON.parse(search.advance(2));
    for (const point of batch.points)
      if (
        [49, 45, 1].includes(point.observation.level) &&
        isMapEvidence(point.observation, catalog.levels) &&
        point.retained !== null &&
        !arrivals.has(point.observation.level)
      ) {
        const state = JSON.parse(search.state(point.retained)),
          snapshot = search.snapshot(point.retained);
        assert.ok(state.observation.health > 0);
        if (state.observation.level === 1) {
          assert.equal(state.observation.selected_level, 1);
          assert.ok(state.observation.cleared_levels[0] & 1);
          nextLevel = true;
        } else assert.equal(state.observation.selected_level, 0);
        engine.restore(root);
        for (const action of state.actions)
          engine.run(action.buttons, action.frames);
        assert.deepEqual(
          engine.capture(),
          snapshot,
          "Compressed archived state must replay exactly from its real controller history",
        );
        arrivals.set(point.observation.level, batch.executions);
        const tape = {
          format: "harmony-nova-browser-v1",
          rom_sha256: ROM_SHA256,
          core_revision: CORE_REVISION,
          ...state,
          endpoint_sha256: await snapshotHash(snapshot),
          credit: CREDIT,
        };
        await writeFile(
          `test-results/door-${seed}-${point.observation.level}.json`,
          JSON.stringify(tape),
        );
      }
    assert.ok(
      batch.snapshot_bytes > 0 && batch.snapshot_bytes < 128 * 1024 * 1024,
    );
    if (batch.stopped) break;
  }
  assert.ok(
    arrivals.has(49),
    `Seed ${seed} must preserve the intro-to-garden door`,
  );
  assert.ok(
    arrivals.has(45),
    `Seed ${seed} must preserve the garden-to-main door`,
  );
  assert.ok(arrivals.get(49) <= arrivals.get(45));
  if (extended && seed !== 1)
    assert.ok(nextLevel, `Seed ${seed} must enter Level 2 after a real clear`);
  search.free();
  console.log(
    `Seed ${seed}: Garden at ${arrivals.get(49)} paths, main area at ${arrivals.get(45)}; exact replays passed; Level 2 ${nextLevel ? "reached" : extended ? "censored at 22,000 paths" : "checked by recorded witnesses and archive admission"}.`,
  );
}

let checkpoint, prefix;
for (const name of ["main-exit", "level-two-2", "level-two-3"]) {
  const tape = JSON.parse(
    await readFile(new URL(`fixtures/${name}.json`, import.meta.url)),
  );
  validateTape(tape);
  engine.restore(root);
  for (const action of tape.actions) engine.run(action.buttons, action.frames);
  assert.equal(
    await snapshotHash(engine.capture()),
    tape.endpoint_sha256,
    "Recorded witness must reproduce its authoritative endpoint",
  );
  assert.ok(isMapEvidence(engine.observation(), catalog.levels));
  if (name === "main-exit") {
    assert.equal(engine.observation().level, 45);
    assert.equal(engine.observation().selected_level, 0);
    checkpoint = engine.capture();
    prefix = tape.actions;
  } else {
    assert.equal(engine.observation().selected_level, 1);
    assert.ok(engine.observation().cleared_levels[0] & 1);
    if (name === "level-two-2") {
      const expected = engine.capture();
      assert.deepEqual(tape.actions.slice(0, prefix.length), prefix);
      engine.restore(checkpoint);
      for (const action of tape.actions.slice(prefix.length))
        engine.run(action.buttons, action.frames);
      assert.deepEqual(
        engine.capture(),
        expected,
        "Stored Main checkpoint plus the real suffix must exactly reproduce Level 2",
      );
    }
  }
}

engine.restore(root);
const bounded = new Explorer(1);
bounded.set_snapshot_budget(16 * 1024 * 1024);
let batch, lastRetained;
for (let tries = 0; tries < 6000; tries += 2) {
  batch = JSON.parse(bounded.advance(2));
  for (const point of batch.points)
    if (point.retained !== null) lastRetained = point.retained;
  if (batch.stopped) break;
}
assert.ok(batch.stopped && batch.snapshot_bytes >= 16 * 1024 * 1024);
assert.ok(
  batch.snapshot_bytes < 16 * 1024 * 1024 + 8 * engine.capture().length,
);
assert.ok(lastRetained > 0);
const retained = JSON.parse(bounded.state(lastRetained));
const snapshot = bounded.snapshot(lastRetained);
assert.ok(retained.frames > 0 && snapshot.length > 0);
engine.restore(root);
for (const action of retained.actions)
  engine.run(action.buttons, action.frames);
assert.deepEqual(
  engine.capture(),
  snapshot,
  "A late retained state must replay exactly after the budget stop",
);
bounded.free();
console.log(
  "Small snapshot budget stops with its retained histories available.",
);

const levelTwo = JSON.parse(
  await readFile(new URL("fixtures/level-two-2.json", import.meta.url)),
);
const mainExit = JSON.parse(
  await readFile(new URL("fixtures/main-exit.json", import.meta.url)),
);
for (const [label, prefix, seed] of [
  ["mid-action", prefixAt(levelTwo.actions, 7), 11],
  ["Main manual continuation", mainExit.actions.map((a) => ({ ...a })), 13],
  [
    "later-level manual continuation",
    levelTwo.actions.map((a) => ({ ...a })),
    17,
  ],
]) {
  engine.restore(root);
  for (const action of prefix) engine.run(action.buttons, action.frames, true);
  appendInput(prefix, 0, 2);
  engine.run(0, 2, true);
  appendInput(prefix, 1, 6);
  engine.run(1, 6, true);
  const manual = engine.capture();
  const branch = Explorer.from_history(seed, JSON.stringify(prefix));
  assert.deepEqual(JSON.parse(branch.state(0)).actions, prefix);
  assert.deepEqual(branch.snapshot(0), manual);
  assert.equal(branch.state_count(), 1);
  assert.equal(branch.snapshot_bytes() > 0, true);
  let descendant;
  for (let jobs = 0; jobs < 20 && descendant === undefined; jobs += 2) {
    for (const point of JSON.parse(branch.advance(2)).points)
      if (point.retained !== null && point.retained > 0)
        descendant = point.retained;
  }
  assert.ok(descendant > 0, `${label} must produce a real retained descendant`);
  const child = JSON.parse(branch.state(descendant)),
    snapshot = branch.snapshot(descendant);
  assert.deepEqual(child.actions.slice(0, prefix.length), prefix);
  engine.restore(root);
  for (const action of child.actions) engine.run(action.buttons, action.frames);
  assert.deepEqual(
    engine.capture(),
    snapshot,
    `${label} descendant must exactly replay from the original game root`,
  );
  branch.free();
}
console.log(
  "Mid-action and manually guided roots in Main and Level 2 produce exact replayable descendants.",
);
