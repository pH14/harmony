// SPDX-License-Identifier: AGPL-3.0-or-later
import test from "node:test";
import assert from "node:assert/strict";
import {
  CommandSession,
  ConsoleDecoder,
  markers,
  complete,
  report,
} from "../src/command-session.mjs";
import { ConsoleFilter } from "../src/console-filter.mjs";
const output = (id, value) =>
  `${markers(id).json}\n${JSON.stringify(value)}\n${markers(id).end}\nharmony-linux# `;
test("a late report cannot complete or label another command", () => {
  const old = output("1", { branch: "old" }),
    current = output("2", { branch: "current" });
  assert.equal(complete(old, "2"), false);
  assert.throws(() => report(old, "2"));
  assert.equal(complete(old + current, "2"), true);
  assert.deepEqual(report(old + current, "2"), { branch: "current" });
});
test("a timed-out command poisons the session even if it later finishes", () => {
  const session = new CommandSession(),
    id = session.start();
  assert.throws(() => session.start());
  session.fail(new Error("Timed out"));
  assert.equal(session.ready, false);
  assert.throws(() => session.finish(id));
  assert.throws(() => session.start(), /Reload/);
});
test("command framing stays hidden with nonce and split unicode bytes", () => {
  const decoder = new ConsoleDecoder(),
    filter = new ConsoleFilter();
  filter.begin("7");
  const bytes = new TextEncoder().encode(
    `echo\r\n${markers("7").start}\r\ncafé 🐿\r\n${output("7", { text: "é" })}`,
  );
  let actual = "";
  for (const byte of bytes)
    actual += filter.write(decoder.decode(new Uint8Array([byte])));
  assert.equal(actual, "café 🐿\r\nharmony-linux# ");
});
