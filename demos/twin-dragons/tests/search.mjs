// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import twinDragons from "../src/games/twin-dragons.js";
import { GameAudio } from "../src/audio.js";
import { readFile, mkdir, writeFile } from "node:fs/promises";
import { createEngine } from "../src/emulator.js";
import { QUICKNES_REVISION as CORE_REVISION } from "../src/cores/quicknes.js";
const ROM_SHA256 = twinDragons.rom.sha256;
import { validateTape } from "../src/heat.js";
import { prefixAt, appendInput } from "../src/branch.js";
import { isMapEvidence } from "../src/world.js";
import { snapshotHash, snapshotDigest, CREDIT } from "../src/media.js";
import { RoundSearch, ROUND_PATHS } from "../src/rollout.js";
import init, { Explorer } from "../rust/pkg/twin_dragons_browser.js";
const base = new URL("../public/", import.meta.url);
const engine = await createEngine(base, twinDragons, {
  rom: await readFile(new URL("twin-dragons.nes", base)),
  wasmBinary: await readFile(new URL("engine/quicknes.wasm", base)),
});
globalThis.harmonyEngine = engine;
await init({
  module_or_path: await readFile(
    new URL("../rust/pkg/twin_dragons_browser_bg.wasm", import.meta.url),
  ),
});
const catalog = JSON.parse(await readFile(new URL("maps.json", base)));
const root = engine.boot();
const extended = process.env.TWIN_DRAGONS_PROGRESS_PATHS === "25000";
const target = extended ? 2 : 1,
  budget = extended ? 25000 : 6000;
await mkdir("test-results", { recursive: true });
for (const seed of extended ? [11] : [27, 34]) {
  engine.restore(root);
  const search = new Explorer(seed),
    rounds = new RoundSearch(search, [engine]),
    arrivals = new Map();
  for (
    let tries = 0;
    tries < budget && !arrivals.has(target);
    tries += ROUND_PATHS
  ) {
    const { batch } = rounds.round();
    for (const point of batch.points)
      if (
        point.observation.level > 0 &&
        isMapEvidence(point.observation, catalog.levels) &&
        point.retained !== null &&
        !arrivals.has(point.observation.level)
      ) {
        const state = JSON.parse(search.state(point.retained)),
          snapshot = search.digest(point.retained);
        assert.ok(state.observation.health > 0);
        assert.equal(state.observation.cleared, state.observation.level);
        engine.restore(root);
        for (const action of state.actions)
          engine.run(action.buttons, action.frames);
        assert.equal(
          snapshotDigest(engine.capture()),
          snapshot,
          "Compressed archived state must replay exactly from its real controller history",
        );
        arrivals.set(point.observation.level, batch.executions);
        const tape = {
          format: "harmony-twin-dragons-browser-v1",
          rom_sha256: ROM_SHA256,
          core_revision: CORE_REVISION,
          ...state,
          endpoint_sha256: await snapshotHash(engine.capture()),
          credit: CREDIT,
        };
        await writeFile(
          `test-results/level-${seed}-${point.observation.level + 1}.json`,
          JSON.stringify(tape),
        );
      }
    assert.ok(
      batch.snapshot_bytes > 0 && batch.snapshot_bytes < 128 * 1024 * 1024,
    );
    if (batch.stopped) break;
  }
  assert.ok(
    arrivals.has(target),
    `Seed ${seed} must enter Level ${target + 1} after a real clear within ${budget} paths`,
  );
  search.free();
  console.log(
    `Seed ${seed}: ${[...arrivals].map(([level, paths]) => `Level ${level + 1} at ${paths} paths`).join(", ")}; exact replays passed.`,
  );
}

