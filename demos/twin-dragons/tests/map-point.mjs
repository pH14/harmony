// SPDX-License-Identifier: AGPL-3.0-or-later
export async function showOnMap(page, room, x, y, touch = false) {
  for (let attempt = 0; ; attempt++) {
    try {
      return await seekAndProject(page, room, x, y, touch);
    } catch (error) {
      if (attempt === 2 || !/not attached|detached/.test(error.message)) throw error;
    }
  }
}
async function seekAndProject(page, room, x, y, touch) {
  const strip = page.locator(`.map-row[data-map="${room}"] .level-overview`);
  await strip.scrollIntoViewIfNeeded();
  const seek = await strip.evaluate((s, [room, x, y]) => {
    const c = document.querySelector(`.map-row[data-map="${room}"] .area-map`), r = s.getBoundingClientRect();
    return { x: r.left + (x / +c.dataset.roomWidth) * r.width, y: r.top + (y / +c.dataset.roomHeight) * r.height };
  }, [room, x, y]);
  if (touch) await page.touchscreen.tap(seek.x, seek.y);
  else await page.mouse.click(seek.x, seek.y);
  const canvas = page.locator(`.map-row[data-map="${room}"] .area-map`);
  await canvas.scrollIntoViewIfNeeded();
  await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
  return canvas.evaluate((c, [x, y]) => {
    const r = c.getBoundingClientRect(), scale = Math.min(c.width / +c.dataset.mapWidth, c.height / +c.dataset.mapHeight) * +c.dataset.zoom;
    return {
      x: r.left + ((x - +c.dataset.centerX) * scale + c.width / 2) * r.width / c.width,
      y: r.top + ((y - +c.dataset.centerY) * scale + c.height / 2) * r.height / c.height,
    };
  }, [x, y]);
}
