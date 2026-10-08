// SPDX-License-Identifier: AGPL-3.0-or-later
import test from "node:test";
import assert from "node:assert/strict";
import { ConsoleFilter } from "../src/console-filter.mjs";
test("protocol and report bytes stay hidden across arbitrary stream boundaries", () => {
  const input =
    'internal wrapper echo\r\n\x1eHARMONY_EXEC\r\nbranch point: after action 1\r\n# touch /tmp/proof\r\n\x1eHARMONY_BEGIN\r\n{"report":true}\r\n\x1eHARMONY_END\r\nharmony-linux# ';
  for (let size = 1; size < input.length; size++) {
    const f = new ConsoleFilter();
    f.begin();
    let actual = "";
    for (let i = 0; i < input.length; i += size)
      actual += f.write(input.slice(i, i + size));
    assert.equal(
      actual,
      "branch point: after action 1\r\n# touch /tmp/proof\r\nharmony-linux# ",
    );
  }
});

test("interactive carriage returns and cursor controls are preserved", () => {
  const f = new ConsoleFilter();
  assert.equal(f.write("abc\rdef\x1b[K"), "abc\rdef\x1b[K");
});
