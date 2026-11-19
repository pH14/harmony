// SPDX-License-Identifier: AGPL-3.0-or-later
import { chromium } from '@playwright/test';
import assert from 'node:assert/strict';
import { TOUR_KEY } from '../src/tour.js';
import { showOnMap } from './map-point.mjs';
const browser = await chromium.launch({headless: true, ...(process.env.CHROME_CHANNEL ? {channel: process.env.CHROME_CHANNEL} : {})});
const errors = [];
const paths = page => page.evaluate(() => Number(document.querySelector('#attempts').textContent.replaceAll(',', '')));
const growing = (page, n) => page.waitForFunction(n => Number(document.querySelector('#attempts').textContent.replaceAll(',', '')) > n, n, {timeout: 30000});
const MIN_REPLAY_FRAMES = 300;
async function startReplay(page) {
  const seen = await page.evaluate(() => window.tdReplayStarts.length);
  await page.locator('#play').click();
  await page.waitForFunction((seen) => window.tdReplayStarts.length > seen, seen, {timeout: 30000});
  const start = await page.evaluate((seen) => window.tdReplayStarts[seen], seen);
  assert.equal(start.search, 'Resume Search', 'Starting a replay leaves the search paused while the history is open');
  return start;
}
async function replayFromStart(page) {
  await page.evaluate(() => {
    const scrub = document.querySelector('#scrub');
    scrub.value = 0;
    scrub.dispatchEvent(new Event('input', {bubbles: true}));
    scrub.dispatchEvent(new Event('change', {bubbles: true}));
  });
  await page.waitForFunction(() => document.querySelector('#frame-label').textContent.startsWith('FRAME 0 ') && document.querySelector('#play').dataset.state === 'play', null, {timeout: 15000});
  assert.equal(Number(await page.locator('#scrub').inputValue()), 0, 'Scrubbing to the start shows frame 0');
  const start = await startReplay(page);
  assert.equal(start.from, 0, 'The replay starts from frame 0');
  assert.ok(start.to - start.from >= MIN_REPLAY_FRAMES, `The replay runs at least ${MIN_REPLAY_FRAMES} frames: ${JSON.stringify(start)}`);
}
async function stillDuring(page, ms) {
  await page.waitForTimeout(150);
  const samples = await page.evaluate(async (ms) => {
    const out = [];
    for (const end = performance.now() + ms; performance.now() < end; await new Promise((r) => setTimeout(r, 25)))
      out.push({
        paths: Number(document.querySelector('#attempts').textContent.replaceAll(',', '')),
        playing: document.querySelector('#play').dataset.state === 'pause' && document.querySelector('#pause').getAttribute('aria-label') === 'Resume Search',
      });
    return out;
  }, ms);
  const during = samples.filter((sample) => sample.playing);
  assert.ok(during.length >= 4, 'The replay keeps playing while the counter is sampled');
  assert.equal(new Set(during.map((sample) => sample.paths)).size, 1, 'The path counter stays still while a replay plays');
}
const detail = (page, name) => page.evaluate((name) => [...document.querySelectorAll('#details div')].find((d) => d.firstChild.textContent === name)?.lastChild.textContent, name);
async function scrubMarker(page, frame) {
  await page.evaluate((frame) => {
    const scrub = document.querySelector('#scrub');
    scrub.value = frame;
    scrub.dispatchEvent(new Event('input', {bubbles: true}));
  }, frame);
  await page.waitForFunction((frame) => {
    const shown = frame.toLocaleString();
    const field = [...document.querySelectorAll('#details div')].find((d) => d.firstChild.textContent === 'Frame');
    return document.querySelector('#frame-label').textContent.startsWith(`FRAME ${shown} `) && field?.lastChild.textContent === shown;
  }, frame, {timeout: 15000});
  if (await detail(page, 'Level') !== 'Level transition') {
    await page.waitForFunction((frame) => {
      const marked = [...document.querySelectorAll('.area-map')].map((c) => c.dataset.markerFrame).filter(Boolean);
      return marked.length > 0 && marked.every((value) => Number(value) === frame);
    }, frame, {timeout: 15000});
    return 'marker';
  }
  const unmarked = () => [...document.querySelectorAll('.area-map')].every((c) => !c.dataset.markerFrame);
  await page.waitForFunction(unmarked, null, {timeout: 15000});
  for (let sample = 0; sample < 8; sample++, await page.waitForTimeout(50))
    assert.ok(await page.evaluate(unmarked), `Frame ${frame} has no map evidence, so no map shows a replay marker`);
  return 'none';
}
try {
  const page = await browser.newPage({viewport: {width: 1440, height: 1000}});
  page.on('pageerror', e => errors.push(e.message));
  await page.addInitScript(([key, minFrames]) => {
    try { localStorage.setItem(key, 'seen'); } catch {}
    window.tdReplayStarts = [];
    new MutationObserver((records) => {
      if (!records.some((record) => record.target.id === 'play' && record.target.dataset.state === 'pause')) return;
      const scrub = document.querySelector('#scrub');
      window.tdReplayStarts.push({from: Number(scrub.value), to: Number(scrub.max), search: document.querySelector('#pause').getAttribute('aria-label')});
    }).observe(document, {subtree: true, attributes: true, attributeFilter: ['data-state']});
    const WorkerClass = window.Worker;
    window.Worker = class extends WorkerClass {
      constructor(...args) {
        super(...args);
        this.addEventListener('message', ({data}) => {
          if (data.type === 'batch')
            for (const point of data.points)
              if (point.retained !== null && point.frame >= minFrames && !point.observation.loading && point.observation.health > 0)
                (window.tdRetained ??= new Map()).set(point.observation.level, {...point.observation, id: String(point.retained)});
        });
      }
    };
  }, [TOUR_KEY, MIN_REPLAY_FRAMES]);
  await page.goto(`${(process.env.DEMO_URL || 'http://127.0.0.1:4173').replace(/\/$/, '')}/?game=twin-dragons&level=0&workers=1&pace=0`);
  await page.waitForFunction(() => document.querySelector('#pause')?.disabled === false);

  await page.waitForFunction(() => document.querySelector('#inspector').hidden && document.querySelector('#play').dataset.state === 'pause', null, {timeout: 60000});
  assert.equal(await page.locator('#pause').getAttribute('aria-label'), 'Pause Search', 'The automatic preview leaves the search running');
  await growing(page, await paths(page));

  let chosen;
  for (let attempt = 0; ; attempt++) {
    await page.waitForFunction(() => window.tdRetained?.has(Number(document.querySelector('#map').dataset.map)), null, {timeout: 60000});
    const o = chosen = await page.evaluate(() => window.tdRetained.get(Number(document.querySelector('#map').dataset.map)));
    const opened = await showOnMap(page, o.level, Math.floor((o.x + 8) / 32) * 32 + 16, Math.floor(o.y / 32) * 32 + 24)
      .then((point) => page.mouse.click(point.x, point.y))
      .then(() => page.waitForFunction(() => !document.querySelector('#inspector').hidden, null, {timeout: 15000}))
      .then(() => true, () => false);
    if (opened) break;
    assert.ok(attempt < 2, 'Clicking a retained cell on the live map opens the inspector');
  }
  await page.waitForFunction(() => !document.querySelector('#play').disabled && document.querySelector('#verification').textContent === 'Exact replay ✓' && Number(document.querySelector('#scrub').max) > 0);
  if (await page.locator('#film-title').getAttribute('data-state-id') !== chosen.id) {
    await page.locator(`#state-list [data-state-id="${chosen.id}"]`).click();
    await page.waitForFunction((id) => document.querySelector('#film-title').dataset.stateId === id && !document.querySelector('#play').disabled && document.querySelector('#verification').textContent === 'Exact replay ✓', chosen.id);
  }
  assert.equal(await page.locator('#play').getAttribute('data-state'), 'play');
  assert.equal(await page.locator('#pause').getAttribute('aria-label'), 'Resume Search', 'Selecting a path pauses the search');
  await replayFromStart(page);
  await stillDuring(page, 400);
  assert.ok(Number(await page.locator('#scrub').inputValue()) > 0, 'The replay advances while the search waits');

  await page.evaluate(() => {
    const scrub = document.querySelector('#scrub');
    scrub.value = 10;
    scrub.dispatchEvent(new Event('input', {bubbles: true}));
    scrub.dispatchEvent(new Event('change', {bubbles: true}));
  });
  await page.waitForFunction(() => document.querySelector('#play').dataset.state === 'pause' && Number(document.querySelector('#scrub').value) > 10, null, {timeout: 15000});

  await page.locator('#play').click();
  await page.waitForFunction(() => document.querySelector('#play').dataset.state === 'play', null, {timeout: 15000});
  await page.waitForTimeout(400);
  assert.equal(await page.locator('#pause').getAttribute('aria-label'), 'Resume Search', 'Pausing the replay leaves the search paused while the history is open');

  const branches = {marker: 0, none: 0};
  for (const fraction of [0.3, 0.8, 0.5])
    branches[await scrubMarker(page, Math.floor(Number(await page.locator('#scrub').getAttribute('max')) * fraction))]++;

  await page.locator('#speed').selectOption('12');
  await startReplay(page);
  await page.waitForFunction(() => document.querySelector('#play').dataset.state === 'replay', null, {timeout: 60000});
  assert.equal(await page.locator('#pause').getAttribute('aria-label'), 'Resume Search', 'A finished replay leaves the search paused while the history is open');

  await page.locator('#speed').selectOption('1');
  await replayFromStart(page);
  await stillDuring(page, 300);
  await page.locator('#pause').click();
  await page.locator('#pause').click();
  assert.equal(await page.locator('#pause').getAttribute('aria-label'), 'Resume Search');
  await page.locator('#close-inspector').click();
  await page.waitForFunction(() => document.querySelector('#pause').getAttribute('aria-label') === 'Pause Search');
  await growing(page, await paths(page));

  const tape = await browser.newPage({viewport: {width: 1440, height: 1000}});
  tape.on('pageerror', e => errors.push(e.message));
  await tape.addInitScript(key => { try { localStorage.setItem(key, 'seen'); } catch {} }, TOUR_KEY);
  await tape.route('**/replay-pause-tape.json', (route) => route.fulfill({path: 'tests/fixtures/level-two.json', contentType: 'application/json'}));
  await tape.goto(`${(process.env.DEMO_URL || 'http://127.0.0.1:4173').replace(/\/$/, '')}/?game=twin-dragons&workers=1&pace=0&tape=replay-pause-tape.json`);
  await tape.waitForFunction(() => document.querySelector('#play')?.disabled === false && !document.querySelector('#inspector').hidden && Number(document.querySelector('#scrub').max) > 2950, null, {timeout: 60000});
  if (await tape.locator('#play').getAttribute('data-state') === 'pause') await tape.locator('#play').click();
  await tape.waitForFunction(() => document.querySelector('#play').dataset.state !== 'pause');
  for (const frame of [1500, 2700, 2950]) branches[await scrubMarker(tape, frame)]++;
  await tape.close();
  assert.ok(branches.marker > 0 && branches.none > 0, `Scrubs cover frames with and without map evidence: ${JSON.stringify(branches)}`);

  assert.deepEqual(errors, []);
  console.log('Opening a history pauses the search, and the search resumes when the history closes.');
} finally {
  await browser.close();
}
