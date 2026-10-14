// SPDX-License-Identifier: AGPL-3.0-or-later
import { chromium } from '@playwright/test';
import assert from 'node:assert/strict';
import { TOUR_KEY } from '../src/tour.js';
const browser = await chromium.launch({headless: true, ...(process.env.CHROME_CHANNEL ? {channel: process.env.CHROME_CHANNEL} : {})});
const errors = [];
const paths = page => page.evaluate(() => Number(document.querySelector('#attempts').textContent.replaceAll(',', '')));
const growing = (page, n) => page.waitForFunction(n => Number(document.querySelector('#attempts').textContent.replaceAll(',', '')) > n, n, {timeout: 30000});
async function stillDuring(page, ms) {
  await page.waitForTimeout(150);
  const before = await paths(page);
  await page.waitForTimeout(ms);
  assert.equal(await paths(page), before, 'The path counter stays still while a replay plays');
}
try {
  const page = await browser.newPage({viewport: {width: 1440, height: 1000}});
  page.on('pageerror', e => errors.push(e.message));
  await page.addInitScript(key => {
    try { localStorage.setItem(key, 'seen'); } catch {}
    const WorkerClass = window.Worker;
    window.Worker = class extends WorkerClass {
      constructor(...args) {
        super(...args);
        this.addEventListener('message', ({data}) => {
          if (data.type === 'batch')
            for (const point of data.points)
              if (point.retained !== null && !point.observation.loading && point.observation.health > 0)
                (window.tdRetained ??= new Map()).set(point.observation.level, point.observation);
        });
      }
    };
  }, TOUR_KEY);
  await page.goto(`${(process.env.DEMO_URL || 'http://127.0.0.1:4173').replace(/\/$/, '')}/?level=0&workers=1&pace=0`);
  await page.waitForFunction(() => !document.querySelector('#pause').disabled);

  await page.waitForFunction(() => document.querySelector('#inspector').hidden && document.querySelector('#play').textContent === 'Ⅱ Pause', null, {timeout: 60000});
  assert.equal(await page.locator('#pause').getAttribute('aria-label'), 'Pause Search', 'The automatic preview leaves the search running');
  await growing(page, await paths(page));

  await page.locator('#camera-toggle [data-camera="room"]').click();
  await page.waitForFunction(() => window.tdRetained?.has(Number(document.querySelector('#map').dataset.map)), null, {timeout: 60000});
  const point = await page.evaluate(() => {
    const o = window.tdRetained.get(Number(document.querySelector('#map').dataset.map));
    const c = document.querySelector(`.map-row[data-map="${o.level}"] .area-map`), r = c.getBoundingClientRect();
    const scale = Math.min(c.width / Number(c.dataset.mapWidth), c.height / Number(c.dataset.mapHeight)) * Number(c.dataset.zoom);
    const x = Math.floor((o.x + 8) / 32) * 32 + 16, y = Math.floor(o.y / 32) * 32 + 24;
    return {x: r.left + ((x - Number(c.dataset.centerX)) * scale + c.width / 2) * r.width / c.width,
      y: r.top + ((y - Number(c.dataset.centerY)) * scale + c.height / 2) * r.height / c.height};
  });
  await page.mouse.click(point.x, point.y);
  await page.waitForFunction(() => !document.querySelector('#inspector').hidden && !document.querySelector('#play').disabled && document.querySelector('#verification').textContent === 'Exact replay ✓' && Number(document.querySelector('#scrub').max) > 0);
  assert.equal(await page.locator('#play').textContent(), '▶ Replay');
  assert.equal(await page.locator('#pause').getAttribute('aria-label'), 'Pause Search', 'Selecting a path leaves the search running');
  await growing(page, await paths(page));

  await page.locator('#play').click();
  await page.waitForFunction(() => document.querySelector('#play').textContent === 'Ⅱ Pause' && document.querySelector('#pause').getAttribute('aria-label') === 'Resume Search');
  await stillDuring(page, 400);
  assert.ok(Number(await page.locator('#scrub').inputValue()) > 0, 'The replay advances while the search waits');

  await page.locator('#play').click();
  assert.equal(await page.locator('#play').textContent(), '▶ Replay');
  await page.waitForFunction(() => document.querySelector('#pause').getAttribute('aria-label') === 'Pause Search');
  await growing(page, await paths(page));

  await page.locator('#speed').selectOption('12');
  await page.locator('#play').click();
  await page.waitForFunction(() => document.querySelector('#play').textContent === 'Ⅱ Pause' && document.querySelector('#pause').getAttribute('aria-label') === 'Resume Search');
  await page.waitForFunction(() => document.querySelector('#play').textContent === '↺ Replay', null, {timeout: 60000});
  await page.waitForFunction(() => document.querySelector('#pause').getAttribute('aria-label') === 'Pause Search');
  await growing(page, await paths(page));

  await page.locator('#play').click();
  await page.waitForFunction(() => document.querySelector('#play').textContent === 'Ⅱ Pause' && document.querySelector('#pause').getAttribute('aria-label') === 'Resume Search');
  await stillDuring(page, 300);
  await page.locator('#close-inspector').click();
  await page.waitForFunction(() => document.querySelector('#pause').getAttribute('aria-label') === 'Pause Search');
  await growing(page, await paths(page));

  assert.deepEqual(errors, []);
  console.log('A replay pauses the search, and the search resumes when the replay pauses, ends or closes.');
} finally {
  await browser.close();
}
