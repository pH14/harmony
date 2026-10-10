// SPDX-License-Identifier: AGPL-3.0-or-later
import { chromium } from "@playwright/test";
import assert from "node:assert/strict";
import { writeFile, mkdir, readFile } from "node:fs/promises";
import { TOUR_KEY } from "../src/tour.js";
import { createEngine } from "../src/emulator.js";
const browser = await chromium.launch({
  headless: true,
  ...(process.env.CHROME_CHANNEL
    ? { channel: process.env.CHROME_CHANNEL }
    : {}),
});
const url = process.env.DEMO_URL || "http://127.0.0.1:4173";
const errors = [];
async function open(options = {}) {
  const page = await browser.newPage(options);
  page.on("pageerror", (e) => errors.push(e.message));
  return page;
}
async function ready(page, step) {
  await page.waitForFunction(
    (step) => {
      const tour = document.querySelector("#guided-tour");
      return (
        tour?.open &&
        Number(tour.dataset.step) === step &&
        tour.querySelector(".tour-card").getAttribute("aria-busy") ===
          "false" &&
        document.querySelector("#tour-next").textContent !== "Try again"
      );
    },
    step,
    { timeout: 60000 },
  );
}
async function layout(page) {
  const geometry = await page.locator("#guided-tour").evaluate((tour) => {
    const c = tour.querySelector(".tour-card").getBoundingClientRect(),
      h = tour.querySelector("#tour-rings rect")?.getBoundingClientRect();
    return {
      left: c.left,
      top: c.top,
      right: c.right,
      bottom: c.bottom,
      w: innerWidth,
      h: innerHeight,
      overlap: h
        ? Math.max(0, Math.min(c.right, h.right) - Math.max(c.left, h.left)) *
          Math.max(0, Math.min(c.bottom, h.bottom) - Math.max(c.top, h.top))
        : -1,
    };
  });
  assert.ok(geometry.left >= 8 && geometry.top >= 8);
  assert.ok(
    geometry.right <= geometry.w - 8 && geometry.bottom <= geometry.h - 8,
  );
  assert.equal(
    geometry.overlap,
    0,
    "The callout must leave its primary highlighted control visible",
  );
  assert.equal(await page.locator("#tour-next").isVisible(), true);
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
async function next(page, step) {
  await page.locator("#tour-next").click();
  await ready(page, step);
  await layout(page);
}
async function dimmedFraction(page, lit, undimmed) {
  return await page.evaluate(async ([before, after]) => {
    const pixels = async (data) => {
      const image = new Image();
      image.src = 'data:image/png;base64,' + data;
      await image.decode();
      const c = document.createElement('canvas');
      c.width = image.naturalWidth; c.height = image.naturalHeight;
      const ctx = c.getContext('2d');
      ctx.drawImage(image, 0, 0);
      return ctx.getImageData(0, 0, c.width, c.height).data;
    };
    const a = await pixels(before), b = await pixels(after);
    let darkened = 0;
    for (let i = 0; i < a.length; i += 4)
      if ((b[i] + b[i + 1] + b[i + 2]) - (a[i] + a[i + 1] + a[i + 2]) > 60) darkened++;
    return darkened / (a.length / 4);
  }, [lit.toString('base64'), undimmed.toString('base64')]);
}
async function watchDuringTour(page) {
  const map = page.locator('.area-map[data-trace-points]').filter({ visible: true });
  const traced = await map.evaluateAll((canvases) => canvases.find((c) => Number(c.dataset.tracePoints) > 0)?.dataset.map);
  assert.notEqual(traced, undefined);
  const canvas = page.locator(`.map-row[data-map="${traced}"] canvas`);
  const lit = await canvas.screenshot();
  await page.locator('.tour-shade').evaluate((e) => e.style.visibility = 'hidden');
  const undimmed = await canvas.screenshot();
  const darkenedFraction = await dimmedFraction(page, lit, undimmed);
  if (darkenedFraction >= .01) { await writeFile('test-results/tour-map-lit.png',lit); await writeFile('test-results/tour-map-clear.png',undimmed); await page.screenshot({path:'test-results/tour-map-layout.png'}); }
  assert.ok(darkenedFraction < 0.01, `The selected route map must remain lit; only transient sparks may differ (${darkenedFraction})`);
  await page.locator('.tour-shade').evaluate((e) => e.style.visibility = '');
  await page.screenshot({ path: `test-results/tour-live-${page.viewportSize().width}.png` });
  assert.equal(await page.locator('#pause').evaluate((e) => !!e.closest('[inert]')), true);
  await page.locator('#play').click();
  await page.waitForFunction(() => document.querySelector('#play').textContent.includes('Pause replay') && Number(document.querySelector('#scrub').value) >= 10);
  await page.locator('#play').focus();
  assert.equal(await page.evaluate(() => document.activeElement.id), 'play');
  await page.keyboard.press('Space');
  await page.waitForFunction(() => document.querySelector('#play').textContent.includes('Replay'));
  await page.keyboard.press('Tab');
  if (await page.locator('#sound').evaluate((node) => !!node.closest('.transport'))) {
    assert.equal(await page.evaluate(() => document.activeElement.id), 'sound');
    await page.keyboard.press('Tab');
  }
  assert.equal(await page.evaluate(() => document.activeElement.id), 'scrub');
  await page.keyboard.press('Home');
  await page.waitForFunction(() => document.querySelector('#scrub').value === '0' && document.querySelector('#frame-label').textContent.startsWith('FRAME 0'));
  const slider = await page.locator('#scrub').boundingBox();
  if (page.viewportSize().width < 800) await page.touchscreen.tap(slider.x + slider.width * 0.55, slider.y + slider.height / 2);
  else await page.locator('#scrub').click({ position: { x: slider.width * 0.55, y: slider.height / 2 } });
  await page.waitForFunction(() => {
    const frame = document.querySelector('#scrub').value;
    return Number(frame) > 1 && document.querySelector('#frame-label').textContent.startsWith('FRAME ' + Number(frame).toLocaleString('en-US')) &&
      [...document.querySelectorAll('.area-map')].some((c) => c.dataset.markerFrame === frame);
  });
  await page.locator('#tour-next').focus();
  await page.keyboard.press('Tab');
  assert.equal(await page.evaluate(() => document.activeElement.id), 'play');
  assert.equal(await page.locator('#guided-tour').getAttribute('data-step'), '3');
}
try {
  await mkdir("test-results", { recursive: true });
  const page = await open({ viewport: { width: 1440, height: 1100 } });
  await page.goto(url);
  await ready(page, 0);
  await opening(page);
  await layout(page);
  assert.equal(
    await page.locator("#pause").getAttribute("aria-label"),
    "Pause Search",
  );
  assert.equal(
    await page.evaluate(() => document.activeElement.id),
    "tour-next",
  );
  await page.screenshot({ path: "test-results/tour-desktop-heat.png" });
  const initial = await page.locator("#attempts").innerText();
  await page.waitForFunction(
    (n) => document.querySelector("#attempts").textContent !== n,
    initial,
  );
  await next(page, 2);
  assert.ok((await page.locator("#state-list .state").count()) >= 2);
  assert.equal(
    await page.locator("#verification").innerText(),
    "Exact replay ✓",
  );
  assert.equal(
    await page.locator("#pause").getAttribute("aria-label"),
    "Resume Search",
  );
  assert.equal(await page.locator("#tour-holes rect").count(), 2);
  assert.equal(await page.locator('#tour-ping').getAttribute('visibility'), 'visible');
  assert.ok(await page.locator('#tour-ping-bounds rect').count() > 0);
  const wave = page.locator('.tour-ping-wave').first();
  const phase = await wave.evaluate((node) => node.getAnimations()[0].currentTime);
  await page.waitForTimeout(150);
  assert.ok(await wave.evaluate((node) => node.getAnimations()[0].currentTime) > phase, 'The History ping repeats without resetting each redraw');
  const center = await page.locator('.tour-ping-core').evaluate((node) => { const r = node.getBoundingClientRect(); return { x: Math.floor(r.left + r.width / 2), y: Math.floor(r.top + r.height / 2) }; });
  const pixel = await page.screenshot({clip:{...center,width:1,height:1}});
  const rgba = await page.evaluate(async (data) => { const image = new Image(); image.src = 'data:image/png;base64,' + data; await image.decode(); const c = document.createElement('canvas'); c.width = c.height = 1; const ctx = c.getContext('2d'); ctx.drawImage(image,0,0); return [...ctx.getImageData(0,0,1,1).data]; }, pixel.toString('base64'));
  assert.deepEqual(rgba, [255,240,174,255], 'The History cell ping is actually rendered above the shade');
  const highlightedRows = await page.locator("#state-list").screenshot();
  await page
    .locator(".tour-shade")
    .evaluate((e) => (e.style.visibility = "hidden"));
  assert.deepEqual(
    await page.locator("#state-list").screenshot(),
    highlightedRows,
    "Spotlighted route pixels must remain unchanged by the dimming layer",
  );
  await page.locator(".tour-shade").evaluate((e) => (e.style.visibility = ""));
  const other = page.locator('#state-list .state:not(.selected)').first();
  const chosen = await other.getAttribute('data-state-id');
  const selectedBeforeHover = await page.locator('#film-title').getAttribute('data-state-id');
  const selectedFrameBeforeHover = await page.locator('#scrub').inputValue();
  await other.hover();
  await page.waitForFunction((id) => document.querySelector('#route-preview').dataset.stateId === id && !document.querySelector('#route-preview canvas').hidden && [...document.querySelectorAll('.area-map')].some((c) => c.dataset.previewRoute === id && Number(c.dataset.tracePoints) > 0), chosen);
  assert.equal(await page.locator('#film-title').getAttribute('data-state-id'), selectedBeforeHover);
  assert.equal(await page.locator('#scrub').inputValue(), selectedFrameBeforeHover);
  const hoverMap = page.locator('.area-map').filter({visible:true});
  const mapId = await hoverMap.evaluateAll((canvases) => canvases.find((c) => Number(c.dataset.tracePoints) > 0)?.dataset.map);
  const hoveredCanvas = page.locator(`.area-map[data-map="${mapId}"]`);
  const litHover = await hoveredCanvas.screenshot();
  await page.locator('.tour-shade').evaluate((e) => e.style.visibility = 'hidden');
  const dimmedHover = await dimmedFraction(page, litHover, await hoveredCanvas.screenshot());
  assert.ok(dimmedHover < .01, `The hovered history path and its map must be fully lit (${dimmedHover})`);
  await page.locator('.tour-shade').evaluate((e) => e.style.visibility = '');
  await page.screenshot({path:'test-results/tour-hover-path.png'});
  await page.locator('#tour-title').hover();
  await page.waitForFunction(() => [...document.querySelectorAll('.area-map')].every((c) => c.dataset.previewRoute === ''));
  assert.equal(await page.locator('#film-title').getAttribute('data-state-id'),selectedBeforeHover);
  await other.click();
  await page.waitForFunction((id) => document.querySelector('#film-title').dataset.stateId === id && document.querySelector('#verification').textContent === 'Exact replay ✓', chosen);
  assert.equal(await page.locator('#guided-tour').getAttribute('data-step'), '2');
  await page.screenshot({ path: "test-results/tour-desktop-routes.png" });
  for (let step = 3; step < 7; step++) {
    await next(page, step);
    if (step === 3) {
      await page.locator('#sound').click();
      assert.equal(await page.locator('#sound').getAttribute('aria-pressed'), 'true');
      await page.locator('#sound').click();
      assert.equal(await page.locator('#sound').getAttribute('aria-pressed'), 'false');
      await watchDuringTour(page);
    }
    if (step === 4) {
      assert.match(await page.locator('#tour-copy').innerText(), /branch/);
      assert.equal(await page.locator('#guided-tour').evaluate((e) => e.matches(':modal')), false);
      assert.equal(await page.locator('#pause').evaluate((e) => !!e.closest('[inert]')), true);
    }
  }
  assert.equal(
    await page.locator("#branch-tree button[data-search]").count(),
    1,
    "The tour must not create a search branch",
  );
  await page.locator("#tour-back").click();
  await ready(page, 5);
  await layout(page);
  await next(page, 6);
  await page.locator("#tour-next").click();
  assert.equal(await page.locator("#guided-tour").isVisible(), false);
  assert.equal(
    await page.locator("#pause").getAttribute("aria-label"),
    "Pause Search",
  );
  assert.equal(
    await page.evaluate((key) => localStorage.getItem(key), TOUR_KEY),
    "seen",
  );
  await page.locator("#timeline-toggle").click();
  await page.locator("#tour-open").click();
  assert.equal(await page.locator("#timeline-toggle").getAttribute("aria-expanded"), "true", "The tour opens a collapsed Timeline");
  await ready(page, 0);
  await opening(page);
  await next(page, 2);
  await page.keyboard.press("Escape");
  assert.equal(await page.locator("#guided-tour").isVisible(), false);
  assert.equal(
    await page.locator("#inspector").isVisible(),
    true,
    "Tour Escape must not also close the history pane",
  );
  assert.equal(
    await page.evaluate(() => document.activeElement.id),
    "tour-open",
  );
  await page.reload();
  await page.waitForFunction(
    () =>
      Number(
        document.querySelector("#attempts").textContent.replaceAll(",", ""),
      ) >= 50,
  );
  assert.equal(
    await page.locator("#guided-tour").isVisible(),
    false,
    "A dismissed tour must not interrupt the next visit",
  );
  await page.locator("#tour-open").click();
  await ready(page, 0);
  await opening(page);
  for (let step = 2; step < 7; step++) await next(page, step);
  assert.equal(await page.locator("#tour-next").innerText(), "Let’s go explore!");
  assert.equal(await page.locator("#tour-try, #tour-progress").count(), 0);
  await page.locator("#tour-next").click();
  await page.locator("#take-control").click();
  assert.equal(await page.locator("#guided-tour").isVisible(), false);
  assert.equal(
    await page.locator("#take-control").getAttribute("aria-pressed"),
    "true",
  );
  assert.equal(await page.locator("#tour-open").isDisabled(), true);
  await page.waitForTimeout(1200);
  await page.keyboard.press("ArrowRight");
  await page.keyboard.press("Escape");
  assert.equal(
    await page.locator("#take-control").getAttribute("aria-pressed"),
    "false",
  );
  const manual = await page
      .locator("#film-title")
      .getAttribute("data-state-id"),
    manualFrame = await page.locator("#scrub").inputValue(),
    manualPixels = await page.locator("#film").evaluate((c) => c.toDataURL());
  assert.ok(manual.startsWith("manual-"));
  await page.locator("#tour-open").click();
  await ready(page, 0);
  await opening(page);
  await next(page, 2);
  await next(page, 3);
  await watchDuringTour(page);
  await page.locator('#scrub').focus();
  const cpu = await page.context().newCDPSession(page);
  await cpu.send("Emulation.setCPUThrottlingRate", { rate: 6 });
  await page.keyboard.press('Escape');
  await page.waitForFunction(
    ({ manual, manualFrame }) =>
      document.querySelector("#film-title").dataset.stateId === manual &&
      document.querySelector("#scrub").value === manualFrame &&
      !document.querySelector("#take-control").disabled,
    { manual, manualFrame },
  );
  assert.equal(
    await page.locator("#film").evaluate((c) => c.toDataURL()),
    manualPixels,
    "Escaping from the live scrubber must restore the existing human history and its frame",
  );
  assert.equal(
    await page.evaluate(() => document.activeElement.id),
    "tour-open",
    "Focus must return after asynchronous human-history restoration finishes",
  );
  assert.equal(await page.locator('[inert]').count(), 0, 'Leaving replay interaction must release all temporary background isolation');
  await cpu.send("Emulation.setCPUThrottlingRate", { rate: 1 });
  await cpu.detach();
  await page.locator('#tour-open').click();
  await ready(page, 0);
  await opening(page);
  await next(page, 2);
  const keyboardRoute = await page.locator('#film-title').getAttribute('data-state-id');
  assert.notEqual(keyboardRoute, manual);
  await page.locator('#map').focus();
  await page.keyboard.press('ArrowLeft');
  await page.keyboard.press('ArrowRight');
  await page.keyboard.press('Enter');
  await page.waitForFunction((id) => document.querySelector('#film-title').dataset.stateId === id && document.querySelector('#verification').textContent === 'Exact replay ✓', keyboardRoute);
  await page.keyboard.press('Escape');
  assert.equal(await page.locator('#guided-tour').isVisible(), false);
  assert.equal(await page.locator('#film-title').getAttribute('data-state-id'), keyboardRoute, 'Explicit keyboard cell selection must survive dismissal instead of restoring an older manual history');
  assert.equal(await page.locator('[inert]').count(), 0);

  assert.equal(
    await page.locator("#pause").getAttribute("aria-label"),
    "Pause Search",
    "Tour exit must resume even an already-paused search",
  );
  await page.close();

  const phone = await open({
    viewport: { width: 390, height: 844 },
    isMobile: true,
    hasTouch: true,
    reducedMotion: "reduce",
  });
  await phone.goto(url);
  await ready(phone, 0);
  await opening(phone);
  await layout(phone);
  for (let step = 2; step < 7; step++) {
    await next(phone, step);
    if (step === 2) {
      const route = phone.locator("#state-list .state").last();
      await route.tap();
      await phone.waitForFunction(() => !document.querySelector('#take-control').disabled);
      const clips = await phone.evaluate(() => {
        const hole = document.querySelector('#tour-rings rect').getBoundingClientRect(), pane = document.querySelector('#inspector').getBoundingClientRect();
        return {top:hole.top,bottom:hole.bottom,paneTop:pane.top,paneBottom:pane.bottom};
      });
      assert.ok(clips.top >= clips.paneTop && clips.bottom <= clips.paneBottom, `Phone spotlights stay inside the visible history sheet: ${JSON.stringify(clips)}`);
    }
    if (step === 3) await watchDuringTour(phone);
    if (step === 2 || step === 4 || step === 6)
      await phone.screenshot({ path: `test-results/tour-phone-${step}.png` });
  }
  assert.equal(await phone.locator("#tour-next").innerText(), "Let’s go explore!");
  assert.equal(await phone.locator("#tour-try, #tour-progress").count(), 0);
  await phone.locator("#tour-next").click();
  await phone.locator("#take-control").click();
  assert.equal(
    await phone.locator("#take-control").getAttribute("aria-pressed"),
    "true",
  );
  assert.equal(
    await phone
      .locator("#inspector")
      .evaluate((e) => e.classList.contains("expanded")),
    true,
  );
  await phone.close();

  const landscape = await open({
    viewport: { width: 844, height: 390 },
    isMobile: true,
    hasTouch: true,
  });
  await landscape.goto(url);
  await ready(landscape, 0);
  await opening(landscape);
  for (let step = 2; step < 7; step++) await next(landscape, step);
  assert.equal(await landscape.locator("#tour-next").innerText(), "Let’s go explore!");
  assert.equal(await landscape.locator("#tour-try, #tour-progress").count(), 0);
  await landscape.locator("#tour-next").click();
  await landscape.locator("#take-control").click();
  assert.equal(
    await landscape.locator("#take-control").getAttribute("aria-pressed"),
    "true",
  );
  const gameVisible = await landscape.locator("#film").evaluate((e) => {
    const r = e.getBoundingClientRect(),
      pane = e.closest("#inspector").getBoundingClientRect();
    return (
      Math.max(
        0,
        Math.min(r.bottom, pane.bottom, innerHeight) -
          Math.max(r.top, pane.top, 0),
      ) >=
      r.height - 1
    );
  });
  assert.equal(
    gameVisible,
    true,
    "Tour takeover must bring the game into view on short screens",
  );
  await landscape.screenshot({ path: "test-results/tour-landscape-play.png" });
  await landscape.keyboard.press("Escape");
  const landscapeManual = await landscape
    .locator("#film-title")
    .getAttribute("data-state-id");
  await landscape.locator("#tour-open").click();
  await ready(landscape, 0);
  await opening(landscape);
  await next(landscape, 2);
  await landscape.locator("#tour-skip").click();
  await landscape.waitForFunction(
    (id) =>
      document.querySelector("#film-title").dataset.stateId === id &&
      !document.querySelector("#take-control").disabled,
    landscapeManual,
  );
  assert.equal(
    await landscape.locator("#film").evaluate((e) => {
      const r = e.getBoundingClientRect(),
        pane = e.closest("#inspector").getBoundingClientRect();
      return (
        Math.max(
          0,
          Math.min(r.bottom, pane.bottom, innerHeight) -
            Math.max(r.top, pane.top, 0),
        ) >=
        r.height - 1
      );
    }),
    true,
    "Restoring human gameplay must reveal its frame after leaving the tour",
  );
  await landscape.close();

  const practice = await open({ viewport: { width: 1440, height: 1100 } });
  await practice.goto(url);
  await ready(practice, 0);
  await opening(practice);
  for (let step = 2; step <= 4; step++) await next(practice, step);
  await practice.locator('#take-control').click();
  assert.equal(await practice.locator('#guided-tour').isVisible(), true);
  const start = Number(await practice.locator('#scrub').inputValue());
  await practice.keyboard.down('ArrowRight');
  await practice.waitForTimeout(200);
  await practice.keyboard.up('ArrowRight');
  await practice.waitForFunction((frame) => Number(document.querySelector('#scrub').value) > frame, start);
  await next(practice, 5);
  assert.equal(await practice.locator('#take-control').getAttribute('aria-pressed'), 'true');
  await practice.locator('#search-here').click();
  await ready(practice, 6);
  await practice.waitForFunction(() => document.querySelector('#branch-tree button[aria-pressed=true]').dataset.search === '1' && document.querySelector('#inspector').hidden);
  assert.equal(await practice.locator('li[data-search-node="0"] > ol > li[data-search-node="1"]').count(), 1);
  await practice.locator('button[data-search="0"]').hover();
  await practice.waitForFunction(() => [...document.querySelectorAll('.area-map')].every(c => c.dataset.previewSearch === '0'));
  assert.equal(await practice.locator('button[data-search="1"]').getAttribute('aria-pressed'), 'true');
  await practice.locator('button[data-search="0"]').click();
  await practice.waitForFunction(() => document.querySelector('#branch-tree button[aria-pressed=true]').dataset.search === '0' && document.querySelector('#pause').getAttribute('aria-label') === 'Pause Search');
  await practice.locator('#tour-next').click();
  assert.equal(await practice.locator('[inert]').count(), 0);
  await practice.close();

  const death = await open({ viewport: { width: 1440, height: 1100 } });
  await death.addInitScript(() => {
    const RealWorker = window.Worker;
    window.Worker = class extends RealWorker {
      constructor(...args) { super(...args); window.tourTestWorker = this; }
      postMessage(data, ...rest) { if (data.type === 'states' && data.request > 0) window.tourTestRequest = data.request; super.postMessage(data, ...rest); }
    };
  });
  await death.goto(url);
  await ready(death, 0);
  await opening(death);
  for (let step = 2; step <= 4; step++) await next(death, step);
  const base = new URL('../public/', import.meta.url);
  const emulator = await createEngine(base, {rom:await readFile(new URL('nova.nes',base)), wasmBinary:await readFile(new URL('engine/quicknes.wasm',base))});
  emulator.boot();
  const tape = JSON.parse(await readFile(new URL('fixtures/level-two-2.json', import.meta.url)));
  const actions = [...tape.actions, {buttons:130,frames:100}];
  for (const action of actions) emulator.run(action.buttons,action.frames);
  assert.ok(emulator.observation().health, 'The recorded controller history reaches a living near-death state');
  const state = {id:'tour-near-death', actions, frames:actions.reduce((sum,a)=>sum+a.frames,0), observation:emulator.observation(), snapshot:[...emulator.capture()]};
  await death.evaluate((state) => {
    state.snapshot = new Uint8Array(state.snapshot);
    window.tourTestWorker.onmessage({data:{type:'states',request:window.tourTestRequest,states:[state]}});
  }, state);
  await death.waitForFunction(() => document.querySelector('#film-title').dataset.stateId === 'tour-near-death' && !document.querySelector('#take-control').disabled);
  await death.locator('#take-control').click();
  const draftId = await death.locator('#film-title').getAttribute('data-state-id');
  await death.keyboard.down('ArrowRight');
  await death.keyboard.down('x');
  await death.waitForFunction(() => [...document.querySelectorAll('#details div')].some(node => node.firstElementChild.textContent === 'Health' && node.lastElementChild.textContent === '0 / 4'), null, {timeout:10000});
  await next(death, 5);
  await death.keyboard.up('x');
  await death.keyboard.up('ArrowRight');
  assert.equal(await death.locator('#film-title').getAttribute('data-state-id'),draftId,'Admission must preserve the played draft even when Nova dies');
  assert.equal(await death.locator('#take-control').isVisible(),false);
  assert.equal(await death.locator('#game-controls').isVisible(),true);
  assert.equal(await death.locator('#discard-branch').isVisible(),true);
  await death.locator('#discard-branch').click();
  await death.locator('#tour-skip').click();
  await death.close();

  const blocked = await open({ viewport: { width: 1440, height: 900 } });
  await blocked.addInitScript(() =>
    Object.defineProperty(window, "localStorage", {
      get() {
        throw new Error("Storage denied");
      },
    }),
  );
  await blocked.goto(url);
  await ready(blocked, 0);
  await opening(blocked);
  await blocked.locator("#tour-skip").click();
  const paths = await blocked.locator("#attempts").innerText();
  await blocked.waitForFunction(
    (n) =>
      Number(
        document.querySelector("#attempts").textContent.replaceAll(",", ""),
      ) >
      Number(n.replaceAll(",", "")) + 100,
    paths,
  );
  assert.equal(
    await blocked.locator("#guided-tour").isVisible(),
    false,
    "Blocked storage must not repeatedly reopen onboarding",
  );
  await blocked.locator("#tour-open").click();
  await ready(blocked, 0);
  await opening(blocked);
  await blocked.keyboard.press("Escape");
  await blocked.close();
  assert.deepEqual(errors, []);
  console.log(
    "Guided tour: clickable routes, real takeover/fork/tree switching, live replay and keyboard/touch scrubbing, fully lit route maps, background isolation, authentic routes, spotlight geometry, unconditional search resumption, keyboard exit, persistence, explicit takeover, rendered repeating cell ping, preserved authentic dead gameplay draft, mobile and blocked storage passed.",
  );
} finally {
  for (const page of browser.contexts().flatMap((context) => context.pages())) {
    await page.goto("about:blank");
    await page.close();
  }
  await browser.close();
}
