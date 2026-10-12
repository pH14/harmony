// SPDX-License-Identifier: AGPL-3.0-or-later
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { readEvidence, atTime, heat, command } from "../src/evidence.mjs";
const read = (name) =>
  readEvidence(
    JSON.parse(
      readFileSync(new URL(`../public/evidence/${name}.json`, import.meta.url)),
    ),
  );
test("real reports preserve the failing invariant and successful counterfactual", () => {
  const f = read("failure"),
    q = read("quorum");
  assert.deepEqual(f.replay.violations, ["raft-ack-durability"]);
  assert.deepEqual(q.replay.violations, []);
  assert.equal(q.replay.actions_applied, 3);
  assert(f.events.some((e) => e.kind === "acknowledged"));
  assert(!q.events.some((e) => e.kind === "acknowledged"));
});
test("rewind branch exposes the actual missing quorum trace", () => {
  assert(
    read("investigation").events.some(
      (e) =>
        e.message === "ack sent with replica acknowledgments=1; required=2",
    ),
  );
});
test("cumulative console tails are deduplicated and future events stay hidden", () => {
  const e = read("failure");
  assert.equal(e.events.filter((x) => x.kind === "ready").length, 3);
  assert(atTime(e, 0.8).every((x) => x.time <= 0.8));
  assert(!atTime(e, 0.8).some((x) => x.kind === "acknowledged"));
});
test("branch timelines map suffix actions to original phases", () => {
  assert.deepEqual(
    read("investigation").steps.map((x) => x.step),
    [0, 1, 2, 3, 4],
  );
});
test("activity cools, and commands cannot inject shell syntax", () => {
  const events = [{ line: 1, time: 1 }];
  assert(heat(events, 1, 1) > heat(events, 2, 1));
  assert.equal(heat(events, 0, 1), 0);
  assert.equal(
    command("failure", 1, "trace"),
    "harmony branch failure --step 1 --exec '/app/control trace' --name investigation",
  );
  assert.throws(() => command("failure;evil", 1, "shell"));
});
