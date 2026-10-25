// SPDX-License-Identifier: AGPL-3.0-or-later
import { chromium, firefox } from '@playwright/test';
import assert from 'node:assert/strict';
import { TOUR_KEY } from '../src/tour.js';

const engine = process.env.BROWSER_ENGINE === 'firefox' ? firefox : chromium;
const browser = await engine.launch({headless: true, ...(engine === chromium && process.env.CHROME_CHANNEL ? {channel: process.env.CHROME_CHANNEL} : {})});
const url = (process.env.DEMO_URL || 'http://127.0.0.1:4173').replace(/\/$/, '');
const errors = [];
const SWITCHES = [['famidash', 'Famidash'], ['tobu-tobu-girl', 'Tobu Tobu Girl'], ['famidash', 'Famidash'], ['tobu-tobu-girl', 'Tobu Tobu Girl'], ['twin-dragons', 'Twin Dragons']];

try {
  const page = await browser.newPage({viewport: {width: 1440, height: 1000}});
  page.on('pageerror', (e) => errors.push(e.message));
  await page.addInitScript((key) => {
    try { localStorage.setItem(key, 'seen'); } catch {}
    const Native = window.Worker;
    window.liveWorkers = 0;
    window.Worker = class extends Native {
      constructor(...args) {
        super(...args);
        window.liveWorkers++;
        this.ended = false;
      }
      terminate() {
        if (!this.ended) window.liveWorkers--;
        this.ended = true;
        super.terminate();
      }
    };
    window.liveListeners = 0;
    for (const target of [window, document]) {
      const add = target.addEventListener.bind(target), remove = target.removeEventListener.bind(target), seen = new Map();
      target.addEventListener = (type, listener, options) => {
        const key = `${type}:${typeof options === 'object' ? !!options.capture : !!options}`;
        if (!seen.has(listener)) seen.set(listener, new Set());
        if (!seen.get(listener).has(key)) { seen.get(listener).add(key); window.liveListeners++; }
        add(type, listener, options);
      };
      target.removeEventListener = (type, listener, options) => {
        const key = `${type}:${typeof options === 'object' ? !!options.capture : !!options}`;
        if (seen.get(listener)?.delete(key)) window.liveListeners--;
        remove(type, listener, options);
      };
    }
    window.bootMarker = 'same document';
  }, TOUR_KEY);

  await page.goto(`${url}/?game=twin-dragons`);
  await page.locator('#level-picker[open]').waitFor({timeout: 60000});
  const settled = async (game) => {
    await page.waitForFunction((game) => document.documentElement.dataset.game === game && !document.documentElement.dataset.switching && document.querySelector('#level-picker[open]'), game, {timeout: 60000});
  };
  await settled('twin-dragons');
  await page.locator('#level-grid button').first().click();
  await page.waitForFunction(() => Number(document.querySelector('#attempts').textContent.replaceAll(',', '')) > 0, null, {timeout: 60000});
  const first = await page.evaluate(() => ({workers: window.liveWorkers, listeners: window.liveListeners}));
  assert.ok(first.workers > 0, 'A running search owns workers');

  const loads = [];
  page.on('load', () => loads.push(page.url()));
  for (const [id, title] of SWITCHES) {
    await page.locator('#new-search').click();
    await page.locator('#level-picker[open]').waitFor();
    await page.locator('#game-choice .game-row', {hasText: title}).first().click();
    await settled(id);
    assert.equal(await page.evaluate(() => window.bootMarker), 'same document', `Switching to ${id} keeps the document`);
    assert.equal(new URL(page.url()).searchParams.get('game'), id, `The address names ${id}`);
    await page.locator('#level-grid button').first().click();
    await page.waitForFunction(() => Number(document.querySelector('#attempts').textContent.replaceAll(',', '')) > 0, null, {timeout: 60000});
  }
  assert.deepEqual(loads, [], 'No game switch loads a new page');
  await page.locator('#new-search').click();
  await page.locator('#game-choice .game-row', {hasText: 'Famidash'}).first().click();
  await settled('famidash');
  await page.goBack();
  await settled('twin-dragons');
  const after = await page.evaluate(() => ({workers: window.liveWorkers, listeners: window.liveListeners}));
  assert.ok(after.workers <= first.workers + 1, `Workers do not accumulate (${first.workers} then ${after.workers})`);
  assert.ok(after.listeners <= first.listeners + 2, `Listeners do not accumulate (${first.listeners} then ${after.listeners})`);
  assert.deepEqual(errors, []);
  console.log(`Six in-page game switches kept one document; workers ${first.workers} to ${after.workers}, listeners ${first.listeners} to ${after.listeners}.`);
} finally {
  await browser.close();
}
