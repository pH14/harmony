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
import { campaignSearch, campaignWorker, coreIdentity, runCampaign } from "../src/campaign.js";
const base = new URL("../public/", import.meta.url);
const engine = await createEngine(base, twinDragons, {
  rom: await readFile(new URL("twin-dragons.nes", base)),
  wasmBinary: await readFile(new URL("engine/quicknes.wasm", base)),
});
const identity = coreIdentity("browser");
const worker = campaignWorker(engine, identity);
const catalog = JSON.parse(await readFile(new URL("maps.json", base)));
const root = engine.boot();
const extended = process.env.TWIN_DRAGONS_PROGRESS_PATHS === "25000";
const budget = extended ? 25000 : 12000;
await mkdir("test-results", { recursive: true });
for (const seed of extended ? [1, 2, 27] : [1]) {
  const search = campaignSearch(engine, identity, { seed, executions: budget }),
    arrivals = new Map();
  while (!arrivals.has(1) && runCampaign(search, worker, { jobs: 16 })) {
    const batch = JSON.parse(search.take_batch());
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
          "An archived state must replay exactly from its real controller history",
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
  }
  assert.ok(
    arrivals.has(1),
    `Seed ${seed} must enter Level 2 after a real clear within ${budget} executions`,
  );
  search.free();
  console.log(
    `Seed ${seed}: ${[...arrivals].map(([level, paths]) => `Level ${level + 1} at ${paths} executions`).join(", ")}; exact replays passed.`,
  );
}

function shuffledRun(reverse) {
  const search = campaignSearch(engine, identity, { seed: 7, executions: 800, record: true });
  for (;;) {
    const jobs = [];
    for (let job; (job = search.plan()); ) jobs.push(job);
    if (!jobs.length) break;
    const outcomes = jobs.map((job) => worker.run(job));
    if (reverse) outcomes.reverse();
    for (const outcome of outcomes) search.admit(outcome);
  }
  const stream = search.take_stream().split("\n").slice(1).join("\n"),
    batch = JSON.parse(search.take_batch()),
    found = batch.points.filter((p) => p.retained !== null).map((p) => [p.retained, search.digest(p.retained)]);
  return { search, stream, found };
}
const ordered = shuffledRun(false), reversed = shuffledRun(true), repeated = shuffledRun(false);
assert.equal(repeated.stream, ordered.stream, "A fixed seed must make the same search choices on every run");
assert.equal(reversed.stream, ordered.stream, "Results that arrive out of order must not change the search choices");
assert.deepEqual(reversed.found, ordered.found, "Retained states and their snapshot digests must match");
const [lateId, lateDigest] = ordered.found.at(-1);
const lateState = JSON.parse(ordered.search.state(lateId));
engine.restore(root);
for (const action of lateState.actions) engine.run(action.buttons, action.frames);
assert.equal(snapshotDigest(engine.capture()), lateDigest, "A late retained state must replay exactly");
for (const run of [ordered, reversed, repeated]) run.search.free();
console.log(`Out-of-order results repeat the in-order search stream exactly over 800 executions; a late state replays exactly.`);

const fixtures = {};
for (const name of ["level-one", "level-two", "level-three"]) {
  const tape = JSON.parse(
    await readFile(new URL(`fixtures/${name}.json`, import.meta.url)),
  );
  validateTape(tape, twinDragons, CORE_REVISION);
  engine.restore(root);
  for (const action of tape.actions) engine.run(action.buttons, action.frames);
  assert.equal(
    await snapshotHash(engine.capture()),
    tape.endpoint_sha256,
    "Recorded witness must reproduce its authoritative endpoint",
  );
  const observation = engine.observation();
  assert.ok(isMapEvidence(observation, catalog.levels));
  assert.deepEqual(observation, Object.fromEntries(Object.keys(observation).map((key) => [key, tape.observation[key]])));
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

const smallBudget = 3;
const bounded = campaignSearch(engine, identity, { seed: 1, memoryMiB: smallBudget, executions: 6000 });
let batch, lastRetained, peak = 0;
while (runCampaign(bounded, worker, { jobs: 16 })) {
  batch = JSON.parse(bounded.take_batch());
  for (const point of batch.points)
    if (point.retained !== null) lastRetained = point.retained;
  peak = Math.max(peak, batch.snapshot_bytes);
}
batch = JSON.parse(bounded.take_batch());
assert.equal(batch.executions, 6000, "A small snapshot budget keeps searching until its execution budget");
assert.ok(peak > 0 && peak <= smallBudget * 1048576, `Resident snapshots stay within the budget (${peak} bytes)`);
assert.ok(lastRetained > 0);
const retained = JSON.parse(bounded.state(lastRetained));
assert.ok(retained.frames > 0);
engine.restore(root);
for (const action of retained.actions)
  engine.run(action.buttons, action.frames);
assert.equal(
  snapshotDigest(engine.capture()),
  bounded.digest(lastRetained),
  "A late retained state must replay exactly within the budget",
);
bounded.free();
console.log(`Small snapshot budget keeps searching: peak ${peak} bytes; retained histories replay.`);

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
  const branch = campaignSearch(engine, identity, { seed, root: manual, prefix });
  assert.deepEqual(JSON.parse(branch.state(0)).actions, prefix);
  assert.deepEqual(branch.snapshot(0), manual);
  assert.equal(JSON.parse(branch.take_batch()).states, 1);
  let descendant;
  for (let jobs = 0; jobs < 256 && descendant === undefined; jobs += 16) {
    runCampaign(branch, worker, { jobs: 16 });
    for (const point of JSON.parse(branch.take_batch()).points)
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