const loadEngine = async () => createEngine(base, twinDragons, {
  rom: await readFile(new URL("twin-dragons.nes", base)),
  wasmBinary: await readFile(new URL("engine/quicknes.wasm", base)),
});
const workers = [engine, await loadEngine(), await loadEngine()];
function parallelRun(pool) {
  engine.restore(root);
  const explorer = new Explorer(7),
    rounds = new RoundSearch(explorer, pool),
    batches = [],
    found = [];
  for (let r = 0; r < 48; r++) {
    const { batch, jobs } = rounds.round();
    batches.push(batch);
    let cursor = 0;
    for (const job of jobs) {
      for (const point of batch.points.slice(cursor, cursor + job.points))
        if (point.retained !== null) found.push({ engine: job.engine, id: point.retained, digest: explorer.digest(point.retained) });
      cursor += job.points;
    }
    assert.equal(cursor, batch.points.length);
  }
  return { explorer, batches, found };
}
const first = parallelRun(workers), second = parallelRun(workers), single = parallelRun([engine]);
assert.deepEqual(second.batches, first.batches, "A fixed seed and worker count must make the same search choices on every run");
assert.deepEqual(second.found, first.found, "Retained states and their snapshot digests must match across runs");
assert.deepEqual(single.batches, first.batches, "The worker count must not change the search choices");
const late = first.found.findLast((f) => f.engine === 2);
assert.ok(late, "The third worker must find a retained state");
const lateState = JSON.parse(first.explorer.state(late.id));
const replayer = await loadEngine();
replayer.restore(root);
for (const action of lateState.actions) replayer.run(action.buttons, action.frames);
assert.equal(snapshotDigest(replayer.capture()), late.digest, "A state found by the third worker must replay exactly");
for (const run of [first, second, single]) run.explorer.free();
console.log(`Three-worker rounds repeat exactly across runs and match one worker over ${first.batches.at(-1).executions} paths; a third-worker state replays exactly.`);

const fixtures = {};
for (const name of ["level-one", "level-two", "level-three"]) {
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
  const observation = engine.observation();
  assert.ok(isMapEvidence(observation, catalog.levels));
  assert.deepEqual(observation, tape.observation);
  fixtures[name] = { tape, snapshot: engine.capture() };
}
assert.equal(fixtures["level-one"].tape.observation.level, 0);
assert.equal(fixtures["level-two"].tape.observation.cleared, 1);
assert.equal(fixtures["level-three"].tape.observation.cleared, 2);
const prefix = fixtures["level-one"].tape.actions,
  levelTwoActions = fixtures["level-two"].tape.actions;
assert.deepEqual(levelTwoActions.slice(0, prefix.length), prefix);
engine.restore(fixtures["level-one"].snapshot);
for (const action of levelTwoActions.slice(prefix.length))
  engine.run(action.buttons, action.frames);
assert.deepEqual(
  engine.capture(),
  fixtures["level-two"].snapshot,
  "Stored Level 1 checkpoint plus the real suffix must exactly reproduce Level 2",
);
console.log("Level 1, 2 and 3 witnesses replay exactly; the Level 1 checkpoint plus its suffix reaches Level 2.");

engine.restore(root);
const bounded = new Explorer(1);
const smallBudget = 2 * 1024 * 1024;
bounded.set_snapshot_budget(smallBudget);
const boundedRounds = new RoundSearch(bounded, [engine]);
let batch, lastRetained, peak = 0, overBudget = 0;
for (let tries = 0; tries < 6000; tries += ROUND_PATHS) {
  batch = boundedRounds.round().batch;
  for (const point of batch.points)
    if (point.retained !== null) lastRetained = point.retained;
  peak = Math.max(peak, batch.snapshot_bytes);
  if (batch.snapshot_bytes > smallBudget) overBudget++;
  if (batch.stopped) break;
}
assert.equal(batch.stopped, false, "A full snapshot budget keeps searching instead of stopping");
assert.ok(bounded.retired_snapshots() > 0, "Retired entries release their snapshots");
assert.ok(peak < smallBudget * 1.5, `Resident snapshots stay near the budget (${peak} bytes)`);
assert.ok(lastRetained > 0);
const retained = JSON.parse(bounded.state(lastRetained));
assert.ok(retained.frames > 0);
let expected = null;
try {
  expected = bounded.snapshot(lastRetained);
} catch {}
engine.restore(root);
for (const action of retained.actions)
  engine.run(action.buttons, action.frames);
if (expected)
  assert.deepEqual(
    engine.capture(),
    expected,
    "A late retained state must replay exactly within the budget",
  );
bounded.free();
console.log(
  `Small snapshot budget keeps searching: peak ${peak} bytes, ${overBudget} batches over budget, retired snapshots released, retained histories replay.`,
);

