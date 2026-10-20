// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from 'node:assert/strict';
import { chromium } from '@playwright/test';
import { TOUR_KEY } from '../src/tour.js';
import { showOnMap } from './map-point.mjs';
const browser = await chromium.launch({ headless: true, ...(process.env.CHROME_CHANNEL ? { channel: process.env.CHROME_CHANNEL } : {}) });
try {
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 } }), errors = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.addInitScript(key => {
    localStorage.setItem(key, 'seen');
    const Worker = window.Worker;
    window.Worker = class extends Worker {
      constructor(url, options) {
        super(url, options);
        if (!String(url).includes('search-worker')) return;
        let handler;
        Object.defineProperty(this, 'onmessage', { get: () => handler, set: value => { handler = value; } });
        this.addEventListener('message', event => {
          if (event.data.type === 'ready') { window.routeRoot = event.data.state; handler?.(event); }
          else if (event.data.type === 'batch') window.routeBatch = event.data;
          else handler?.(event);
        });
        this.postMessage = (message, ...rest) => {
          if (message.type === 'states') {
            if (message.request > 0) (window.routeRequests ??= []).push(message);
          } else Worker.prototype.postMessage.call(this, message, ...rest);
        };
        window.routeDeliver = data => handler({ data });
      }
    };
  }, TOUR_KEY);
  await page.goto(`${process.env.DEMO_URL || 'http://127.0.0.1:4173'}/?game=famidash&level=0&workers=1&pace=0`);
  await page.waitForFunction(() => window.routeRoot && window.routeBatch && !document.querySelector('#take-control').disabled, null, { timeout: 60000 });
  await page.evaluate(() => routeDeliver({ ...routeBatch, motion: [], finishes: [], exits: [], points: [1, 2].map(id => ({ observation: routeRoot.observation, retained: `0:${id}`, frame: 0 })) }));
  const observation = await page.evaluate(() => routeRoot.observation);
  const target = await showOnMap(page, observation.level, Math.floor((observation.x + 8) / 32) * 32 + 16, Math.floor(observation.y / 32) * 32 + 24);
  await page.mouse.click(target.x, target.y);
  await page.waitForFunction(() => window.routeRequests?.length);
  await page.evaluate(() => {
    window.firstRouteRequest = routeRequests.at(-1);
    routeDeliver({ type: 'states', request: firstRouteRequest.request, states: [{ ...routeRoot, id: '0:2' }], missing: [] });
  });
  await page.waitForFunction(() => document.querySelector('.state.selected')?.dataset.stateId === '0:2');
  assert.equal(await page.locator('.state.selected .state-name').textContent(), 'Path 3');
  await page.locator('.state[data-state-id="0:1"]').click();
  await page.waitForFunction(() => routeRequests.length >= 2);
  await page.evaluate(() => {
    window.pendingRouteRequest = routeRequests.at(-1);
    routeDeliver({ ...routeBatch, motion: [], finishes: [], exits: [], points: Array.from({ length: 10000 }, (_, index) => ({ observation: routeRoot.observation, retained: `0:${index + 3}`, frame: 0 })) });
  });
  await page.locator('.state[data-state-id="0:1"]').click();
  await page.waitForFunction(() => routeRequests.length >= 3);
  await page.evaluate(() => {
    const currentRequest = routeRequests.at(-1);
    routeDeliver({ type: 'states', request: pendingRouteRequest.request, states: [], missing: ['0:1'] });
    routeDeliver({ type: 'states', request: currentRequest.request, states: [{ ...routeRoot, id: '0:1' }], missing: [] });
  });
  await page.waitForFunction(() => document.querySelector('.state.selected')?.dataset.stateId === '0:1');
  assert.equal(await page.locator('.state.selected .state-name').textContent(), 'Path 2', 'A delayed selection keeps its ordinal through churn and an older missing-state response');
  assert.equal(await page.locator('#state-list .state').count(), 13);
  assert.equal(await page.locator('.state[data-state-id="0:2"]').count(), 0, 'The previous playing route is released');
  await page.evaluate(() => { window.staleRouteButton = document.querySelector('.state.selected'); });
  const empty = await showOnMap(page, observation.level, Math.floor((observation.x + 8) / 32) * 32 + 80, Math.floor(observation.y / 32) * 32 + 24);
  await page.mouse.click(empty.x, empty.y);
  assert.equal(await page.locator('#inspector').getAttribute('data-empty'), 'true');
  await page.evaluate(() => staleRouteButton.click());
  assert.equal(await page.locator('#inspector').getAttribute('data-empty'), 'true', 'A stale detached route button cannot revive a selection after the cell changes');
  assert.deepEqual(errors, []);
  console.log('Route selection pins survive delayed state fetch, ten thousand arrivals, and stale missing-state replies.');
} finally { await browser.close(); }
