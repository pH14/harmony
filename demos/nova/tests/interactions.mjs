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
    try { localStorage.setItem(key, 'seen'); } catch {}
    const WorkerClass = window.Worker;
    window.Worker = class extends WorkerClass {
      constructor(...args) {
        super(...args);
        this.addEventListener('message', ({data}) => {
          if (data.type === 'ready') window.novaReady = {active: data.active, root: data.state};
          if (data.type === 'batch') window.novaMemory = data.snapshot_bytes;
          if (data.type === 'paused') window.novaPaused = (window.novaPaused || 0) + 1;
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
  const room = await page.evaluate(() => window.novaReady.root.observation.level);
  await page.locator(`.map-row[data-map="${room}"] canvas`).scrollIntoViewIfNeeded();
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
  assert.equal(await desktop.locator('#film-title').innerText(), 'History');
  const collapsedFrame = await desktop.locator('#scrub').inputValue();
  await desktop.locator('#close-inspector').click();
  assert.equal(await desktop.locator('#inspector').isVisible(), false);
  assert.equal(await desktop.locator('#history-reopen').isVisible(), true);
  await desktop.locator('#history-reopen').click();
  assert.equal(await desktop.locator('#inspector').isVisible(), true);
  assert.equal(await desktop.locator('#scrub').inputValue(), collapsedFrame);
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
  assert.equal(await desktop.locator('[data-search-node="0"] > .timeline-row #reset').count(),1);
  assert.equal(await desktop.locator('[data-search-node="4"] > .timeline-row [data-delete-search="4"]').count(),1);
  assert.equal(await desktop.locator('[data-delete-search="0"]').count(),0);
  const pauseAck = await desktop.evaluate(() => window.novaPaused || 0);
  await desktop.locator('#pause').click();
  await desktop.waitForFunction(n => window.novaPaused > n,pauseAck);
  const beforeDelete = await desktop.evaluate(() => window.novaMemory);
  await desktop.locator('[data-delete-search="2"]').click();
  await desktop.waitForFunction(() => !document.querySelector('[data-search="2"]') && !document.querySelector('#pause').disabled);
  assert.equal(await desktop.locator('li[data-search-node="1"] > ol > li[data-search-node="3"]').count(),1,'Deleting a parent keeps its independent child search');
  assert.equal(await desktop.locator('#pause').getAttribute('aria-label'),'Resume Search','Deleting an inactive branch preserves pause');
  await desktop.waitForFunction(bytes => window.novaMemory < bytes,beforeDelete);
  await desktop.locator('[data-delete-search="4"]').click();
  await desktop.waitForFunction(() => window.novaReady.active === 3 && !document.querySelector('#pause').disabled);
  assert.equal(await desktop.locator('#pause').getAttribute('aria-label'),'Pause Search','Deleting the active branch resumes its parent');
  await inspectRoot(desktop);
  await play(desktop);
  await desktop.waitForTimeout(100);
  await desktop.locator('#search-here').click();
  await desktop.waitForFunction(() => window.novaReady.active === 5 && !document.querySelector('#pause').disabled);
  assert.equal(await desktop.locator('[data-search="5"]').innerText(),'Branch 5','Deleted branch IDs never alias old states');

  await desktop.close();
  const phone = await open({viewport: {width: 390, height: 844}, isMobile: true, hasTouch: true, deviceScaleFactor: 1});
  assert.ok((await phone.locator('#branches').boundingBox()).height <= 60, 'A single mobile search should use one compact row');
  await inspectRoot(phone, true);
  await play(phone);
  assert.equal(await phone.locator('.state-picker').isVisible(), false, 'Playing hides the retained-route list');
  assert.equal(await phone.locator('.transport').isVisible(), false, 'Replay controls take no space while playing');
  assert.equal(await phone.locator('#sound').isVisible(), true, 'Audio remains available in the header');
  const layout = await phone.evaluate(() => {
    const box = q => document.querySelector(q).getBoundingClientRect();
    const screen=box('.screen'), controller=box('#game-controls'), actions=box('.branch-actions'), header=box('.drawer-top'), sound=box('#sound'), b=box('[data-button="2"]'), a=box('[data-button="1"]');
    return {screenBottom:screen.bottom, controllerTop:controller.top, controllerBottom:controller.bottom, actionsTop:actions.top, soundTop:sound.top, soundBottom:sound.bottom, headerTop:header.top, headerBottom:header.bottom, actionGap:a.left-b.right, overflow:document.documentElement.scrollWidth>innerWidth};
  });
  assert.ok(layout.controllerTop >= layout.screenBottom && layout.controllerTop-layout.screenBottom <= 24);
  assert.ok(layout.actionsTop >= layout.controllerBottom);
  assert.ok(layout.soundTop >= layout.headerTop && layout.soundBottom <= layout.headerBottom);
  assert.ok(layout.actionGap >= 12, 'The A and B buttons need comfortable separation');
  assert.equal(layout.overflow, false);
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
  await phone.locator('[data-delete-search="1"]').scrollIntoViewIfNeeded();
  await phone.locator('[data-delete-search="1"]').tap();
  await phone.waitForFunction(() => !document.querySelector('[data-search="1"]') && !document.querySelector('#pause').disabled);
  assert.equal(await phone.locator('#branch-tree button[data-search]').count(),1);
  await cdp.detach();
  for (const viewport of [{width:320,height:568},{width:780,height:390}]) {
    await phone.setViewportSize(viewport);
    await inspectRoot(phone, true);
    await play(phone);
    const geometry = await phone.evaluate(() => ({
      overflow:document.documentElement.scrollWidth>innerWidth,
      controls:[...document.querySelectorAll('.touch-controls button,.branch-actions button:not([hidden]),#sound')].map(b=>{const r=b.getBoundingClientRect();return {left:r.left,right:r.right,top:r.top,bottom:r.bottom};}),
      width:innerWidth,height:innerHeight,
    }));
    assert.equal(geometry.overflow,false);
    for (const r of geometry.controls) assert.ok(r.left>=0 && r.right<=geometry.width && r.top>=0 && r.bottom<=geometry.height, `Controls must remain visible at ${viewport.width}×${viewport.height}`);
    await phone.screenshot({path:`test-results/interaction-phone-play-${viewport.width}.png`});
    await phone.locator('#discard-branch').tap();
    await phone.waitForFunction(()=>document.querySelector('#inspector').hidden && document.querySelector('#pause').getAttribute('aria-label')==='Pause Search');
  }
  await phone.goto('about:blank');
  await phone.close();
  assert.deepEqual(errors, []);
  console.log('Movement trails, gameplay-only branching, discard/close resumption, depth-four stable layout, two-finger controller capture and single-tap mobile branch switching passed.');
} finally { await browser.close(); }