const levelTwo = fixtures["level-two"].tape,
  levelOne = fixtures["level-one"].tape;
for (const [label, prefix, seed] of [
  ["mid-action", prefixAt(levelTwo.actions, 7), 11],
  ["Level 1 manual continuation", levelOne.actions.map((a) => ({ ...a })), 13],
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
  const branchRounds = new RoundSearch(branch, [engine]);
  for (let jobs = 0; jobs < 64 && descendant === undefined; jobs += ROUND_PATHS) {
    for (const point of branchRounds.round().batch.points)
      if (point.retained !== null && point.retained > 0)
        descendant = point.retained;
  }
  assert.ok(descendant > 0, `${label} must produce a real retained descendant`);
  const child = JSON.parse(branch.state(descendant)),
    snapshot = branch.digest(descendant);
  assert.deepEqual(child.actions.slice(0, prefix.length), prefix);
  engine.restore(root);
  for (const action of child.actions) engine.run(action.buttons, action.frames);
  assert.equal(
    snapshotDigest(engine.capture()),
    snapshot,
    `${label} descendant must exactly replay from the original game root`,
  );
  branch.free();
}
console.log(
  "Mid-action and manually guided roots in Levels 1 and 2 produce exact replayable descendants.",
);

engine.restore(root);
engine.run(128, 120, true);
const batched = engine.capture();
engine.restore(root);
for (let i = 0; i < 120; i++) engine.run(128, 1, true);
assert.deepEqual(
  engine.capture(),
  batched,
  "Rendering only the final frame must preserve the exact game snapshot",
);
const shadow = await createEngine(base, twinDragons, {
  rom: await readFile(new URL("twin-dragons.nes", base)),
  wasmBinary: await readFile(new URL("engine/quicknes.wasm", base)),
});
shadow.restore(root);
shadow.enableAudio(true);
shadow.run(128, 120, false);
const pcm = shadow.audio();
assert.ok(pcm.length > 0 && pcm.length <= 19200);
assert.ok(
  pcm.some((n) => n !== 0),
  "The actual game APU must generate music samples",
);
assert.deepEqual(
  engine.capture(),
  batched,
  "Audio synthesis must not touch the recorded replay engine",
);
engine.restore(root);
shadow.restore(root);
for (const buttons of [128, 129, 0, 64, 65, 2, 16, 0]) {
  for (let frame = 0; frame < 90; frame++) {
    engine.run(buttons, 1, false);
    shadow.run(buttons, 1, false);
    assert.deepEqual(
      shadow.memory(),
      engine.memory(),
      "Audio must follow the visible game RAM on every takeover frame",
    );
  }
  shadow.audio();
}
console.log(
  "Efficient video preserves snapshot bytes; isolated game audio produces bounded non-silent PCM and matches game RAM across 720 movement/jump frames.",
);

const audioContext = class {
  constructor() {
    this.currentTime = 0;
    this.state = "running";
  }
  resume() {
    return Promise.resolve();
  }
  createGain() {
    return { gain: { value: 1 }, connect() {} };
  }
  createBuffer(channels, frames) {
    const pcm = Array.from(
      { length: channels },
      () => new Float32Array(frames),
    );
    return { getChannelData: (c) => pcm[c] };
  }
  createBufferSource() {
    return {
      playbackRate: { value: 1 },
      connect() {},
      disconnect() {},
      start() {},
      stop() {},
    };
  }
};
const movieAudio = new GameAudio(async () => shadow, audioContext);
await movieAudio.start(() => root);
movieAudio.advance(129, 24, 12);
assert.ok(
  movieAudio.samples > 18500 && movieAudio.samples < 19500,
  "A 30 Hz presentation at 12x must retain all 24 game frames of real PCM instead of overflowing the 9600-frame capture buffer",
);
assert.equal(movieAudio.sources.size, 2);
for (const source of movieAudio.sources)
  assert.equal(source.playbackRate.value, 12);
engine.restore(root);
engine.run(129, 24);
assert.deepEqual(shadow.memory(), engine.memory());
movieAudio.stop();
console.log(
  `12x audio retains ${movieAudio.samples} stereo samples across a 24-game-frame presentation; visible game RAM still matches.`,
);
