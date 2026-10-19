// SPDX-License-Identifier: AGPL-3.0-or-later
import test from "node:test";
import assert from "node:assert/strict";
import { snapshotDigest } from "../src/media.js";
test("snapshot digests match the Rust archive reference vectors", () => {
  assert.equal(snapshotDigest(new Uint8Array()), "811c9dc501c93a75");
  assert.equal(snapshotDigest(new TextEncoder().encode("twin")), "9a8704f335dd7e16");
  assert.notEqual(snapshotDigest(new TextEncoder().encode("twio")), "9a8704f335dd7e16");
});
