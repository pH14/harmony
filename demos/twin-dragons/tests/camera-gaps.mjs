// SPDX-License-Identifier: AGPL-3.0-or-later
import { chromium } from '@playwright/test';
import assert from 'node:assert/strict';
import { TOUR_KEY } from '../src/tour.js';

function observeSearch(key) {
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
}

const freeze = (page) => page.evaluate(() => {
  window.freezeResults = true;
  window.testWorker.postMessage({ type: 'pause' });
  window.sendTrail = (x, duration = 120) => {
    const room = +document.querySelector('#map').dataset.map;
    const samples = new Uint16Array([0, room, x, 100, 0, duration, room, x + 10, 100, 0]);
    window.testWorker.onmessage({ data: { ...window.lastBatch, active: 0, points: [],
      motion: [samples, samples, samples, samples], finishes: [], exits: [] } });
  };
});

const url = `${(process.env.DEMO_URL || 'http://127.0.0.1:4173').replace(/\/$/, '')}/?game=famidash&level=0&workers=2`;

const browser = await chromium.launch({ headless: true,
  ...(process.env.CHROME_CHANNEL ? { channel: process.env.CHROME_CHANNEL } : {}) });
const errors = [];
try {
  for (const [phone, reducedMotion] of [[false, 'no-preference'], [true, 'no-preference'], [false, 'reduce']]) {
    const page = await browser.newPage({ viewport: phone ? { width: 390, height: 844 } : { width: 1440, height: 1000 }, hasTouch: phone, isMobile: phone, reducedMotion });
    page.on('pageerror', (e) => errors.push(e.message));
    await page.addInitScript(observeSearch, TOUR_KEY);
    await page.goto(url);
    await page.waitForFunction(() => window.lastBatch && !document.querySelector('#new-search').disabled);
    await freeze(page);
    await page.mouse.move(0, 0);
    await page.evaluate(() => window.sendTrail(1600));
    await page.waitForFunction(() => +document.querySelector('#map').dataset.centerX > 1400);
    await page.waitForTimeout(4000);
    const before = +await page.locator('#map').getAttribute('data-center-x');
    assert.equal(+await page.locator('#map').getAttribute('data-swarm-count'), 0, 'Expired attempts disappear during a results gap');
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
    const panFrom = +await page.locator('#map').getAttribute('data-center-x');
    await page.locator('#map').dispatchEvent('wheel', { deltaX: 120, deltaY: 0, deltaMode: 0 });
    assert.ok(+await page.locator('#map').getAttribute('data-center-x') > panFrom + 1, 'The wheel actually moves the camera and creates a manual hold');
    await page.evaluate(() => { window.panAt = performance.now(); });
    await page.locator('#new-search').click();
    await page.evaluate(() => { window.freezeResults = false; window.lastBatch = null; });
    await page.locator('.level-card[data-game="famidash"][data-level="1"]').click();
    await page.waitForFunction(() => window.root?.boot_level === 1 && window.lastBatch && !document.querySelector('#pause').disabled);
    await freeze(page);
    assert.ok(await page.evaluate(() => performance.now() - window.panAt < 5000), 'The old ten-second hold would still block the next four-second check');
    await page.mouse.move(0, 0);
    await page.evaluate(() => window.sendTrail(1600));
    await page.waitForFunction(() => +document.querySelector('#map').dataset.centerX > 1400, null, { timeout: 4000 });
    await page.close();
  }

  const timed = await browser.newPage({ viewport: { width: 1440, height: 1000 }, reducedMotion: 'reduce' });
  timed.on('pageerror', (e) => errors.push(e.message));
  await timed.clock.install();
  await timed.addInitScript(observeSearch, TOUR_KEY);
  await timed.goto(url);
  await timed.waitForFunction(() => window.lastBatch && !document.querySelector('#new-search').disabled);
  await freeze(timed);
  await timed.mouse.move(0, 0);
  await timed.clock.pauseAt(await timed.evaluate(() => Date.now() + 100));
  const initial = +await timed.locator('#map').getAttribute('data-center-x');
  const anchor = Math.min(+await timed.locator('#map').getAttribute('data-map-width') - 1000, initial + 2000);
  assert.ok(anchor > initial + 500);
  await timed.evaluate((x) => window.sendTrail(x, 180), anchor);
  for (let frame = 0; frame < 200 && +await timed.locator('#map').getAttribute('data-center-x') < initial + 500; frame++) await timed.clock.runFor(16);
  assert.ok(+await timed.locator('#map').getAttribute('data-center-x') > initial + 500, 'Locate a reduced-motion steering frame');
  // Keep the same phase, expire the opening attempt, and straddle successive steering frames.
  await timed.clock.runFor(4000);
  assert.equal(+await timed.locator('#map').getAttribute('data-swarm-count'), 0);
  const steady = +await timed.locator('#map').getAttribute('data-center-x');
  await timed.clock.runFor(1900);
  await timed.evaluate(() => window.sendTrail(200, 12));
  await timed.clock.runFor(200);
  await timed.clock.runFor(1800);
  await timed.evaluate(() => window.sendTrail(200, 12));
  await timed.clock.runFor(200);
  assert.ok(+await timed.locator('#map').getAttribute('data-center-x') > steady - 100, 'Reduced motion evaluates the empty frames between successive steering frames');
  await timed.evaluate(() => window.sendTrail(200, 36000));
  await timed.clock.runFor(200);
  await timed.evaluate(() => document.dispatchEvent(new Event('visibilitychange')));
  await timed.clock.fastForward(60000);
  assert.ok(+await timed.locator('#map').getAttribute('data-center-x') > steady - 100, 'Returning after a visibility gap cannot reuse stale retreat evidence');
  await timed.close();
  assert.deepEqual(errors, []);
  console.log('Famidash camera: empty gaps, sparse bursts and pointer holds do not cause retreats, and selecting a new level clears manual holds on desktop, phone and reduced motion.');
} finally { await browser.close(); }
