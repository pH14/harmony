// SPDX-License-Identifier: AGPL-3.0-or-later
import test from "node:test";
import assert from "node:assert/strict";
import { viewCenter } from "../src/view.js";
test("zoom keeps states near the floor and room edges in the viewport", () => {
  for (const width of [1280, 3584])
    for (const zoom of [1, 2, 4])
      for (const point of [
        { x: 52, y: 176 },
        { x: width - 16, y: 206 },
        { x: width / 2, y: 8 },
      ]) {
        const center = viewCenter(width, zoom, point),
          x = width / 2 + zoom * (point.x - center.x),
          y = 112 + zoom * (point.y - center.y);
        assert.ok(x >= 0 && x <= width);
        assert.ok(y >= 0 && y <= 224);
        assert.ok(center.x - width / (2 * zoom) >= 0);
        assert.ok(center.y - 112 / zoom >= 0);
      }
});
