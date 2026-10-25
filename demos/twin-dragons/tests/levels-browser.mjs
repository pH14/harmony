// SPDX-License-Identifier: AGPL-3.0-or-later
import { chromium } from '@playwright/test';
import assert from 'node:assert/strict';
import { mkdir } from 'node:fs/promises';
import { showOnMap } from './map-point.mjs';
const browser = await chromium.launch({headless:true, ...(process.env.CHROME_CHANNEL ? {channel:process.env.CHROME_CHANNEL} : {})});
const errors = [];
await mkdir('test-results', {recursive:true});
try {
  for (const phone of [false, true]) {
    const page = await browser.newPage({viewport: phone ? {width:390,height:844} : {width:1440,height:1000}, hasTouch:phone, isMobile:phone, colorScheme:"light"});
    page.on('pageerror', error => errors.push(error.message));
    await page.addInitScript(() => {
      localStorage.setItem('harmony.twin-dragons.tour.v1','seen');
      const NativeWorker = window.Worker;
      window.Worker = class extends NativeWorker {
        constructor(...args) {
          super(...args);
          this.addEventListener('message', ({data}) => {
            if (data.type === 'ready') window.tdRoot = data.state;
          });
        }
      };
    });
    await page.goto(`${(process.env.DEMO_URL || 'http://127.0.0.1:4173').replace(/\/$/, '')}/?game=twin-dragons&level=0&pace=0`);
    await page.locator('#new-search').waitFor({state:'visible'});
    await page.waitForFunction(() => !document.querySelector('#new-search').disabled);
    for (const level of [1,2,0]) {
      console.log(`Testing ${phone ? 'phone' : 'desktop'} level ${level}`);
      await page.locator('#new-search').click();
      assert.equal(await page.locator('#level-grid .level-card').count(),3);
      await page.locator('.level-card[data-level="2"]').scrollIntoViewIfNeeded();
      const close = await page.locator('#close-level-picker').boundingBox();
      assert.ok(close.y >= 0 && close.y + close.height <= page.viewportSize().height, 'Level picker close stays visible when browsing the last level');
      await page.locator('.level-card[data-level="0"]').scrollIntoViewIfNeeded();
      await page.waitForFunction(() => [...document.querySelectorAll('.level-card img')].filter(img => img.complete && img.naturalWidth).length >= 3);
      const sizes = await page.locator('.level-card img').evaluateAll(images => images.filter(image => image.complete && image.naturalWidth).map(image => [image.naturalWidth,image.naturalHeight]));
      for (const size of sizes) assert.deepEqual(size,[256,96], 'Gallery images must be bounded thumbnails');
      await page.screenshot({path:`test-results/levels-${phone ? 'phone' : 'desktop'}.png`});
      await page.locator(`.level-card[data-level="${level}"]`).click();
      await page.waitForFunction(level => window.tdRoot?.boot_level === level && !document.querySelector('#pause').disabled && !document.querySelector('#new-search').disabled,level);
      assert.equal((await page.evaluate(() => window.tdRoot)).observation.level,level);
      assert.equal(await page.locator('#level-strip [aria-pressed="true"]').textContent(), `Level ${level+1}`);
      assert.equal(await page.locator('#branch-tree [data-search]').count(),1);
      await page.waitForFunction(() => Number(document.querySelector('#attempts').textContent.replaceAll(',','')) >= 4);
      await page.locator('#pause').click();
      // Select the exact starting state in its projected map cell.
      await page.waitForFunction(() => document.querySelector('.map-wrap').dataset.camera !== 'focus', null, {timeout: 10000});
      const o = await page.evaluate(() => window.tdRoot.observation);
      const point = await showOnMap(page, o.level, Math.floor((o.x+8)/32)*32+16, Math.floor(o.y/32)*32+24, phone);
      if (phone) await page.touchscreen.tap(point.x,point.y); else await page.mouse.click(point.x,point.y);
      await page.waitForFunction(() => !document.querySelector('#inspector').hidden && !document.querySelector('#take-control').disabled);
      const route = await page.locator('#state-list .state').first();
      await route.click();
      await page.waitForFunction(() => ['Exact replay ✓','Original game'].includes(document.querySelector('#verification').textContent) && !document.querySelector('#take-control').disabled);
      await page.locator('#take-control').click();
      await page.waitForTimeout(150);
      await page.locator('#search-here').click();
      await page.waitForFunction(level => window.tdRoot?.boot_level === level && String(window.tdRoot.id)==='1:0' && document.querySelector('#inspector').hidden && !document.querySelector('#pause').disabled,level);
      assert.equal(await page.locator('#branch-tree [data-search]').count(),2);
      if (!phone) {
        await page.locator('[data-search="0"]').hover();
        await page.waitForFunction(() => document.querySelector('#map').dataset.previewSearch === '0');
        assert.equal(await page.locator('#visualization').getAttribute('data-value'),'movement');
        assert.equal(await page.locator('#map').getAttribute('data-overlay'),'movement');
        await page.mouse.move(0,0);
      }
    }
    assert.equal(await page.evaluate(() => getComputedStyle(document.documentElement).backgroundColor),'rgb(28, 28, 28)','Dark is the default theme');
    await page.locator('#theme').click();
    assert.equal(await page.evaluate(() => getComputedStyle(document.documentElement).backgroundColor),'rgb(255, 255, 255)');
    await page.locator('#theme').click();
    assert.equal(await page.evaluate(() => getComputedStyle(document.documentElement).backgroundColor),'rgb(28, 28, 28)');
    await page.locator('#new-search').click();
    await page.locator('#close-level-picker').click();
    assert.equal(await page.locator('#branch-tree [data-search]').count(),2,'Dismissing the level picker keeps the Timeline');
    if (phone) {
      await page.setViewportSize({width:390,height:600});
      await page.locator('#pause').click();
      const canvas=page.locator('.map-row[data-map="0"] .area-map').first();
      await canvas.scrollIntoViewIfNeeded();
      await canvas.evaluate(c=>window.scrollBy({top:c.getBoundingClientRect().bottom-innerHeight+12,behavior:'instant'}));
      const r=await canvas.boundingBox(),before=Number(await canvas.getAttribute('data-zoom'));
      const cdp=await page.context().newCDPSession(page);
      const x=r.x+r.width/2,y=r.y+r.height/2;
      await cdp.send('Input.dispatchTouchEvent',{type:'touchStart',touchPoints:[{x:x-30,y,id:0},{x:x+30,y,id:1}]});
      for (let i=1;i<=5;i++) await cdp.send('Input.dispatchTouchEvent',{type:'touchMove',touchPoints:[{x:x-30-i*7,y,id:0},{x:x+30+i*7,y,id:1}]});
      await cdp.send('Input.dispatchTouchEvent',{type:'touchEnd',touchPoints:[]});
      await page.waitForFunction(() => Number(document.querySelector('.map-row[data-map="0"] .area-map').dataset.zoom)>1.5);
      assert.ok(Number(await canvas.getAttribute('data-zoom'))>before);
      assert.equal(await page.locator('#inspector').isVisible(),false,'Pinching must not select a cell');
      const scrollBefore=await page.evaluate(() => scrollY);
      const after=await canvas.boundingBox(),touch={x:after.x+after.width*.8,y:after.y+after.height*.5,id:0};
      await cdp.send('Input.dispatchTouchEvent',{type:'touchStart',touchPoints:[touch]});
      for (let i=1;i<=5;i++) await cdp.send('Input.dispatchTouchEvent',{type:'touchMove',touchPoints:[{...touch,y:touch.y-i*12}]});
      await cdp.send('Input.dispatchTouchEvent',{type:'touchEnd',touchPoints:[]});
      await page.waitForTimeout(200);
      assert.ok(await page.evaluate(() => scrollY)>scrollBefore,'A zoomed map must still allow vertical page scrolling');
      await page.screenshot({path:'test-results/levels-phone-zoom.png'});
      await cdp.detach();
    }
    await page.close();
  }
  assert.deepEqual(errors,[]);
  console.log('Desktop/phone level picker: 3 cards, three repeated real warps, exact route replay and gameplay forks, pinch zoom and vertical page scroll passed.');
} finally { await browser.close(); }
