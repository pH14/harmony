// SPDX-License-Identifier: AGPL-3.0-or-later
import { chromium } from '@playwright/test';
import assert from 'node:assert/strict';
import { mkdir } from 'node:fs/promises';
const browser = await chromium.launch({headless: true, ...(process.env.CHROME_CHANNEL ? {channel: process.env.CHROME_CHANNEL} : {})});
const url = process.env.DEMO_URL || 'http://127.0.0.1:4173';
const errors = [];
async function ready(page, step) {
  await page.waitForFunction((step) => {
    const tour = document.querySelector('#guided-tour');
    return tour.open && Number(tour.dataset.step) === step && tour.querySelector('.tour-card').getAttribute('aria-busy') === 'false' && document.querySelector('#tour-next').textContent !== 'Try again';
  }, step, {timeout: 60000});
  await page.waitForTimeout(100);
  const layout = await page.evaluate(() => {
    const card = document.querySelector('.tour-card').getBoundingClientRect();
    return [...document.querySelectorAll('#tour-rings rect')].map((ring) => {
      const r = ring.getBoundingClientRect();
      return Math.max(0, Math.min(card.right, r.right) - Math.max(card.left, r.left)) * Math.max(0, Math.min(card.bottom, r.bottom) - Math.max(card.top, r.top));
    });
  });
  assert.ok(layout.length > 0, `Step ${step} needs a visible highlight`);
  assert.ok(layout.every((area) => area === 0), `No spotlight may be covered by the callout at step ${step}: ${layout}`);
  await exposed(page.locator('#tour-next'));
}
async function exposed(locator, spotlight = false) {
  const result = await locator.evaluate((node, spotlight) => {
    const r = node.getBoundingClientRect(), x = r.left + r.width / 2, y = r.top + r.height / 2;
    const hit = document.elementFromPoint(x, y);
    const lit = [...document.querySelectorAll('#tour-rings rect')].some((hole) => {
      const h = hole.getBoundingClientRect();
      return h.left <= x && h.right >= x && h.top <= y && h.bottom >= y;
    });
    return {id: node.id || node.dataset.map || node.textContent, rect: {top:r.top,bottom:r.bottom,left:r.left,right:r.right}, viewport: {width:innerWidth,height:innerHeight}, hit:hit?.id || hit?.className || hit?.tagName, reachable: hit === node || node.contains(hit), lit: !spotlight || lit};
  }, spotlight);
  if (!result.reachable || !result.lit) await locator.page().screenshot({path:'test-results/tour-mobile-failure.png'});
  assert.ok(result.rect.top >= 0 && result.rect.bottom <= result.viewport.height && result.rect.left >= 0 && result.rect.right <= result.viewport.width, `Control inside viewport: ${JSON.stringify(result)}`);
  assert.ok(result.reachable, `Control must receive real taps: ${JSON.stringify(result)}`);
  assert.ok(result.lit, `Control must be spotlighted: ${JSON.stringify(result)}`);
}
async function opening(page) {
  assert.equal(await page.locator('#tour-title').innerText(), 'Welcome!');
  assert.equal(await page.locator('#tour-copy p').count(), 2);
  assert.equal(await page.locator('#visualization').getAttribute('data-value'), 'movement');
  const openingMap = await page.locator('.area-map').evaluateAll((maps) => maps.find((map) => {
    const r = map.getBoundingClientRect();
    return [...document.querySelectorAll('#tour-rings rect')].some((ring) => {
      const h = ring.getBoundingClientRect();
      return Math.min(r.right,h.right)>Math.max(r.left,h.left) && Math.min(r.bottom,h.bottom)>Math.max(r.top,h.top);
    });
  })?.dataset.map);
  await next(page, 1);
  assert.equal(await page.locator('#tour-title').innerText(), 'Exploration');
  assert.equal(await page.locator('#visualization').getAttribute('data-value'), 'movement');
  const sameMapLit = await page.locator(`.area-map[data-map="${openingMap}"]`).evaluate((map) => {
    const r=map.getBoundingClientRect();
    return [...document.querySelectorAll('#tour-rings rect')].some((ring) => {
      const h=ring.getBoundingClientRect();
      return Math.min(r.right,h.right)>Math.max(r.left,h.left) && Math.min(r.bottom,h.bottom)>Math.max(r.top,h.top);
    });
  });
  assert.equal(sameMapLit,true,'Welcome and Exploration spotlight the same map');
}
async function next(page, step) { await page.locator('#tour-next').tap(); await ready(page, step); }
async function play(page) {
  await exposed(page.locator('#take-control'), true);
  await page.locator('#take-control').tap();
  await page.waitForFunction(() => document.querySelector('#inspector').classList.contains('controlling'));
  await page.waitForTimeout(100);
  for (const button of await page.locator('.touch-controls button').all()) await exposed(button, true);
  await exposed(page.locator('#sound'));
  await exposed(page.locator('#search-here'));
  await exposed(page.locator('#discard-branch'));
  const start = Number(await page.locator('#scrub').inputValue());
  const right = await page.locator('[data-button="128"]').boundingBox();
  const cdp = await page.context().newCDPSession(page);
  await cdp.send('Input.dispatchTouchEvent', {type:'touchStart', touchPoints:[{x:right.x+right.width/2,y:right.y+right.height/2,id:1}]});
  await page.waitForTimeout(350);
  await cdp.send('Input.dispatchTouchEvent', {type:'touchEnd', touchPoints:[]});
  await cdp.detach();
  assert.ok(Number(await page.locator('#scrub').inputValue()) > start, 'The actual game runs during the tour');
  assert.equal(await page.evaluate(() => getSelection().toString()), '');
}
try {
  await mkdir('test-results', {recursive: true});
  for (const viewport of [{width:320,height:568}, {width:390,height:844}, {width:844,height:390}]) {
    const page = await browser.newPage({viewport,isMobile:true,hasTouch:true,reducedMotion:'reduce'});
    page.on('pageerror', (e) => errors.push(e.message));
    await page.addInitScript(() => {
      const RealWorker = window.Worker;
      window.Worker = class extends RealWorker {
        postMessage(data, ...rest) {
          if (data.type === 'fork' && window.tourForkDelay) setTimeout(() => super.postMessage(data,...rest),window.tourForkDelay);
          else super.postMessage(data,...rest);
        }
      };
    });
    await page.goto(url);
    await ready(page, 0);
    await opening(page);
    await next(page, 2);
    assert.equal(await page.locator('#tour-ping').getAttribute('visibility'), 'visible');
    assert.ok(await page.locator('#tour-ping-bounds rect').count() > 0);
    assert.equal(await page.locator('.tour-ping-wave').first().evaluate((node) => getComputedStyle(node).animationName), 'none', 'Reduced motion keeps the cell emphasis static');
    const header = await page.evaluate(() => { const title=document.querySelector('#tour-title').getBoundingClientRect(), skip=document.querySelector('#tour-skip').getBoundingClientRect(); return Math.min(title.bottom,skip.bottom)-Math.max(title.top,skip.top); });
    assert.ok(header > 0,'Skip tour shares the title row');
    assert.equal(await page.locator('#tour-progress,#tour-try').count(),0);
    const state = page.locator('#state-list .state:not(.selected)').first();
    const chosen = await state.getAttribute('data-state-id');
    await state.tap();
    await page.waitForFunction((id) => document.querySelector('#film-title').dataset.stateId === id && !document.querySelector('#take-control').disabled, chosen);
    await ready(page, 2);
    await exposed(state, true);
    await next(page, 3);
    await exposed(page.locator('#play'), true);
    await exposed(page.locator('#scrub'), true);
    await exposed(page.locator('#sound'), true);
    const audioLayout = await page.evaluate(() => {
      const replay=document.querySelector('#play').getBoundingClientRect(), sound=document.querySelector('#sound').getBoundingClientRect();
      return {parent:document.querySelector('#sound').parentElement.className, overlap:Math.min(replay.bottom,sound.bottom)-Math.max(replay.top,sound.top), gap:sound.left-replay.right};
    });
    assert.equal(audioLayout.parent,'replay-actions');
    assert.ok(audioLayout.overlap > 0 && audioLayout.gap >= 0 && audioLayout.gap <= 10,'Audio sits beside Replay, rather than floating above it');
    const map = await page.locator('.area-map').evaluateAll((maps) => maps.find((c) => c.dataset.markerFrame && c.dataset.markerFrame !== '')?.dataset.map);
    await exposed(page.locator(`.area-map[data-map="${map}"]`), true);
    await page.screenshot({path:`test-results/tour-mobile-${viewport.width}-replay.png`});
    await page.locator('#play').tap();
    await page.waitForFunction(() => Number(document.querySelector('#scrub').value) > 10);
    await page.locator('#play').tap();
    const scrub = await page.locator('#scrub').boundingBox();
    await page.touchscreen.tap(scrub.x + scrub.width*.6, scrub.y+scrub.height/2);
    await page.waitForFunction(() => document.querySelector('#frame-label').textContent.startsWith('FRAME ' + Number(document.querySelector('#scrub').value).toLocaleString('en-US')));
    await next(page, 4);
    await play(page);
    await page.screenshot({path:`test-results/tour-mobile-${viewport.width}-controller.png`});
    if (viewport.width === 390) {
      await page.setViewportSize({width:844,height:390});
      await ready(page, 4);
      for (const button of await page.locator('.touch-controls button').all()) await exposed(button, true);
      await page.setViewportSize(viewport);
      await ready(page, 4);
    }
    await next(page, 5);
    assert.equal(await page.locator('#take-control').isVisible(), false);
    assert.equal(await page.locator('#game-controls').isVisible(), true);
    for (const button of await page.locator('.touch-controls button').all()) await exposed(button);
    assert.equal(await page.locator('#tour-copy p').first().innerText(), 'Decide whether you want Harmony to start a new search fresh from where you just left off.');
    await exposed(page.locator('#tour-back'));
    await exposed(page.locator('#tour-next'));
    await exposed(page.locator('#search-here'), true);
    await exposed(page.locator('#discard-branch'), true);
    await exposed(page.locator('button[data-search="0"]'));
    assert.equal(await page.locator('#tour-rings rect').count(),1,'The branch-decision step focuses on Branch/Discard only');
    const litActions = await page.locator('.branch-actions').screenshot();
    await page.locator('.tour-shade').evaluate(e => e.style.visibility='hidden');
    assert.deepEqual(await page.locator('.branch-actions').screenshot(),litActions,'Branch/Discard must actually be undimmed, not merely outlined');
    await page.locator('.tour-shade').evaluate(e => e.style.visibility='');
    await page.screenshot({path:`test-results/tour-mobile-${viewport.width}-branch.png`});
    await page.locator('#discard-branch').tap();
    await page.waitForFunction(() => document.querySelector('#inspector').hidden && document.querySelector('#pause').getAttribute('aria-label') === 'Pause Search');
    await ready(page, 5);
    await page.locator('#tour-back').tap();
    await ready(page, 4);
    await play(page);
    await next(page, 5);
    await page.locator('#search-here').tap();
    await ready(page, 6);
    await page.waitForFunction(() => document.querySelector('button[data-search="1"][aria-pressed="true"]') && document.querySelector('#inspector').hidden);
    await page.locator('button[data-search="0"]').scrollIntoViewIfNeeded();
    await exposed(page.locator('button[data-search="0"]'), true);
    await page.locator('button[data-search="0"]').tap();
    await page.waitForFunction(() => document.querySelector('button[data-search="0"][aria-pressed="true"]'));
    await exposed(page.locator('#reset'), true);
    await page.locator('#reset').tap();
    await page.waitForFunction(() => document.querySelectorAll('button[data-search]').length === 1 && Number(document.querySelector('#attempts').textContent.replaceAll(',', '')) >= 30 && document.querySelector('#inspector').hidden);
    assert.equal(await page.locator('#tour-next').innerText(), "Let’s go explore!");
    await page.locator('#tour-next').tap();
    assert.equal(await page.locator('[inert]').count(),0);
    assert.equal(await page.locator('body').evaluate((e)=>e.classList.contains('tour-phone')),false);
    assert.equal(await page.locator('body').getAttribute('data-tour-step'),null);
    const finishedPaths = await page.locator('#attempts').innerText();
    await page.waitForFunction((n) => Number(document.querySelector('#attempts').textContent.replaceAll(',','')) > Number(n.replaceAll(',','')), finishedPaths);
    if (viewport.width === 390) {
      // Visitors can choose an exit directly during gameplay, before pressing Next.
      await page.locator('#pause').tap();
      await page.waitForFunction(() => document.querySelector('#pause').getAttribute('aria-label') === 'Resume Search');
      await page.locator('#tour-open').tap();
      await ready(page, 0);
      await opening(page);
      for (let step=2;step<=4;step++) await next(page,step);
      await play(page);
      await page.locator('#discard-branch').tap();
      await ready(page,5);
      assert.equal(await page.locator('#tour-title').innerText(),'To branch or not to branch','Early discard must visit the branch-decision step');
      await page.locator('#tour-back').tap();
      await ready(page,4);
      await play(page);
      await page.locator('#close-inspector').tap();
      await ready(page,5);
      assert.equal(await page.locator('#tour-title').innerText(),'To branch or not to branch','Collapsing gameplay must visit the branch-decision step');
      await page.locator('#tour-back').tap();
      await ready(page,4);
      await play(page);
      await page.evaluate(() => window.tourForkDelay = 1000);
      await page.locator('#search-here').tap();
      await next(page,5);
      await page.waitForFunction(() => document.querySelector('button[data-search="1"][aria-pressed="true"]') && document.querySelector('#inspector').hidden);
      assert.equal(await page.locator('#tour-title').innerText(),'To branch or not to branch','Early admission must not skip straight to Searches');
      await next(page,6);
      await page.locator('#pause').tap();
      await page.waitForFunction(() => document.querySelector('#pause').getAttribute('aria-label') === 'Resume Search');
      const paths = await page.locator('#attempts').innerText();
      await page.locator('#tour-next').tap();
      await page.waitForFunction((n) => document.querySelector('#pause').getAttribute('aria-label') === 'Pause Search' && Number(document.querySelector('#attempts').textContent.replaceAll(',','')) > Number(n.replaceAll(',','')),paths);
      assert.equal(await page.locator('button[data-search="1"]').getAttribute('aria-pressed'),'true','Finishing resumes the selected branch without switching to Main');
      await page.locator('#tour-open').tap();
      await ready(page,0);
      await opening(page);
      for (let step=2;step<=4;step++) await next(page,step);
      await play(page);
      const playingPaths = await page.locator('#attempts').innerText();
      await page.locator('#tour-skip').tap();
      await page.waitForFunction((n) => !document.querySelector('#inspector').classList.contains('controlling') && document.querySelector('#pause').getAttribute('aria-label') === 'Pause Search' && Number(document.querySelector('#attempts').textContent.replaceAll(',','')) > Number(n.replaceAll(',','')),playingPaths);
    }
    await page.goto('about:blank');
    await page.close();
  }
  assert.deepEqual(errors, []);
  console.log('Mobile guided tour: grouped replay audio, sequential early gameplay exits, actual search resumption on Finish and Skip, unobstructed spotlights, real route selection/replay/scrubbing, touch gameplay, rotation, discard, branching and switching at 320px, 390px and landscape passed.');
} finally { await browser.close(); }
