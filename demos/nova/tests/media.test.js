// SPDX-License-Identifier: AGPL-3.0-or-later
import test from "node:test";
import assert from "node:assert/strict";
import { snapshotDigest } from "../src/media.js";
test("snapshot digests match the Rust archive reference vectors", () => {
  assert.equal(snapshotDigest(new Uint8Array()), "811c9dc501c93a75");
  assert.equal(snapshotDigest(new TextEncoder().encode("nova")), "8193e2dfc1ebc4ee");
  assert.notEqual(snapshotDigest(new TextEncoder().encode("novb")), "8193e2dfc1ebc4ee");
});
