// SPDX-License-Identifier: AGPL-3.0-or-later
import { chromium } from '@playwright/test';
import assert from 'node:assert/strict';
import { mkdir } from 'node:fs/promises';
const browser = await chromium.launch({headless:true, ...(process.env.CHROME_CHANNEL ? {channel:process.env.CHROME_CHANNEL} : {})});
const errors = [];
await mkdir('test-results', {recursive:true});
try {
  for (const phone of [false, true]) {
    const page = await browser.newPage({viewport: phone ? {width:390,height:844} : {width:1440,height:1000}, hasTouch:phone, isMobile:phone, colorScheme:"light"});
    page.on('pageerror', error => errors.push(error.message));
    await page.addInitScript(() => {
      localStorage.setItem('harmony.nova.tour.v1','seen');
      const NativeWorker = window.Worker;
      window.Worker = class extends NativeWorker {
        constructor(...args) {
          super(...args);
          this.addEventListener('message', ({data}) => {
            if (data.type === 'ready') window.novaRoot = data.state;
          });
        }
      };
    });
    await page.goto(process.env.DEMO_URL || 'http://127.0.0.1:4173/');
    await page.locator('#goal-title').waitFor({state:'visible'});
    await page.waitForFunction(() => !document.querySelector('#goal-title').disabled);
    for (const level of [8,26,42,0]) {
      console.log(`Testing ${phone ? 'phone' : 'desktop'} level ${level}`);
      await page.locator('#goal-title').click();
      assert.equal(await page.locator('#level-worlds .level-card').count(),44);
      await page.locator('.level-card[data-level="43"]').scrollIntoViewIfNeeded();
      const close = await page.locator('#close-level-picker').boundingBox();
      assert.ok(close.y >= 0 && close.y + close.height <= page.viewportSize().height, 'Level picker close stays visible when browsing the last world');
      await page.locator('.level-card[data-level="0"]').scrollIntoViewIfNeeded();
      await page.waitForFunction(() => [...document.querySelectorAll('.level-card img')].filter(img => img.complete && img.naturalWidth).length >= 4);
      const sizes = await page.locator('.level-card img').evaluateAll(images => images.filter(image => image.complete && image.naturalWidth).map(image => [image.naturalWidth,image.naturalHeight]));
      for (const size of sizes) assert.deepEqual(size,[256,96], 'Gallery images must be bounded thumbnails');
      await page.screenshot({path:`test-results/levels-${phone ? 'phone' : 'desktop'}.png`});
      await page.locator(`.level-card[data-level="${level}"]`).click();
      await page.waitForFunction(level => window.novaRoot?.boot_level === level && !document.querySelector('#pause').disabled && !document.querySelector('#goal-title').disabled,level);
      assert.equal((await page.evaluate(() => window.novaRoot)).observation.selected_level,level);
      assert.equal(await page.locator('#goal-title').textContent(), `World ${Math.floor(level/8)+1} – Level ${level%8+1}`);
      assert.equal(await page.locator('#branch-tree [data-search]').count(),1);
      await page.waitForFunction(() => Number(document.querySelector('#attempts').textContent.replaceAll(',','')) >= 4);
      await page.locator('#pause').click();
      // Select the exact starting state in its projected map cell.
      const point = await page.evaluate(() => {
        const o = window.novaRoot.observation, panels=[...document.querySelectorAll(`.map-row[data-map="${o.level}"] canvas`)];
        const roomWidth=Number(panels[0].dataset.roomWidth);
        const worldX=o.x%roomWidth, worldY=o.y+Math.floor(o.x/roomWidth)*224;
        const x=Math.floor(worldX/32)*32+16,y=Math.floor(worldY/32)*32+8;
        const c=panels.find(c=>worldX>=+c.dataset.panelX && worldX<+c.dataset.panelX+c.width && worldY-8>=+c.dataset.panelY && worldY-8<+c.dataset.panelY+c.height);
        c.scrollIntoView({block:'center',behavior:'instant'});
        const r=c.getBoundingClientRect(),w=Number(c.dataset.mapWidth),h=Number(c.dataset.mapHeight);
        const scale=Math.min(c.width/w,c.height/h)*Number(c.dataset.zoom);
        return {x:r.left+((x-Number(c.dataset.centerX))*scale+c.width/2)*r.width/c.width,y:r.top+((y-Number(c.dataset.centerY))*scale+c.height/2)*r.height/c.height};
      });
      if (phone) await page.touchscreen.tap(point.x,point.y); else await page.mouse.click(point.x,point.y);
      await page.waitForFunction(() => !document.querySelector('#inspector').hidden && !document.querySelector('#take-control').disabled);
      const route = await page.locator('#state-list .state').first();
      await route.click();
      await page.waitForFunction(() => ['Exact replay ✓','Original game'].includes(document.querySelector('#verification').textContent) && !document.querySelector('#take-control').disabled);
      await page.locator('#take-control').click();
      await page.waitForTimeout(150);
      await page.locator('#search-here').click();
      await page.waitForFunction(level => window.novaRoot?.boot_level === level && String(window.novaRoot.id)==='1:0' && document.querySelector('#inspector').hidden && !document.querySelector('#pause').disabled,level);
      assert.equal(await page.locator('#branch-tree [data-search]').count(),2);
      if (!phone) {
        await page.locator('[data-search="0"]').hover();
        await page.waitForFunction(() => document.querySelector('#map').dataset.previewSearch === '0');
        assert.equal(await page.locator('#visualization').getAttribute('data-value'),'movement');
        assert.equal(await page.locator('#map').getAttribute('data-overlay'),'movement');
        await page.mouse.move(0,0);
      }
    }
    await page.locator('#theme').click();
    await page.locator('#theme').click();
    assert.equal(await page.evaluate(() => getComputedStyle(document.documentElement).backgroundColor),'rgb(255, 255, 255)');
    await page.locator('#theme').click();
    assert.equal(await page.evaluate(() => getComputedStyle(document.documentElement).backgroundColor),'rgb(28, 28, 28)');
    await page.locator('#goal-title').click();
    await page.locator('#close-level-picker').click();
    assert.equal(await page.locator('#branch-tree [data-search]').count(),2,'Dismissing the level picker keeps the Timeline');
    if (phone) {
      await page.setViewportSize({width:390,height:600});
      await page.locator('#pause').click();
      const canvas=page.locator('.map-row[data-map="0"] canvas');
      await canvas.scrollIntoViewIfNeeded();
      const r=await canvas.boundingBox(),before=Number(await canvas.getAttribute('data-zoom'));
      const cdp=await page.context().newCDPSession(page);
      const x=r.x+r.width/2,y=r.y+r.height/2;
      await cdp.send('Input.dispatchTouchEvent',{type:'touchStart',touchPoints:[{x:x-30,y,id:0},{x:x+30,y,id:1}]});
      for (let i=1;i<=5;i++) await cdp.send('Input.dispatchTouchEvent',{type:'touchMove',touchPoints:[{x:x-30-i*7,y,id:0},{x:x+30+i*7,y,id:1}]});
      await cdp.send('Input.dispatchTouchEvent',{type:'touchEnd',touchPoints:[]});
      await page.waitForFunction(() => Number(document.querySelector('.map-row[data-map="0"] canvas').dataset.zoom)>1.5);
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
  console.log('Desktop/phone level picker: 44 cards, four repeated real warps, exact route replay and gameplay forks, pinch zoom and vertical page scroll passed.');
} finally { await browser.close(); }
