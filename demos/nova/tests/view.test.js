// SPDX-License-Identifier: AGPL-3.0-or-later
import test from "node:test";
import assert from "node:assert/strict";
import { viewCenter, roomPanels, panelContains, panelCenter } from "../src/view.js";
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

test("wrapped rooms cover every original pixel exactly once, in order", () => {
  for (const available of [320, 390, 900, 1700])
    for (const [width, height] of [[1280,224], [3584,224], [4096,224], [2048,448], [256,3584], [1024,896]]) {
      const panels = roomPanels(width, height, available);
      assert.equal(panels.reduce((area, p) => area + p.width * p.height, 0), width * height);
      for (let y = 0; y < height; y += 16)
        for (let x = 0; x < width; x += 16)
          assert.equal(panels.filter(p => panelContains(p, {x,y})).length, 1);
      for (const p of panels) {
        assert.ok(p.width > 0 && p.height > 0);
        assert.equal(p.x % 32, 0);
        assert.equal(p.y % 224, 0);
        for (const zoom of [1,2,4]) {
          const point = {x:p.x+p.width-16,y:p.y+p.height-18};
          const center = panelCenter(p, zoom, point, p);
          const x=p.width/2+(point.x-center.x)*zoom,y=p.height/2+(point.y-center.y)*zoom;
          assert.ok(x>=0 && x<=p.width && y>=0 && y<=p.height);
          assert.ok(center.x>=p.x && center.y>=p.y);
        }
      }
    }
});
