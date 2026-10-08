// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { readFile, mkdir, writeFile } from "node:fs/promises";
import { createEngine, ROM_SHA256, CORE_REVISION } from "../src/emulator.js";
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
await mkdir("test-results", { recursive: true });
for (const seed of [1, 2, 3]) {
  engine.restore(root);
  const search = new Explorer(seed),
    arrivals = new Map();
  for (let tries = 0; tries < 6000 && arrivals.size < 2; tries += 2) {
    const batch = JSON.parse(search.advance(2));
    for (const point of batch.points)
      if (
        [49, 45].includes(point.observation.level) &&
        isMapEvidence(point.observation, catalog.levels) &&
        point.retained !== null &&
        !arrivals.has(point.observation.level)
      ) {
        const state = JSON.parse(search.state(point.retained)),
          snapshot = search.snapshot(point.retained);
        assert.ok(state.observation.health > 0);
        assert.equal(state.observation.selected_level, 0);
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
  console.log(
    `Seed ${seed}: Garden at ${arrivals.get(49)} paths, main area at ${arrivals.get(45)}; both exact replays passed.`,
  );
}
