// SPDX-License-Identifier: AGPL-3.0-or-later
import test from "node:test";
import assert from "node:assert/strict";
import { viewCenter, roomPanels, panelCenter, CAMERA_VIEWPORT } from "../src/view.js";
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

test("every room shows through one camera viewport that can reach every point", () => {
  for (const [width, height] of [[1280, 224], [3584, 224], [2048, 448], [256, 3584], [1024, 896]]) {
    const [panel, ...rest] = roomPanels(width, height);
    assert.equal(rest.length, 0);
    assert.deepEqual(panel, { index: 0, x: 0, y: 0, width, height, viewport: true });
    for (const viewport of [CAMERA_VIEWPORT.phone, CAMERA_VIEWPORT.desktop]) {
      const fit = Math.min(viewport.width / width, viewport.height / height);
      const zoom = Math.max(1, viewport.height / 224 / fit);
      for (const point of [{ x: 16, y: 16 }, { x: width - 16, y: height - 16 }, { x: width / 2, y: height / 2 }]) {
        const center = panelCenter(panel, zoom, point, viewport), scale = fit * zoom;
        const x = viewport.width / 2 + (point.x - center.x) * scale, y = viewport.height / 2 + (point.y - center.y) * scale;
        assert.ok(x >= 0 && x <= viewport.width && y >= 0 && y <= viewport.height, `${width}x${height} reaches ${point.x},${point.y}`);
      }
    }
  }
});
