// SPDX-License-Identifier: AGPL-3.0-or-later
import { chromium } from '@playwright/test';
import assert from 'node:assert/strict';
import { mkdir } from 'node:fs/promises';
import { TOUR_KEY } from '../src/tour.js';
const browser = await chromium.launch({headless: true, ...(process.env.CHROME_CHANNEL ? {channel: process.env.CHROME_CHANNEL} : {})});
const errors = [];
async function open(options) {
  const page = await browser.newPage(options);
  page.on('pageerror', e => errors.push(e.message));
  await page.addInitScript(key => {
    localStorage.setItem(key, 'seen');
    const WorkerClass = window.Worker;
    window.Worker = class extends WorkerClass {
      constructor(...args) {
        super(...args);
        this.addEventListener('message', ({data}) => {
          if (data.type === 'ready') window.novaReady = {active: data.active, root: data.state};
        });
      }
    };
  }, TOUR_KEY);
  await page.goto(process.env.DEMO_URL || 'http://127.0.0.1:4173/');
  await page.waitForFunction(() => window.novaReady && !document.querySelector('#pause').disabled);
  return page;
}
async function inspectRoot(page, touch = false) {
  if (await page.locator('#pause').getAttribute('aria-label') === 'Pause Search') await page.locator('#pause').click();
  const point = await page.evaluate(() => {
    const o = window.novaReady.root.observation;
    const c = document.querySelector(`.map-row[data-map="${o.level}"] canvas`), r = c.getBoundingClientRect();
    const scale = Math.min(c.width / Number(c.dataset.mapWidth), c.height / Number(c.dataset.mapHeight)) * Number(c.dataset.zoom);
    const x = Math.floor(o.x / 32) * 32 + 16, y = Math.floor(o.y / 32) * 32 + 8;
    return {room: o.level, x: r.left + ((x - Number(c.dataset.centerX)) * scale + c.width / 2) * r.width / c.width,
      y: r.top + ((y - Number(c.dataset.centerY)) * scale + c.height / 2) * r.height / c.height};
  });
  if (touch) await page.touchscreen.tap(point.x, point.y);
  else await page.mouse.click(point.x, point.y);
  await page.waitForFunction(() => !document.querySelector('#inspector').hidden && !document.querySelector('#take-control').disabled);
  assert.equal(await page.locator("#visualization").getAttribute("data-value"), 'movement');
  await page.waitForFunction(() => [...document.querySelectorAll('.area-map')].some(c => Number(c.dataset.tracePoints) > 0 && c.dataset.markerFrame !== ''));
  assert.equal(await page.locator('#search-here').isVisible(), false);
  assert.equal(await page.locator('#discard-branch').isVisible(), false);
}
async function play(page) {
  await page.locator('#take-control').click();
  assert.equal(await page.locator('#take-control').isVisible(), false);
  assert.equal(await page.locator('#search-here').isVisible(), true);
  assert.equal(await page.locator('#discard-branch').isVisible(), true);
  const widths = await page.locator('.branch-actions').evaluate(el => [...el.querySelectorAll('button:not([hidden])')].map(b => b.getBoundingClientRect().width));
  assert.equal(widths.length, 2);
  assert.ok(Math.abs(widths[0] - widths[1]) < 1);
}
try {
  await mkdir('test-results', {recursive: true});
  const desktop = await open({viewport: {width: 1920, height: 1100}});
  const width = await desktop.locator('.exploration').evaluate(e => e.getBoundingClientRect().width);
  assert.ok(width > 1650, 'Large windows should use their available horizontal space');
  await inspectRoot(desktop);
  await play(desktop);
  await desktop.keyboard.down('ArrowRight');
  await desktop.waitForTimeout(160);
  await desktop.keyboard.up('ArrowRight');
  await desktop.locator('#close-inspector').click();
  await desktop.waitForFunction(() => document.querySelector('#pause').getAttribute('aria-label') === 'Pause Search' && document.querySelector('#inspector').hidden);
  assert.equal(await desktop.locator('#branch-tree button[data-search]').count(), 1, 'Closing a draft must resume without admitting a search');
  for (let id = 1; id <= 4; id++) {
    await inspectRoot(desktop);
    await play(desktop);
    await desktop.keyboard.down('ArrowRight');
    await desktop.waitForTimeout(100);
    await desktop.keyboard.up('ArrowRight');
    await desktop.locator('#search-here').click();
    await desktop.waitForFunction(id => window.novaReady.active === id && document.querySelector('#inspector').hidden && !document.querySelector('#pause').disabled, id);
    assert.equal(await desktop.locator(`li[data-search-node="${id-1}"] > ol > li[data-search-node="${id}"]`).count(), 1);
    assert.ok(Math.abs(await desktop.locator('.exploration').evaluate(e => e.getBoundingClientRect().width) - width) < 1, 'Tree depth must not resize the maps');
  }
  assert.equal(await desktop.locator('[data-search="4"] small').innerText(), 'from Branch 3');
  await desktop.screenshot({path: 'test-results/interaction-deep-tree.png'});
  await desktop.close();
  const phone = await open({viewport: {width: 390, height: 844}, isMobile: true, hasTouch: true, deviceScaleFactor: 1});
  await inspectRoot(phone, true);
  await play(phone);
  const cdp = await phone.context().newCDPSession(phone);
  const right = await phone.getByRole('button', {name: 'Move right', exact: true}).boundingBox();
  const jump = await phone.getByRole('button', {name: 'Jump', exact: true}).boundingBox();
  const touches = [right, jump].map((r, id) => ({x:r.x+r.width/2, y:r.y+r.height/2, id}));
  const before = await phone.locator('#scrub').inputValue();
  await cdp.send('Input.dispatchTouchEvent', {type:'touchStart', touchPoints:touches});
  await phone.waitForTimeout(350);
  assert.equal(await phone.locator('.touch-controls .held').count(), 2);
  assert.ok(Number(await phone.locator('#scrub').inputValue()) > Number(before));
  assert.equal(await phone.evaluate(() => getSelection().toString()), '', 'Holding two controller buttons must not select page text');
  await cdp.send('Input.dispatchTouchEvent', {type:'touchMove', touchPoints:touches.map(t => ({...t,y:t.y-60}))});
  await cdp.send('Input.dispatchTouchEvent', {type:'touchEnd', touchPoints:[]});
  assert.equal(await phone.locator('.touch-controls .held').count(), 0, 'Captured fingers release even outside their buttons');
  await phone.screenshot({path:'test-results/interaction-phone-play.png'});
  await phone.locator('#discard-branch').tap();
  await phone.waitForFunction(() => document.querySelector('#pause').getAttribute('aria-label') === 'Pause Search' && document.querySelector('#inspector').hidden);
  assert.equal(await phone.locator('#branch-tree button[data-search]').count(), 1);
  await inspectRoot(phone, true);
  await play(phone);
  await phone.waitForTimeout(100);
  await phone.locator('#search-here').tap();
  await phone.waitForFunction(() => window.novaReady.active === 1 && document.querySelector('#inspector').hidden && !document.querySelector('#pause').disabled);
  for (const id of [0, 1, 0]) {
    await phone.locator(`[data-search="${id}"]`).scrollIntoViewIfNeeded();
    await phone.locator(`[data-search="${id}"]`).tap();
    await phone.waitForFunction(id => window.novaReady.active === id && document.querySelector('#pause').getAttribute('aria-label') === 'Pause Search' && !document.querySelector('#pause').disabled, id);
    assert.equal(await phone.locator('#branches').getAttribute('data-preview'), null, 'A first tap switches instead of opening a hover preview');
  }
  await cdp.detach();
  await phone.close();
  assert.deepEqual(errors, []);
  console.log('Movement trails, gameplay-only branching, discard/close resumption, depth-four stable layout, two-finger controller capture and single-tap mobile branch switching passed.');
} finally { await browser.close(); }
