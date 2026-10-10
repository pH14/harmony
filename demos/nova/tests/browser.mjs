// SPDX-License-Identifier: AGPL-3.0-or-later
import { chromium } from "@playwright/test";
import assert from "node:assert/strict";
import { readFile, mkdir } from "node:fs/promises";
import { pathToFileURL } from "node:url";
import { gunzipSync } from "node:zlib";
import { createEngine } from "../src/emulator.js";
import { snapshotHash } from "../src/media.js";
import { prefixAt } from "../src/branch.js";
import { TOUR_KEY } from "../src/tour.js";
import { isMapEvidence } from "../src/world.js";
const browser = await chromium.launch({
  headless: true,
  ...(process.env.CHROME_CHANNEL
    ? { channel: process.env.CHROME_CHANNEL }
    : {}),
});
const page = await browser.newPage({ viewport: { width: 1440, height: 1100 } }),
  errors = [];
page.on("pageerror", (error) => errors.push(error.message));
page.on("response", (response) => {
  if (response.status() >= 400)
    errors.push(`${response.status()} ${response.url()}`);
});
await page.addInitScript((tourKey) => {
  localStorage.setItem(tourKey, "seen");
  const RealWorker = window.Worker;
  window.novaTestRouteCells = new Map();
  window.novaTestHeatPixels = (canvas) => {
    const context = canvas.getContext('2d'), pixels = [];
    for (let y = 8; y < canvas.height; y += 32)
      for (let x = 112; x < canvas.width; x += 32)
        pixels.push(...context.getImageData(x, y, 1, 1).data);
    return pixels;
  };
  window.Worker = class extends RealWorker {
    constructor(...args) {
      super(...args);
      window.novaTestWorker = this;
      this.addEventListener("message", ({ data }) => {
        if (data.type === "states") window.novaTestStates = data.states;
        if (data.type === "batch" && data.active === 2) {
          if (!window.novaTestNestedBatch)
            window.novaTestNestedBatch = { ...data, motionStart: [...data.motion[0].slice(0, 5)] };
          window.novaTestNestedRetained ??= data.points.find(
            (point) => point.retained !== null && point.retained !== "2:0",
          )?.retained;
        }
        if (
          data.type === "batch" &&
          data.active === 0 &&
          window.novaTestRouteCells
        ) {
          const canvas = document.querySelector(
              '.map-row[data-map="0"] canvas',
            ),
            width = Number(canvas?.dataset.mapWidth),
            height = Number(canvas?.dataset.mapHeight);
          for (const point of data.points) {
            const o = point.observation;
            if (
              point.retained === null ||
              o.level !== 0 ||
              o.reload ||
              o.program_bank !== 9 ||
              o.selected_level !== 0 ||
              o.x < 96 ||
              o.x >= width ||
              o.y < 0 ||
              o.y >= height
            )
              continue;
            const key = `${Math.floor(o.x / 32)}:${Math.floor(o.y / 32)}`;
            if (!window.novaTestRouteCells.has(key))
              window.novaTestRouteCells.set(key, {
                ids: new Set(),
                observation: o,
              });
            const cell = window.novaTestRouteCells.get(key);
            cell.ids.add(point.retained);
            cell.observation = o;
          }
        }
      });
    }
    postMessage(data, ...rest) {
      if (data.type === "states") window.novaTestRequest = data.request;
      if (data.type === "fork") window.novaTestFork = structuredClone(data);
      super.postMessage(data, ...rest);
    }
  };
}, TOUR_KEY);
async function openCurrentCell() {
  await page.waitForFunction(
    () =>
      !!document.querySelector("#details b") &&
      !document.querySelector("#take-control").disabled,
  );
  const map = page.locator("#map");
  await map.scrollIntoViewIfNeeded();
  const point = await map.evaluate((canvas) => {
    const [x, y] = document
        .querySelector("#details b")
        .textContent.split(",")
        .map(Number),
      width = Number(canvas.dataset.mapWidth),
      height = Number(canvas.dataset.mapHeight),
      scale =
        Math.min(canvas.width / width, canvas.height / height) *
        Number(canvas.dataset.zoom),
      rect = canvas.getBoundingClientRect(),
      px = Math.floor((x % width) / 32) * 32 + 16,
      py = Math.floor((Math.floor(x / width) * 224 + y) / 32) * 32 + 8;
    return {
      x:
        (((px - Number(canvas.dataset.centerX)) * scale + canvas.width / 2) *
          rect.width) /
        canvas.width,
      y:
        (((py - Number(canvas.dataset.centerY)) * scale + canvas.height / 2) *
          rect.height) /
        canvas.height,
    };
  });
  await map.click({ position: point });
  await page.waitForFunction(
    () =>
      !document.querySelector("#inspector").hidden &&
      !document.querySelector("#take-control").disabled,
  );
}
const localBase = pathToFileURL(process.cwd() + "/public/");
const emulator = await createEngine(localBase, {
  rom: new Uint8Array(await readFile(new URL("nova.nes", localBase))),
  wasmBinary: await readFile(new URL("engine/quicknes.wasm", localBase)),
});
const emulatorRoot = emulator.boot();
const catalog = JSON.parse(await readFile(new URL("maps.json", localBase)));
async function selectFixture(tape, id) {
  emulator.restore(emulatorRoot);
  const frames = tape.actions.reduce((n, a) => n + a.frames, 0);
  for (const a of tape.actions) emulator.run(a.buttons, a.frames);
  if (tape.endpoint_sha256)
    assert.equal(await snapshotHash(emulator.capture()), tape.endpoint_sha256);
  const state = {
    id,
    actions: tape.actions,
    frames,
    observation: emulator.observation(),
    snapshot: [...emulator.capture()],
  };
  await page.evaluate((state) => {
    state.snapshot = new Uint8Array(state.snapshot);
    window.novaTestWorker.onmessage({
      data: {
        type: "states",
        request: window.novaTestRequest,
        states: [state],
      },
    });
  }, state);
  return frames;
}
async function measurePlayback() {
  return page.evaluate(
    () =>
      new Promise((resolve) => {
        const start = performance.now(),
          first = Number(document.querySelector("#scrub").value);
        let samples = 0;
        const step = (now) => {
          samples++;
          if (now - start >= 600)
            resolve({
              duration: now - start,
              frames: Number(document.querySelector("#scrub").value) - first,
              samples,
            });
          else requestAnimationFrame(step);
        };
        requestAnimationFrame(step);
      }),
  );
}
try {
  await page.goto(process.env.DEMO_URL || "http://127.0.0.1:4173");
  await page.waitForFunction(
    () =>
      document.querySelector("#verification")?.textContent === "Original game",
  );
  assert.equal(await page.locator("#speed").inputValue(), "1");
  assert.equal(
    await page.locator("#screenshot,#export,#import,#history-file").count(),
    0,
  );
  assert.equal(await page.locator(".brand").innerText(), "harmony");
  const toolbarLayout = await page.locator(".exploration").evaluate((panel) => {
    const controls = panel.querySelector(".controls").getBoundingClientRect(),
      counters = panel.querySelector(".metrics").getBoundingClientRect(),
      goal = panel.querySelector(".goal").getBoundingClientRect(),
      panelRect = panel.getBoundingClientRect();
    return {
      left: controls.left - panelRect.left,
      countersBottom: counters.bottom,
      goalTop: goal.top,
      size: getComputedStyle(panel.querySelector(".metrics b")).fontSize,
    };
  });
  assert.ok(toolbarLayout.left < 30);
  assert.ok(toolbarLayout.countersBottom <= toolbarLayout.goalTop);
  assert.ok(parseFloat(toolbarLayout.size) <= 14);
  const originImage = await page
    .locator("#film")
    .evaluate((c) => c.toDataURL());
  await page.waitForFunction(
    () =>
      Number(
        document.querySelector("#attempts").textContent.replaceAll(",", ""),
      ) > 30 || !document.querySelector("#error").hidden,
    { timeout: 60000 },
  );
  assert.equal(await page.locator("#error").isVisible(), false);
  await page.waitForFunction(
    () => Number(document.querySelector("#scrub").max) > 0,
  );
  await page.waitForFunction(() =>
    [...window.novaTestRouteCells.values()].some((cell) => cell.ids.size > 1),
  );
  await page.locator("#pause").click();
  await page.waitForTimeout(200);
  const attempts = await page.locator("#attempts").innerText();
  await page.waitForTimeout(200);
  assert.equal(await page.locator("#attempts").innerText(), attempts);
  assert.equal(await page.locator("#inspector").isVisible(), false);
  await page.waitForFunction(() =>
    [...document.querySelectorAll(".map-row canvas")].every(
      (c) => c.dataset.markerFrame === "",
    ),
  );
  assert.equal(
    await page.locator(".map-row canvas").evaluateAll((canvases) =>
      canvases.some((c) => {
        const pixels = c
          .getContext("2d")
          .getImageData(0, 0, c.width, c.height).data;
        for (let i = 0; i < pixels.length; i += 4)
          if (
            pixels[i] === 255 &&
            pixels[i + 1] === 245 &&
            pixels[i + 2] === 204
          )
            return true;
        return false;
      }),
    ),
    false,
    "The hidden automatic preview must not highlight a replay position on the map",
  );
  const { x, y } = await page.evaluate(() => {
    const cell = [...window.novaTestRouteCells.values()]
      .filter((entry) => entry.ids.size > 1)
      .sort((a, b) => b.ids.size - a.ids.size)[0];
    window.novaTestRouteCells = null;
    return cell.observation;
  });
  const introMap = page.locator('.map-row[data-map="0"] canvas');
  await introMap.scrollIntoViewIfNeeded();
  const clickIntroCell = async () => {
    const click = await introMap.evaluate(
      (canvas, position) => {
        const width = Number(canvas.dataset.mapWidth),
          height = Number(canvas.dataset.mapHeight),
          scale = Math.min(canvas.width / width, canvas.height / height),
          rect = canvas.getBoundingClientRect();
        const px = Math.floor((position.x % width) / 32) * 32 + 16,
          py =
            Math.floor(
              (Math.floor(position.x / width) * 224 + position.y) / 32,
            ) *
              32 +
            8;
        return {
          x:
            (rect.width * ((canvas.width - width * scale) / 2 + px * scale)) /
            canvas.width,
          y:
            (rect.height *
              ((canvas.height - height * scale) / 2 + py * scale)) /
            canvas.height,
        };
      },
      { x, y },
    );
    await introMap.hover({ position: click });
    assert.equal(await introMap.evaluate((c) => getComputedStyle(c).cursor), "pointer");
    await introMap.click({ position: click });
  };
  await introMap.hover({ position: { x: 2, y: 2 } });
  assert.equal(await introMap.evaluate((c) => getComputedStyle(c).cursor), "default");
  await introMap.click({ position: { x: 2, y: 2 } });
  assert.equal(
    await page.locator("#inspector").getAttribute("data-empty"),
    "true",
  );
  assert.equal(await page.locator("#state-list .state").count(), 0);
  assert.equal(await page.locator("#inspector").isVisible(), false);
  assert.equal(
    await page
      .locator("#workspace")
      .evaluate((el) => el.classList.contains("inspect-open")),
    false,
  );
  await introMap.click({ position: { x: 2, y: 2 } });
  assert.equal(await page.locator("#inspector").isVisible(), false);
  await page.waitForFunction(() =>
    [...document.querySelectorAll(".map-row canvas")].every(
      (c) => c.dataset.markerFrame === "",
    ),
  );
  await clickIntroCell();
  await page.waitForFunction(
    (expected) => document.querySelector("#cell-title").title === expected,
    `Cell ${Math.floor(x / 32)}, ${Math.floor(y / 32)}`,
  );
  assert.equal(await page.locator("#inspector").isVisible(), true);
  assert.equal(await page.locator("#selection-hint").isVisible(), false);
  assert.equal(await page.locator("#verification").isVisible(), false);
  await page.waitForFunction(
    () =>
      document.querySelector("#verification").textContent === "Exact replay ✓",
    { timeout: 60000 },
  );
  await page.waitForFunction(
    () =>
      Number(
        document.querySelector('.map-row[data-map="0"] canvas').dataset
          .tracePoints,
      ) > 1,
  );
  await introMap.click({ position: { x: 2, y: 2 } });
  assert.equal(await page.locator("#inspector").isVisible(), false);
  assert.equal(await page.locator("#state-list .state").count(), 0);
  assert.equal(await page.locator("#play").isEnabled(), false);
  assert.equal(await page.locator("#take-control").isEnabled(), false);
  await page.waitForFunction(() =>
    [...document.querySelectorAll(".map-row canvas")].every(
      (c) => c.dataset.markerFrame === "",
    ),
  );
  await clickIntroCell();
  await page.waitForFunction(
    () =>
      document.querySelector("#verification").textContent === "Exact replay ✓",
  );
  assert.equal(await page.locator("#inspector").isVisible(), true);
  const selected = await page.locator("#state-list .selected").innerText();
  assert.match(selected, /Route \d+/);
  assert.match(selected, /\d+:\d{2}/);
  assert.doesNotMatch(selected, / replay/);
  assert.equal(
    await page.locator("#cell-title").innerText(),
    "Routes to this location",
  );
  assert.equal(
    await page.locator("#take-control").innerText(),
    "🎮 Play from here",
  );
  assert.equal(await page.locator("#details").isVisible(), false);
  await page.locator(".state-disclosure summary").click();
  assert.equal(await page.locator("#details").isVisible(), true);
  await page.locator(".state-disclosure summary").click();
  const stateLayout = await page.locator("#inspector").evaluate((panel) => ({
    fits:
      panel.scrollWidth <= panel.clientWidth &&
      [...panel.querySelectorAll(".state")].every(
        (b) => b.scrollWidth <= b.clientWidth,
      ),
    primary: getComputedStyle(panel.querySelector("#take-control"))
      .backgroundColor,
    secondary: getComputedStyle(panel.querySelector("#search-here"))
      .backgroundColor,
  }));
  assert.equal(stateLayout.fits, true, "History metadata must fit its buttons");
  assert.equal(await page.locator("#search-here").isVisible(), false, "Archived states offer gameplay before branching");
  assert.equal(await page.locator("#discard-branch").isVisible(), false);
  const selectedRoute = await page
    .locator("#state-list .selected")
    .evaluate((row) => ({
      id: row.dataset.stateId,
      name: row.querySelector(".state-name").textContent,
    }));
  const sibling = page.locator('#state-list .state:not(.selected)').first();
  const siblingId = await sibling.getAttribute('data-state-id');
  const selectedFrameBeforePreview = await page.locator('#scrub').inputValue();
  const selectedPixelsBeforePreview = await page.locator('#film').evaluate(c => c.toDataURL());
  await sibling.hover();
  await page.waitForFunction((id) => document.querySelector('#film').dataset.previewRoute === id, siblingId);
  assert.equal(await page.locator('#route-preview,#cell-visits').count(),0);
  assert.equal(await page.locator('.screen canvas').count(),1,'Hover reuses the existing History screen');
  assert.equal(await page.locator('#film-title').getAttribute('data-state-id'), selectedRoute.id);
  assert.equal(await page.locator('#scrub').inputValue(), selectedFrameBeforePreview);
  const previewPixels = await page.locator('#film').evaluate(c => c.toDataURL());
  await page.locator('#film-title').hover();
  await page.waitForFunction(() => document.querySelector('#film').dataset.previewRoute === '');
  assert.equal(await page.locator('#film').evaluate(c=>c.toDataURL()),selectedPixelsBeforePreview,'Leaving a hover restores the selected screenshot');
  await sibling.hover();
  await page.waitForFunction((id) => document.querySelector('#film').dataset.previewRoute === id,siblingId);
  const compactRow = await sibling.evaluate(row=>{const spans=[...row.querySelector('.state-summary').children].map(e=>e.getBoundingClientRect());return {overlap:Math.min(...spans.map(r=>r.bottom))-Math.max(...spans.map(r=>r.top)),height:row.getBoundingClientRect().height};});
  assert.ok(compactRow.overlap > 0 && compactRow.height <= 36,'Route name and time share a compact single row');
  await sibling.click();
  await page.waitForFunction((id) => document.querySelector('#film-title').dataset.stateId === id && document.querySelector('#verification').textContent === 'Exact replay ✓', siblingId);
  assert.equal(await page.locator('#film').evaluate(c => c.toDataURL()), previewPixels, 'Hover must show the exact retained endpoint without altering selection');
  await page.locator(`#state-list [data-state-id="${selectedRoute.id}"]`).click();
  await page.waitForFunction((id) => document.querySelector('#film-title').dataset.stateId === id && document.querySelector('#verification').textContent === 'Exact replay ✓', selectedRoute.id);
  const routeNumbers = await page
    .locator("#state-list .state-name")
    .allTextContents();
  const orderedNumbers = routeNumbers.map((name) =>
    Number(name.slice("Route ".length)),
  );
  assert.ok(
    orderedNumbers.length > 1,
    "The real cell must contain alternate histories",
  );
  assert.deepEqual(
    orderedNumbers,
    [...orderedNumbers].sort((a, b) => a - b),
    "Routes must read oldest to newest",
  );
  const beforeRoutes = Number(
    (await page.locator("#attempts").innerText()).replaceAll(",", ""),
  );
  await page.locator("#pause").click();
  await page.waitForFunction(
    (before) =>
      Number(
        document.querySelector("#attempts").textContent.replaceAll(",", ""),
      ) >=
      before + 300,
    beforeRoutes,
  );
  await page.locator("#pause").click();
  await page.waitForTimeout(100);
  await page.locator("#state-list .selected").click();
  await page.waitForFunction(
    (id) =>
      document.querySelector("#film-title").dataset.stateId === id &&
      document.querySelector("#verification").textContent === "Exact replay ✓",
    selectedRoute.id,
  );
  assert.equal(
    await page.locator("#film-title").innerText(),
    "History",
    "The History pane keeps its heading while search advances",
  );
  assert.equal(
    await page.locator("#state-list .selected .state-name").innerText(),
    selectedRoute.name,
  );
  const afterNumbers = (
    await page.locator("#state-list .state-name").allTextContents()
  ).map((name) => Number(name.slice("Route ".length)));
  assert.deepEqual(
    afterNumbers,
    [...afterNumbers].sort((a, b) => a - b),
  );
  assert.ok(
    afterNumbers.length <= 13,
    "Only the latest twelve routes and the inspected history stay listed",
  );
  await clickIntroCell();
  await page.waitForFunction(
    () =>
      document.querySelector("#verification").textContent === "Exact replay ✓",
  );
  const total = await page.locator("#scrub").getAttribute("max");
  assert.ok(Number(total) > 0);
  await page.locator("#scrub").fill("0");
  await page.waitForFunction(() =>
    document.querySelector("#frame-label").textContent.startsWith("FRAME 0 /"),
  );
  assert.equal(
    await page.locator("#film").evaluate((c) => c.toDataURL()),
    originImage,
  );
  await page.locator("#scrub").fill(total);
  await page.waitForFunction(
    () =>
      document.querySelector("#verification").textContent === "Exact replay ✓",
  );
  await page.locator("#scrub").fill(String(Number(total) - 1));
  await page.waitForFunction(
    (frame) =>
      document
        .querySelector("#frame-label")
        .textContent.startsWith(
          "FRAME " + Number(frame).toLocaleString() + " /",
        ),
    Number(total) - 1,
  );
  if (Number(total) > 120)
    assert.ok(
      Number(await page.locator("#film").getAttribute("data-seek-start")) > 0,
      "A near-end scrub should reuse a retained replay checkpoint",
    );
  await page.locator("#scrub").fill(total);
  await page.waitForFunction(
    () =>
      document.querySelector("#verification").textContent === "Exact replay ✓",
  );
  const mainTape = JSON.parse(await readFile("tests/fixtures/main-exit.json"));
  const mainFrames = await selectFixture(mainTape, "fixture-main");
  await page.waitForFunction(
    () =>
      document.querySelector("#verification").textContent ===
        "Exact replay ✓" &&
      document.querySelector("#film-title").dataset.stateId === "fixture-main",
  );
  const firstMainImage = await page
    .locator("#film")
    .evaluate((c) => c.toDataURL());
  await selectFixture(mainTape, "fixture-main-cached");
  await page.waitForFunction(
    () =>
      document.querySelector("#verification").textContent ===
        "Exact replay ✓" &&
      document.querySelector("#film-title").dataset.stateId ===
        "fixture-main-cached",
  );
  assert.ok(
    Number(await page.locator("#film").getAttribute("data-seek-start")) >
      mainFrames / 2,
    "Reopening a route must reuse its verified history checkpoints",
  );
  assert.equal(
    await page.locator("#film").evaluate((c) => c.toDataURL()),
    firstMainImage,
    "A cached route must render the same endpoint pixels as replay from the root",
  );
  const roomFrames = new Map();
  emulator.restore(emulatorRoot);
  let frame = 0;
  for (const action of mainTape.actions) {
    for (let i = 0; i < action.frames; i++) {
      emulator.run(action.buttons, 1);
      frame++;
      const o = emulator.observation();
      if (
        [0, 49, 45].includes(o.level) &&
        o.health &&
        isMapEvidence(o, catalog.levels) &&
        !roomFrames.has(o.level)
      )
        roomFrames.set(o.level, frame);
    }
  }
  assert.equal(roomFrames.size, 3);
  for (const [room, frame] of roomFrames) {
    await page.locator("#scrub").fill(String(frame));
    await page.waitForFunction(
      ({ room, frame }) =>
        document.querySelector("#map").dataset.map === String(room) &&
        document
          .querySelector("#frame-label")
          .textContent.startsWith("FRAME " + frame.toLocaleString() + " /"),
      { room, frame },
    );
    assert.equal(
      await page.locator(`.area-label[aria-pressed="true"]`).innerText(),
      { 0: "Introduction", 49: "Garden", 45: "Main level" }[room],
    );
  }
  await page.locator("#scrub").fill(String(mainFrames));
  await page.waitForFunction(
    () =>
      document.querySelector("#verification").textContent === "Exact replay ✓",
  );
  await page.locator("#scrub").fill(String(mainFrames - 20));
  await page.waitForFunction(
    (n) =>
      document
        .querySelector("#frame-label")
        .textContent.startsWith("FRAME " + Number(n).toLocaleString() + " /"),
    mainFrames - 20,
  );
  assert.ok(
    Number(await page.locator("#film").getAttribute("data-seek-start")) >
      mainFrames / 2,
    "Long histories must seek from a nearby cached snapshot",
  );
  await page.locator("#scrub").fill("0");
  await page.waitForFunction(() =>
    document.querySelector("#frame-label").textContent.startsWith("FRAME 0 /"),
  );
  await page.locator("#play").click();
  const normal = await measurePlayback();
  const normalRate = (normal.frames * 1000) / normal.duration;
  assert.ok(
    normalRate >= 45 && normalRate <= 75,
    `1x replay must track wall time: ${normalRate}`,
  );
  await page.waitForFunction(
    () => Number(document.querySelector("#film").dataset.audioFrames) > 0,
  );
  await page.locator("#play").click();
  const pausedAudio = await page
    .locator("#film")
    .getAttribute("data-audio-frames");
  await page.waitForTimeout(100);
  assert.equal(
    await page.locator("#film").getAttribute("data-audio-frames"),
    pausedAudio,
  );
  await page.locator("#speed").selectOption("4");
  await page.locator("#play").click();
  const pacing = await measurePlayback();
  const rate = (pacing.frames * 1000) / pacing.duration;
  assert.ok(
    rate >= 180 && rate <= 300,
    `4x replay must track wall time; got ${rate.toFixed(1)} game frames/second`,
  );
  assert.ok(
    pacing.samples >= 12,
    "Replay must continue presenting frames during playback",
  );
  await page.locator("#play").click();
  await page.locator("#speed").selectOption("1");
  console.log(
    `Audible 1x replay: ${normalRate.toFixed(1)} game frames/s; 4x: ${rate.toFixed(1)}, ${pacing.samples} presentations; cached scrub follows all three rooms.`,
  );
  for (const room of [49, 45]) {
    const beforeDoor = roomFrames.get(room) - 30;
    await page.locator("#scrub").fill(String(beforeDoor));
    await page.waitForFunction(
      (frame) =>
        document
          .querySelector("#frame-label")
          .textContent.startsWith("FRAME " + frame.toLocaleString() + " /"),
      beforeDoor,
    );
    await page.locator("#play").click();
    await page.waitForFunction(
      (room) => document.querySelector("#map").dataset.map === String(room),
      room,
    );
    assert.ok(
      Number(await page.locator("#scrub").inputValue()) >= roomFrames.get(room),
    );
    await page.locator("#play").click();
  }
  const nextTape = JSON.parse(
    await readFile("tests/fixtures/level-two-2.json"),
  );
  await selectFixture(nextTape, "fixture-level-two");
  await page.waitForFunction(
    () =>
      document.querySelector("#verification").textContent ===
        "Exact replay ✓" && document.querySelector("#map").dataset.map === "1",
  );
  assert.equal(await page.locator("#goal-title").innerText(), "World 1 – Level 2");
  await page.locator("#scrub").fill("0");
  await page.waitForFunction(
    () =>
      document.querySelector("#map").dataset.map === "0" &&
      document
        .querySelector("#frame-label")
        .textContent.startsWith("FRAME 0 /"),
  );
  assert.equal(await page.locator("#goal-title").innerText(), "World 1 – Level 1");
  const longFrames = await selectFixture(
    {
      actions: [
        ...mainTape.actions,
        ...Array(500).fill({ buttons: 0, frames: 120 }),
      ],
    },
    "fixture-long",
  );
  await page.waitForFunction(
    () => document.querySelector("#verification").textContent === "Replaying",
  );
  await page.locator("#scrub").fill("0");
  await page.waitForFunction(() =>
    document.querySelector("#frame-label").textContent.startsWith("FRAME 0 /"),
  );
  assert.equal(await page.locator("#error").isVisible(), false);
  assert.ok(longFrames > 60000);
  await selectFixture({ actions: [] }, "fixture-root");
  await page.waitForFunction(
    () =>
      document.querySelector("#verification").textContent === "Original game" &&
      document.querySelector("#scrub").max === "0",
  );
  await page.locator('[data-viz="heat"]').click();
  const originalAttempts = await page.locator("#attempts").innerText();
  const originalCells = await page.locator("#cells").innerText();
  await page.mouse.move(0, 0);
  await page.waitForTimeout(1700);
  const pausedHeat = await page
    .locator("#map")
    .evaluate((canvas) => canvas.toDataURL());
  await page.waitForTimeout(6100);
  assert.equal(
    await page.locator("#map").evaluate((canvas) => canvas.toDataURL()),
    pausedHeat,
    "The rendered heat overlay must stay frozen while the search is paused",
  );
  const originalOverlay = await page
    .locator('.map-row[data-map="0"] canvas')
    .evaluate((canvas) => window.novaTestHeatPixels(canvas));
  const manualAudioStart = Number(
    await page.locator("#film").getAttribute("data-audio-frames"),
  );
  await page.locator("#take-control").click();
  assert.equal(await page.locator("#take-control").isVisible(), false);
  await page.locator("#discard-branch").hover();
  const stopContrast = await page
    .locator("#discard-branch")
    .evaluate((button) => {
      const styles = getComputedStyle(button);
      const luminance = (color) =>
        color
          .match(/\d+/g)
          .slice(0, 3)
          .map((n) => Number(n) / 255)
          .map((n) => (n <= 0.04045 ? n / 12.92 : ((n + 0.055) / 1.055) ** 2.4))
          .reduce((sum, n, i) => sum + n * [0.2126, 0.7152, 0.0722][i], 0);
      const colors = [
        luminance(styles.color),
        luminance(styles.backgroundColor),
      ].sort((a, b) => b - a);
      return (colors[0] + 0.05) / (colors[1] + 0.05);
    });
  assert.ok(
    stopContrast >= 4.5,
    "Discard branch must stay readable under the cursor",
  );
  await page.keyboard.down("ArrowRight");
  await page.waitForTimeout(500);
  await page.keyboard.up("ArrowRight");
  await page.keyboard.down("KeyZ");
  await page.waitForTimeout(80);
  await page.keyboard.up("KeyZ");
  await page.waitForFunction(
    (start) =>
      Number(document.querySelector("#film").dataset.audioFrames) > start,
    manualAudioStart,
  );
  await page.locator("#sound").click();
  assert.equal(
    await page.locator("#sound").getAttribute("aria-label"),
    "Unmute game audio",
  );
  await page.locator("#sound").click();
  await page.keyboard.press("Escape");
  assert.equal(
    await page.locator("#take-control").getAttribute("aria-pressed"),
    "false",
  );
  await page.evaluate(() => {
    const worker = window.novaTestWorker,
      handler = worker.onmessage;
    worker.onmessage = (event) => {
      handler(event);
      if (event.data.type === "ready") {
        window.novaTestReadyCells =
          document.querySelector("#cells").textContent;
        window.novaTestReady = {
          root: { ...event.data.state, snapshot: [...event.data.state.snapshot] },
          states: document.querySelector("#states").textContent,
          work: document.querySelector("#work").textContent,
          active: event.data.active,
          cells: window.novaTestReadyCells,
          attempts: document.querySelector("#attempts").textContent,
          paneHidden: document.querySelector("#inspector").hidden,
          paused: event.data.paused,
          branchFocused:
            document.activeElement === document.querySelector("#branch-tree button[aria-pressed=true]"),
          overlay: (() => {
            const canvas = document.querySelector(
              '.map-row[data-map="0"] canvas',
            );
            return window.novaTestHeatPixels(canvas);
          })(),
        };
      }
    };
  });
  assert.equal(
    await page.locator("#search-here").innerText(),
    "↗ Branch search from here",
  );
  await page.locator("#search-here").click();
  await page.waitForFunction(() => !!window.novaTestFork);
  const manualTape = await page.evaluate(() => window.novaTestFork.tape);
  const manualSeed = await page.evaluate(() => window.novaTestFork.seed);
  assert.ok(manualTape.actions.some((a) => a.buttons & 128));
  assert.ok(manualTape.actions.some((a) => a.buttons & 1));
  assert.ok(manualTape.actions.reduce((n, a) => n + a.frames, 0) > 12);
  emulator.restore(emulatorRoot);
  for (const a of manualTape.actions) emulator.run(a.buttons, a.frames);
  assert.equal(
    await snapshotHash(emulator.capture()),
    manualTape.endpoint_sha256,
    "Human inputs must reproduce the exact rendered endpoint passed to the worker",
  );
  const manualFrames = manualTape.actions.reduce((n, a) => n + a.frames, 0);
  assert.equal(await page.locator("#branches").isVisible(), true);
  assert.match(
    await page.locator("#branches").textContent(),
    /Searches/,
  );
  await page.waitForFunction(
    () => document.querySelector("#branch-tree button[aria-pressed=true]").dataset.search === "1",
  );
  assert.equal(
    await page.locator("#inspector").isVisible(),
    false,
    "A successful handoff must close the pane",
  );
  await page.waitForFunction(
    () => document.querySelector("#map").dataset.originPulse === "true",
  );
  assert.equal(
    await page.locator("#map").getAttribute("data-origin-frame"),
    String(manualFrames),
  );
  await page.waitForFunction(
    () => Number(document.querySelector("#map").dataset.tracePoints) > 1,
  );
  assert.match(
    await page.locator("#branch-feedback").textContent(),
    /Branch 1 is searching/,
  );
  assert.equal(
    await page.evaluate(() => window.novaTestReady.branchFocused),
    true,
    "A newly admitted branch must focus its enabled selector",
  );
  assert.equal(
    await page.evaluate(() => window.novaTestReadyCells),
    "1",
    "A new branch must initially show only its actual root cell, without the parent's heat",
  );
  await page.waitForFunction(
    () =>
      document.querySelector("#verification").textContent === "Exact replay ✓",
  );
  await page.waitForFunction(
    () =>
      Number(
        document.querySelector("#attempts").textContent.replaceAll(",", ""),
      ) > 100,
  );
  await page.locator("#pause").click();
  await page.waitForTimeout(100);
  const branchCells = await page.locator("#cells").innerText();
  await page.waitForFunction(
    () => document.querySelector("#map").dataset.originPulse === "false",
  );
  assert.ok(
    Number(await page.locator("#map").getAttribute("data-trace-points")) > 1,
    "The prefix trail must outlast the origin ripple while the pane stays closed",
  );
  await openCurrentCell();
  await page.waitForFunction(
    () => !document.querySelector("#take-control").disabled,
  );
  const position = await page.locator("#details b").first().textContent();
  const [bx, by] = position.split(",").map(Number);
  const branchClick = await page.locator("#map").evaluate(
    (c, p) => {
      const r = c.getBoundingClientRect(),
        w = Number(c.dataset.mapWidth),
        h = Number(c.dataset.mapHeight),
        scale = Math.min(c.width / w, c.height / h) * Number(c.dataset.zoom),
        px = Math.floor((p.x % w) / 32) * 32 + 16,
        py = Math.floor((Math.floor(p.x / w) * 224 + p.y) / 32) * 32 + 8;
      return {
        x:
          (((px - Number(c.dataset.centerX)) * scale + c.width / 2) * r.width) /
          c.width,
        y:
          (((py - Number(c.dataset.centerY)) * scale + c.height / 2) *
            r.height) /
          c.height,
      };
    },
    { x: bx, y: by },
  );
  await page.locator("#map").click({ position: branchClick });
  await page.waitForFunction(
    () =>
      document
        .querySelector("#state-list .selected")
        ?.dataset.stateId.startsWith("1:") &&
      document.querySelector("#verification").textContent === "Exact replay ✓",
  );
  const childState = await page.evaluate(() => window.novaTestStates[0]);
  assert.equal(
    await page.locator("#film-title").innerText(),
    "History",
    "The History pane keeps its heading for search descendants",
  );
  const childTape = {
    actions: childState.actions,
    endpoint_sha256: await snapshotHash(
      new Uint8Array(Object.values(childState.snapshot)),
    ),
  };
  assert.ok(
    childTape.actions.reduce((n, a) => n + a.frames, 0) >
      manualTape.actions.reduce((n, a) => n + a.frames, 0),
    "The selected branch history must be an actual search descendant",
  );
  assert.deepEqual(
    childTape.actions.slice(0, manualTape.actions.length),
    manualTape.actions,
  );
  emulator.restore(emulatorRoot);
  for (const a of childTape.actions) emulator.run(a.buttons, a.frames);
  assert.equal(
    await snapshotHash(emulator.capture()),
    childTape.endpoint_sha256,
    "Search descendants must include and reproduce the human input prefix",
  );
  const nestedFrame = childState.frames - 1;
  await page.locator("#scrub").fill(String(nestedFrame));
  await page.waitForFunction((frame) =>
    document.querySelector("#frame-label").textContent.startsWith(`FRAME ${frame} /`) &&
    !document.querySelector("#take-control").disabled,
    nestedFrame,
  );
  await page.evaluate(() => {
    document.querySelector('#take-control').click();
    window.dispatchEvent(new KeyboardEvent('keydown', {code: 'Escape'}));
    window.novaTestFork = window.novaTestReady = null;
  });
  await page.locator("#search-here").click();
  await page.waitForFunction(() => window.novaTestReady?.active === 2 && !!window.novaTestNestedBatch);
  const nested = await page.evaluate(() => ({ ready: window.novaTestReady, tape: window.novaTestFork.tape, seed: window.novaTestFork.seed, batch: window.novaTestNestedBatch }));
  const nestedPrefix = prefixAt(childState.actions, nestedFrame);
  assert.deepEqual(nested.tape.actions, nestedPrefix, "A nested fork must use the selected descendant's scrubbed frame");
  assert.equal(nested.tape.branch.parent_state, childState.id);
  assert.equal(nested.tape.branch.parent_frame, nestedFrame);
  assert.equal(nested.ready.root.id, "2:0");
  assert.equal(nested.ready.attempts, "0");
  assert.equal(nested.ready.work, "0");
  assert.ok(nested.seed > manualSeed, "A nested fork must have its own fresh random seed");
  assert.equal(nested.ready.states, "1", "Nested search starts with only its new root, not the parent's archive");
  assert.equal(nested.ready.cells, "1", "Nested search starts with fresh heat at its actual root");
  assert.deepEqual(nested.ready.root.actions, nestedPrefix);
  assert.equal(nested.ready.root.frames, nestedFrame);
  emulator.restore(emulatorRoot);
  for (const a of nestedPrefix) emulator.run(a.buttons, a.frames);
  assert.deepEqual(new Uint8Array(nested.ready.root.snapshot), emulator.capture(), "Nested root must be the exact selected game snapshot");
  assert.equal(await snapshotHash(emulator.capture()), nested.tape.endpoint_sha256);
  const nestedObservation = emulator.observation();
  assert.deepEqual(nested.batch.motionStart.slice(1, 4), [nestedObservation.level, nestedObservation.x, nestedObservation.y], "The first nested attempt must restore its new root position");
  assert.equal(nested.batch.executions, 2);
  const admitted = new Set(["2:0", ...nested.batch.points.filter(p => p.retained !== null).map(p => p.retained)]);
  assert.equal(nested.batch.states, admitted.size, "Retained-state counts must belong to the active search");
  await page.waitForFunction(
    () => window.novaTestNestedRetained?.startsWith("2:"),
    undefined,
    { timeout: 60000 },
  );
  const nestedId = await page.evaluate(() => window.novaTestNestedRetained);
  await page.evaluate((id) => window.novaTestWorker.postMessage({ type: "states", ids: [id], request: -10 }), nestedId);
  await page.waitForFunction((id) => window.novaTestStates?.[0]?.id === id, nestedId);
  const nestedDescendant = await page.evaluate(() => window.novaTestStates[0]);
  assert.deepEqual(nestedDescendant.actions.slice(0, nestedPrefix.length), nestedPrefix);
  assert.ok(nestedDescendant.frames > nestedFrame);
  emulator.restore(emulatorRoot);
  for (const a of nestedDescendant.actions) emulator.run(a.buttons, a.frames);
  assert.deepEqual(emulator.capture(), new Uint8Array(Object.values(nestedDescendant.snapshot)), "A nested descendant must replay the complete original, human and branch prefix");
  await page.locator("#pause").click();
  await page.waitForTimeout(100);
  console.log("Nested branch: exact scrubbed root, fresh archive/counters/heat, first restored position and replayable descendant passed.");

  assert.equal(await page.locator('li[data-search-node="1"] > ol > li[data-search-node="2"]').count(), 1, 'Nested branches must retain their actual parent in the tree');
  await page.mouse.move(0, 0);
  await page.waitForTimeout(1800);
  const activeBranchBeforeHover = await page.locator('#branch-tree button[aria-pressed=true]').getAttribute('data-search');
  const branchAttemptsBeforeHover = await page.locator('#attempts').innerText();
  const branchImageBeforeHover = await page.locator('#map').evaluate(c => c.toDataURL());
  await page.locator('button[data-search="1"]').hover();
  await page.waitForFunction(() => document.querySelector('#map').dataset.previewSearch === '1' && Number(document.querySelector('#map').dataset.tracePoints) > 1);
  assert.equal(await page.locator('#branch-tree button[aria-pressed=true]').getAttribute('data-search'), activeBranchBeforeHover);
  assert.equal(await page.locator('#attempts').innerText(), branchAttemptsBeforeHover);
  await page.mouse.move(0, 0);
  await page.waitForFunction(() => document.querySelector('#map').dataset.previewSearch === '');
  assert.equal(await page.locator('#map').evaluate(c => c.toDataURL()), branchImageBeforeHover, 'Previewing another branch must restore the untouched active heat and origin trail');
  await page.screenshot({ path: 'test-results/branches-desktop.png' });

  async function switchSearch(id, cells, attempts, overlay) {
    await page.evaluate(() => {
      window.novaTestReady = null;
    });
    await page.locator(`#branch-tree button[data-search="${id}"]`).click();
    await page.waitForFunction(
      (active) => window.novaTestReady?.active === active,
      id,
    );
    const restored = await page.evaluate(() => window.novaTestReady);
    assert.equal(
      restored.paneHidden,
      true,
      "Switching branches must close the history pane",
    );
    assert.equal(
      restored.paused,
      false,
      "Switching branches must resume exploration",
    );
    assert.equal(
      restored.cells,
      cells,
      "Switching restores only this branch's heat without an extra root visit",
    );
    if (attempts !== undefined) assert.equal(restored.attempts, attempts);
    if (overlay) {
      assert.equal(restored.overlay.length, overlay.length);
      assert.ok(
        restored.overlay.every(
          (channel, i) => Math.abs(channel - overlay[i]) <= 3,
        ),
        "Returning to the original search must preserve its rendered cell colors while exploration resumes",
      );
    }
    await page.waitForFunction(
      (before) =>
        Number(
          document.querySelector("#attempts").textContent.replaceAll(",", ""),
        ) > before,
      Number(restored.attempts.replaceAll(",", "")),
    );
    assert.equal(await page.locator("#inspector").isVisible(), false);
    assert.equal(
      await page.locator("#pause").getAttribute("aria-label"),
      "Pause Search",
    );
    await page.locator("#pause").click();
    await page.waitForTimeout(100);
    return page.locator("#cells").innerText();
  }
  const resumedOriginalCells = await switchSearch(
    0,
    originalCells,
    originalAttempts,
    originalOverlay,
  );
  await page.waitForFunction(() =>
    [...document.querySelectorAll(".map-row canvas")].every(
      (c) => c.dataset.markerFrame === "",
    ),
  );
  await switchSearch(1, branchCells);
  await page.waitForFunction(
    () => Number(document.querySelector("#map").dataset.tracePoints) > 1,
  );
  assert.equal(
    await page.locator("#map").getAttribute("data-origin-frame"),
    String(manualFrames),
  );
  await switchSearch(0, resumedOriginalCells);
  await page.waitForFunction(
    () =>
      document.querySelector("#verification").textContent === "Original game",
  );
  await page.locator('.area-zoom[data-map="49"]').click();
  await page.locator('.area-zoom[data-map="45"]').click();
  await page.waitForFunction(
    () =>
      document.querySelector('.map-row[data-map="49"] canvas').dataset.zoom ===
        "2" &&
      document.querySelector('.map-row[data-map="45"] canvas').dataset.zoom ===
        "2",
  );
  assert.equal(
    await page.locator("#map").getAttribute("data-zoom"),
    "1",
    "Zooming another room must leave Introduction at overview scale",
  );
  await page.locator('.area-zoom[data-map="49"]').click();
  await page.locator('.area-zoom[data-map="49"]').click();
  await page.locator('.area-zoom[data-map="45"]').click();
  await page.locator('.area-zoom[data-map="45"]').click();
  await openCurrentCell();
  await page.locator("#zoom").click();
  await page.waitForTimeout(50);
  assert.ok(
    await page.locator("#map").evaluate((canvas) => {
      const data = canvas
        .getContext("2d")
        .getImageData(0, 0, canvas.width, canvas.height).data;
      for (let i = 0; i < data.length; i += 4)
        if (data[i] === 255 && data[i + 1] === 245 && data[i + 2] === 204)
          return true;
      return false;
    }),
    "Zoom must keep the selected ground state visible",
  );
  await page.locator("#zoom").click();
  await page.locator("#zoom").click();
  for (const file of [
    "licenses/CREDITS.md",
    "licenses/harmony-source.tar.gz",
    "licenses/rust-dependencies.tar.gz",
    "licenses/nova-source.tar.gz",
    "licenses/quicknes-source.tar.gz",
    "maps/45.png",
    "maps/49.png",
    "maps.json",
  ]) {
    const response = await page.request.get(new URL(file, page.url()).href);
    assert.equal(response.status(), 200);
    if (file.endsWith(".tar.gz")) {
      const body = await response.body(),
        tar =
          body.subarray(0, 2).toString("hex") === "1f8b"
            ? gunzipSync(body)
            : body;
      assert.equal(tar.subarray(257, 262).toString(), "ustar");
    }
  }
  await page.locator("#credits").click();
  assert.match(
    await page.locator("#info-content").innerText(),
    /CC BY-NC-SA 4.0/,
  );
  await page.locator("#close-info").click();
  assert.equal(await page.locator('.art-credit').count(), 0);
  assert.match(await page.locator('.exploration .game-attribution').innerText(), /NovaSquirrel.*CC BY-NC-SA 4.0/);
  assert.equal(await page.locator('.game-attribution').count(), 1);
  assert.equal(await page.locator('footer').innerText(), 'Credits & source');
  assert.equal(await page.locator('.exploration-heading').innerText(), 'Exploration');
  assert.equal(await page.locator('.history-heading h2').innerText(), 'History');
  assert.equal(await page.getByText('Nova explorer', { exact: true }).count(), 0);

  for (const text of [
    "How it works",
    "Everything runs in your browser",
    "A LIVING MAP OF POSSIBILITIES",
    "Watch the search find its way",
    "Heat cools. Histories stay.",
    "EXPLORATION NOTES",
    "Watch Main level arrival",
    "Find the exit",
    "Garden · Visited",
  ])
    assert.equal(await page.getByText(text, { exact: false }).count(), 0);
  assert.equal(await page.locator("#goal-status").isVisible(), false);
  assert.equal(await page.locator(".area-state").count(), 0);
  assert.equal(
    await page
      .locator(
        "#heat-toggle,#fit,#left,#right,#seed-label,#inspect-open,#goal-count",
      )
      .count(),
    0,
  );
  assert.equal(await page.locator("#pause svg").count(), 1);
  assert.equal(
    await page.locator("#pause").getAttribute("aria-label"),
    "Resume Search",
  );
  assert.equal(await page.locator("#reset").getAttribute("aria-label"), "Restart Search");
  assert.equal(await page.locator(".map-row").count(), 3);
  for (const id of [0, 49, 45])
    assert.equal(
      await page.locator(`.map-row[data-map="${id}"] canvas`).first().isVisible(),
      true,
    );
  await page.locator('.map-row[data-map="49"] canvas').first().focus();
  await page.keyboard.press("ArrowRight");
  await page.keyboard.press("Enter");
  assert.equal(await page.locator("#map").getAttribute("data-map"), "49");
  assert.equal(
    await page.locator("#map").evaluate((c) => c === document.activeElement),
    true,
    "Entering a cell on a stacked map must preserve keyboard focus",
  );
  if (await page.locator("#inspector").isVisible())
    await page.locator("#close-inspector").click();
  await page.locator('.map-row[data-map="45"] .area-label').click();
  assert.equal(await page.locator("#map").getAttribute("data-map"), "45");
  assert.match(await page.locator("#map-label").innerText(), /MAIN LEVEL/);
  await page.locator('.map-row[data-map="49"] .area-label').click();
  assert.match(await page.locator("#map-label").innerText(), /GARDEN/);
  assert.equal(await page.locator('#worlds,.atlas,.map-card').count(), 0);
  await page.locator('.map-row[data-map="45"] .area-label').click();
  await page.locator("#zoom").click();
  await page.locator("#map").scrollIntoViewIfNeeded();
  const before = await page.locator("#map").evaluate((c) => c.toDataURL()),
    bounds = await page.locator("#map").boundingBox();
  await page.mouse.move(
    bounds.x + bounds.width / 2,
    bounds.y + bounds.height / 2,
  );
  await page.mouse.down();
  await page.mouse.move(
    bounds.x + bounds.width / 2 - 100,
    bounds.y + bounds.height / 2,
    { steps: 5 },
  );
  await page.mouse.up();
  await page.waitForTimeout(100);
  assert.ok(
    (await page.locator("#map").evaluate((c) => c.toDataURL())) !== before,
    "Dragging the zoomed map must move the view",
  );
  await page.locator("#reset").click();
  await page.waitForFunction(
    () =>
      document.querySelector("#status").textContent === "Exploring" &&
      document.querySelector("#map").dataset.map === "0",
  );
  assert.equal(
    await page.locator("#pause").getAttribute("aria-label"),
    "Pause Search",
  );
  await page.setViewportSize({ width: 390, height: 844 });
  assert.equal(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
    true,
  );
  assert.equal(
    await page.locator("footer").isVisible(),
    true,
  );
  await page.locator("#memory-limit").evaluate((notice) => {
    notice.textContent = "Memory or search limit reached";
    notice.hidden = false;
  });
  assert.equal(
    await page.locator("#memory-limit").isVisible(),
    true,
    "Mobile must show the budget-stop notice",
  );
  await page.locator("#memory-limit").evaluate((notice) => {
    notice.hidden = true;
  });
  await openCurrentCell();
  await page.waitForFunction(
    () => !document.querySelector("#take-control").disabled,
  );
  await page.setViewportSize({ width: 320, height: 640 });
  const compactPhone = await page.locator("#inspector").evaluate((panel) => {
    const action = panel.querySelector("#take-control").getBoundingClientRect(),
      sheet = panel.getBoundingClientRect();
    return (
      action.top >= sheet.top &&
      action.bottom <= sheet.bottom &&
      panel.scrollWidth <= panel.clientWidth
    );
  });
  assert.equal(
    compactPhone,
    true,
    "Play must be visible without scrolling on a small phone",
  );
  await page.setViewportSize({ width: 390, height: 844 });
  const phonePosition = (await page.locator("#details b").first().textContent())
    .split(",")
    .map(Number);
  const phoneCell = await page.locator("#map").evaluate((canvas, position) => {
    const w = Number(canvas.dataset.mapWidth),
      h = Number(canvas.dataset.mapHeight),
      scale = Math.min(canvas.width / w, canvas.height / h),
      rect = canvas.getBoundingClientRect();
    const x = Math.floor((position[0] % w) / 32) * 32 + 16,
      y =
        Math.floor((Math.floor(position[0] / w) * 224 + position[1]) / 32) *
          32 +
        8;
    return {
      x:
        (rect.width * ((canvas.width - w * scale) / 2 + x * scale)) /
        canvas.width,
      y:
        (rect.height * ((canvas.height - h * scale) / 2 + y * scale)) /
        canvas.height,
    };
  }, phonePosition);
  await page.locator("#map").click({ position: phoneCell });
  await page.waitForFunction(
    () =>
      document.querySelector("#verification").textContent === "Exact replay ✓",
  );
  const phoneStateId = await page
    .locator("#state-list .selected")
    .getAttribute("data-state-id");
  await selectFixture(mainTape, phoneStateId);
  await page.waitForFunction(
    (id) =>
      document.querySelector("#verification").textContent ===
        "Exact replay ✓" &&
      document.querySelector("#film-title").dataset.stateId === id &&
      !document.querySelector("#take-control").disabled,
    phoneStateId,
  );
  await page.evaluate(() => {
    const spacer = document.createElement('div');
    spacer.id = 'test-scroll-space';
    spacer.style.height = '100vh';
    document.querySelector('main').append(spacer);
    const map = document.querySelector("#map").getBoundingClientRect();
    window.scrollBy(0, map.bottom + 100);
  });
  assert.ok(
    await page
      .locator("#map")
      .evaluate((map) => map.getBoundingClientRect().bottom < 0),
  );
  await page.locator("#state-list .selected").click();
  await page.waitForFunction(
    () => !document.querySelector("#take-control").disabled,
  );
  await page.locator("#film-title").hover();
  await page.waitForFunction(() => document.querySelector(`.area-map[data-marker-frame="${document.querySelector("#scrub").value}"]`));
  const phoneMap = await page.locator("#inspector").evaluate((panel) => {
    const sheet = panel.getBoundingClientRect(),
      map = document.querySelector(`.area-map[data-marker-frame="${document.querySelector("#scrub").value}"]`).getBoundingClientRect();
    return {
      height: sheet.height / innerHeight,
      topGap:
        panel.querySelector(".screen").getBoundingClientRect().top - sheet.top,
      visibleMap: map.top >= 0 && map.bottom <= sheet.top,
      preview: panel.querySelector("#film").getBoundingClientRect().width,
    };
  });
  assert.ok(
    phoneMap.height <= 0.45,
    "The compact pane must leave most of the viewport to the route map",
  );
  assert.ok(
    phoneMap.topGap <= 42,
    "The preview should start near the top of the pane",
  );
  assert.ok(
    phoneMap.visibleMap,
    "Selecting a history in the same room must reveal its map above the pane",
  );
  assert.ok(
    phoneMap.preview <= 110,
    "The compact pane should use a small preview beside its controls",
  );
  await page.locator("#test-scroll-space").evaluate(e => e.remove());
  const collapsedHeight = (await page.locator("#inspector").boundingBox())
    .height;
  await page.locator("#expand-inspector").click();
  assert.ok(
    (await page.locator("#inspector").boundingBox()).height > collapsedHeight,
  );
  assert.equal(
    await page
      .locator("#inspector")
      .evaluate(
        (panel) =>
          panel.scrollWidth <= panel.clientWidth &&
          [...panel.querySelectorAll(".state")].every(
            (b) => b.scrollWidth <= b.clientWidth,
          ),
      ),
    true,
    "Phone histories must not overflow the sheet",
  );
  await page.locator("#expand-inspector").click();
  const mapVisible = await page
    .locator('.area-map[data-marker-frame]:not([data-marker-frame=""])').first()
    .evaluate(
      (map) =>
        map.getBoundingClientRect().bottom <=
        document.querySelector("#inspector").getBoundingClientRect().top,
    );
  assert.ok(
    mapVisible,
    "Collapsing after playback must reveal the selected path again",
  );
  await page.locator("#take-control").click();
  assert.equal(
    await page.locator("#expand-inspector").getAttribute("aria-expanded"),
    "true",
    "Playing should expand the phone sheet automatically",
  );
  assert.equal(
    await page.locator("#search-here").innerText(),
    "↗ Branch search from here",
  );
  const touchRight = page.getByRole("button", {
    name: "Move right",
    exact: true,
  });
  await touchRight.scrollIntoViewIfNeeded();
  const touchBounds = await touchRight.boundingBox();
  await page.mouse.move(touchBounds.x + 20, touchBounds.y + 20);
  await page.mouse.down();
  await page.waitForTimeout(160);
  await page.mouse.up();
  await page.evaluate(() => window.dispatchEvent(new Event("blur")));
  assert.equal(
    await page.locator("#take-control").getAttribute("aria-pressed"),
    "false",
  );
  const heldFrame = await page.locator("#frame-label").innerText();
  await page.waitForTimeout(100);
  assert.equal(
    await page.locator("#frame-label").innerText(),
    heldFrame,
    "Losing focus must stop manual play and release controls",
  );
  const phoneBranchId = String(
    await page.locator("#branch-tree button[data-search]").count(),
  );
  await page.locator("#search-here").click();
  await page.waitForFunction(
    (branch) =>
      document.querySelector("#branch-tree button[aria-pressed=true]").dataset.search === branch &&
      document.querySelector("#inspector").hidden,
    phoneBranchId,
  );
  await page.waitForFunction(
    () =>
      document.querySelector('.map-row[data-map="45"] canvas[data-origin-frame]:not([data-origin-frame=""])').dataset
        .originPulse === "true",
  );
  const originRoomVisible = await page
    .locator('.map-row[data-map="45"] .room-part:has(canvas[data-origin-frame]:not([data-origin-frame=""]))')
    .evaluate((row) => {
      const bounds = row.getBoundingClientRect();
      return bounds.top >= 0 && bounds.bottom <= innerHeight;
    });
  assert.equal(
    originRoomVisible,
    true,
    "Handoff from expanded phone gameplay must reveal the actual origin section",
  );
  await page.waitForFunction(
    () =>
      document.querySelector("#verification").textContent === "Exact replay ✓",
  );
  await page.locator("#pause").click();
  await page.waitForFunction(
    () =>
      document.querySelector('.map-row[data-map="45"] canvas[data-origin-frame]:not([data-origin-frame=""])').dataset
        .originPulse === "false",
  );
  assert.ok(
    Number(
      await page
        .locator('.map-row[data-map="45"] canvas[data-origin-frame]:not([data-origin-frame=""])')
        .getAttribute("data-trace-points"),
    ) > 1,
  );
  await mkdir("test-results", { recursive: true });
  await page.screenshot({
    path: "test-results/mobile.png",
    fullPage: true,
    animations: "disabled",
  });
  await page.setViewportSize({ width: 1440, height: 1100 });
  await page.waitForTimeout(2500);
  await page.screenshot({
    path: "test-results/desktop.png",
    fullPage: true,
    animations: "disabled",
  });
  assert.equal(await page.locator("#error").isVisible(), false);
  assert.deepEqual(errors, []);
  console.log(
    "Browser search, exact replay, room-following scrubbing, audible 1x/4x history, manual branching, simplified controls, reset and mobile layout passed.",
  );
} catch (e) {
  console.error(
    await page.locator("#verification").textContent(),
    await page.locator("#frame-label").textContent(),
    await page.locator("#error").textContent(),
  );
  await page.screenshot({ path: "/tmp/nova-play-failure.png", fullPage: true });
  throw e;
} finally {
  await page.goto("about:blank");
  await page.close();
  await browser.close();
}
