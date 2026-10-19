// SPDX-License-Identifier: AGPL-3.0-or-later
import { chromium } from '@playwright/test';
import assert from 'node:assert/strict';
import { TOUR_KEY } from '../src/tour.js';

const browser = await chromium.launch({ headless: true,
  ...(process.env.CHROME_CHANNEL ? { channel: process.env.CHROME_CHANNEL } : {}) });
const errors = [];
try {
  for (const [phone, reducedMotion] of [[false, 'no-preference'], [true, 'no-preference'], [false, 'reduce']]) {
    const page = await browser.newPage({ viewport: phone ? { width: 390, height: 844 } : { width: 1440, height: 1000 }, hasTouch: phone, isMobile: phone, reducedMotion });
    page.on('pageerror', (e) => errors.push(e.message));
    await page.addInitScript((key) => {
      localStorage.setItem(key, 'seen');
      const NativeWorker = Worker;
      window.Worker = class extends NativeWorker {
        constructor(...args) {
          super(...args);
          if (!String(args[0]).includes('search-worker')) return;
          window.testWorker = this;
          this.addEventListener('message', (event) => {
            if (window.freezeResults) { event.stopImmediatePropagation(); return; }
            if (event.data.type === 'batch') {
              window.lastBatch = event.data;
              window.freezeResults = true;
              this.postMessage({ type: 'pause' });
              event.stopImmediatePropagation();
            }
            if (event.data.type === 'ready') window.root = event.data.state;
          });
        }
      };
    }, TOUR_KEY);
    await page.goto(`${(process.env.DEMO_URL || 'http://127.0.0.1:4173').replace(/\/$/, '')}/?game=famidash&level=0&workers=2`);
    await page.waitForFunction(() => window.lastBatch && !document.querySelector('#new-search').disabled);
    const freeze = () => page.evaluate(() => {
      window.freezeResults = true;
      window.testWorker.postMessage({ type: 'pause' });
      window.sendTrail = (x, duration = 120) => {
        const room = +document.querySelector('#map').dataset.map;
        const samples = new Uint16Array([0, room, x, 100, 0, duration, room, x + 10, 100, 0]);
        window.testWorker.onmessage({ data: { ...window.lastBatch, active: 0, points: [],
          motion: [samples, samples, samples, samples], finishes: [], exits: [] } });
      };
    });
    await freeze();
    await page.mouse.move(0, 0);
    await page.evaluate(() => window.sendTrail(1600));
    await page.waitForFunction(() => +document.querySelector('#map').dataset.centerX > 1400);
    await page.waitForTimeout(4000);
    const before = +await page.locator('#map').getAttribute('data-center-x');
    assert.ok(+await page.locator('#map').getAttribute('data-swarm-count') >= 4, 'A results gap holds visible positions');
    await page.evaluate(() => window.sendTrail(200, 12));
    await page.waitForTimeout(400);
    assert.ok(+await page.locator('#map').getAttribute('data-center-x') > before - 100, 'A short burst after a gap cannot pull the camera backward');

    await page.evaluate(() => window.sendTrail(200));
    await page.waitForTimeout(200);
    await page.locator('#map').dispatchEvent('pointermove', { pointerType: 'mouse', movementX: 1, movementY: 0 });
    await page.waitForTimeout(2400);
    await page.evaluate(() => window.sendTrail(200));
    await page.waitForTimeout(900);
    assert.ok(+await page.locator('#map').getAttribute('data-center-x') > before - 100, 'A gap during a pointer hold cannot preserve an old retreat timer');

    // A user pan immediately followed by a fresh level must clear the ten-second hold.
    await page.locator('#map').dispatchEvent('wheel', { deltaX: 120, deltaY: 0, deltaMode: 0 });
    await page.locator('#new-search').click();
    await page.evaluate(() => { window.freezeResults = false; window.lastBatch = null; });
    await page.locator('.level-card[data-game="famidash"][data-level="1"]').click();
    await page.waitForFunction(() => window.root?.boot_level === 1 && window.lastBatch && !document.querySelector('#pause').disabled);
    await freeze();
    await page.mouse.move(0, 0);
    await page.evaluate(() => window.sendTrail(1600));
    await page.waitForFunction(() => +document.querySelector('#map').dataset.centerX > 1400, null, { timeout: 4000 });
    await page.close();
  }
  assert.deepEqual(errors, []);
  console.log('Famidash camera: gaps hold visible positions, sparse bursts and pointer holds do not cause retreats, and selecting a new level clears manual holds on desktop, phone and reduced motion.');
} finally { await browser.close(); }
