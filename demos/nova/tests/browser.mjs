// SPDX-License-Identifier: AGPL-3.0-or-later
import { chromium } from "@playwright/test";
import assert from "node:assert/strict";
import { readFile, mkdir } from "node:fs/promises";
import { pathToFileURL } from "node:url";
import { gunzipSync } from "node:zlib";
import { createEngine } from "../src/emulator.js";
import { snapshotHash } from "../src/media.js";
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
try {
  await page.goto(process.env.DEMO_URL || "http://127.0.0.1:4173");
  await page.waitForFunction(
    () =>
      document.querySelector("#verification")?.textContent === "Original game",
  );
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
  await page.locator("#pause").click();
  await page.waitForTimeout(200);
  const attempts = await page.locator("#attempts").innerText();
  await page.waitForTimeout(200);
  assert.equal(await page.locator("#attempts").innerText(), attempts);
  assert.equal(await page.locator("#inspector").isVisible(), false);
  await page.locator("#inspect-open").click();
  const [x, y] = (await page.locator("#details b").first().innerText())
    .split(",")
    .map(Number);
  const click = await page.locator("#map").evaluate(
    (canvas, position) => {
      const width = Number(canvas.dataset.mapWidth),
        height = Number(canvas.dataset.mapHeight),
        scale = Math.min(canvas.width / width, canvas.height / height),
        rect = canvas.getBoundingClientRect();
      const px = Math.floor((position.x % width) / 32) * 32 + 16,
        py =
          Math.floor((Math.floor(position.x / width) * 224 + position.y) / 32) *
            32 +
          8;
      return {
        x:
          (rect.width * ((canvas.width - width * scale) / 2 + px * scale)) /
          canvas.width,
        y:
          (rect.height * ((canvas.height - height * scale) / 2 + py * scale)) /
          canvas.height,
      };
    },
    { x, y },
  );
  await page.locator("#map").click({ position: click });
  await page.waitForFunction(
    (expected) =>
      document.querySelector("#cell-title").textContent === expected,
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
    () => Number(document.querySelector("#map").dataset.tracePoints) > 1,
  );
  const selected = await page.locator("#state-list .selected").innerText();
  assert.match(selected, /#\d+/);
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
  const pngPromise = page.waitForEvent("download");
  await page.locator("#screenshot").click();
  const shot = await pngPromise;
  const bytes = await readFile(await shot.path());
  assert.equal(bytes.subarray(1, 4).toString(), "PNG");
  assert.match(bytes.toString(), /NovaSquirrel/);
  assert.match(bytes.toString(), /CC BY-NC-SA 4.0/);
  const historyPromise = page.waitForEvent("download");
  await page.locator("#export").click();
  const history = await historyPromise;
  const path = await history.path();
  const tape = JSON.parse(await readFile(path));
  assert.ok(tape.actions.length > 0);
  const status = await page.locator("#status").innerText();
  await page.locator("#history-file").setInputFiles({
    name: "tampered.json",
    mimeType: "application/json",
    buffer: Buffer.from(
      JSON.stringify({ ...tape, endpoint_sha256: "0".repeat(64) }),
    ),
  });
  await page.waitForFunction(() =>
    document.querySelector("#error").textContent.includes("checksum mismatch"),
  );
  assert.equal(await page.locator("#status").innerText(), status);
  await page.locator("#history-file").setInputFiles(path);
  await page.waitForFunction(
    () =>
      document.querySelector("#verification").textContent ===
      "Saved controller history",
  );
  assert.equal(await page.locator("#error").isVisible(), false);
  const localBase = pathToFileURL(process.cwd() + "/public/");
  const emulator = await createEngine(localBase, {
    rom: new Uint8Array(await readFile(new URL("nova.nes", localBase))),
    wasmBinary: await readFile(new URL("engine/quicknes.wasm", localBase)),
  });
  emulator.boot();
  const emulatorRoot = emulator.capture();
  const rootHash = await snapshotHash(emulatorRoot);
  const longActions = [
    ...tape.actions,
    ...Array(500).fill({ buttons: 0, frames: 120 }),
  ];
  for (const action of longActions) emulator.run(action.buttons, action.frames);
  const longTape = {
    ...tape,
    actions: longActions,
    endpoint_sha256: await snapshotHash(emulator.capture()),
  };
  await page.locator("#history-file").setInputFiles({
    name: "long-valid.json",
    mimeType: "application/json",
    buffer: Buffer.from(JSON.stringify(longTape)),
  });
  await page.waitForFunction(
    () => document.querySelector("#verification").textContent === "Replaying",
  );
  await page.locator("#scrub").fill("0");
  await page.waitForTimeout(200);
  assert.equal(await page.locator("#error").isVisible(), false);
  assert.equal(await page.locator("#status").innerText(), status);
  await page.locator("#history-file").setInputFiles(path);
  await page.waitForFunction(
    () =>
      document.querySelector("#verification").textContent ===
      "Saved controller history",
  );
  const mainTape = JSON.parse(await readFile("tests/fixtures/main-exit.json"));
  const mainFrames = mainTape.actions.reduce((n, a) => n + a.frames, 0);
  await page.locator("#history-file").setInputFiles({
    name: "main.json",
    mimeType: "application/json",
    buffer: Buffer.from(JSON.stringify(mainTape)),
  });
  await page.waitForFunction(
    (n) =>
      document.querySelector("#verification").textContent ===
        "Saved controller history" &&
      document.querySelector("#scrub").max === String(n),
    mainFrames,
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
  const pacing = await page.evaluate(
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
  console.log(
    `4x replay: ${rate.toFixed(1)} game frames/s, ${pacing.samples} presentations in ${pacing.duration.toFixed(0)} ms; checkpoint seek reused a late snapshot.`,
  );
  await page.locator("#history-file").setInputFiles({
    name: "root.json",
    mimeType: "application/json",
    buffer: Buffer.from(
      JSON.stringify({ ...tape, actions: [], endpoint_sha256: rootHash }),
    ),
  });
  await page.waitForFunction(
    () =>
      document.querySelector("#verification").textContent ===
        "Saved controller history" &&
      document.querySelector("#scrub").max === "0",
  );
  const originalAttempts = await page.locator("#attempts").innerText();
  await page.locator("#take-control").click();
  await page.keyboard.down("ArrowRight");
  await page.waitForTimeout(500);
  await page.keyboard.up("ArrowRight");
  await page.keyboard.down("KeyZ");
  await page.waitForTimeout(80);
  await page.keyboard.up("KeyZ");
  await page.waitForFunction(
    () => Number(document.querySelector("#film").dataset.audioFrames) > 0,
  );
  await page.locator("#sound").click();
  assert.equal(
    await page.locator("#sound").getAttribute("aria-label"),
    "Unmute game audio",
  );
  await page.locator("#sound").click();
  await page.locator("#take-control").click();
  assert.equal(
    await page.locator("#take-control").getAttribute("aria-pressed"),
    "false",
  );
  const manualDownload = page.waitForEvent("download");
  await page.locator("#export").click();
  const manualTape = JSON.parse(
    await readFile(await (await manualDownload).path()),
  );
  assert.ok(manualTape.actions.some((a) => a.buttons & 128));
  assert.ok(manualTape.actions.some((a) => a.buttons & 1));
  assert.ok(manualTape.actions.reduce((n, a) => n + a.frames, 0) > 12);
  emulator.restore(emulatorRoot);
  for (const a of manualTape.actions) emulator.run(a.buttons, a.frames);
  assert.equal(
    await snapshotHash(emulator.capture()),
    manualTape.endpoint_sha256,
    "Human inputs must reproduce the exact rendered endpoint",
  );
  await page.locator("#search-here").click();
  await page.waitForFunction(
    () =>
      document.querySelector("#branch-choice").value === "1" &&
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
  const position = await page.locator("#details b").first().innerText();
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
        ?.textContent.includes("#1:") &&
      document.querySelector("#verification").textContent === "Exact replay ✓",
  );
  const childDownload = page.waitForEvent("download");
  await page.locator("#export").click();
  const childTape = JSON.parse(
    await readFile(await (await childDownload).path()),
  );
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
  await page.locator("#branch-choice").selectOption("0");
  await page.waitForFunction(
    () =>
      document.querySelector("#verification").textContent === "Original game",
  );
  assert.equal(
    await page.locator("#attempts").innerText(),
    originalAttempts,
    "Switching back must retain the original paused search",
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
  const artCredits = page.locator(".art-credit");
  assert.equal(await artCredits.count(), 3);
  for (const credit of await artCredits.all()) {
    assert.equal(await credit.isVisible(), true);
    assert.match(
      await credit.innerText(),
      /Nova the Squirrel art by NovaSquirrel/,
    );
    assert.match(await credit.innerText(), /CC BY-NC-SA 4.0/);
    assert.equal(
      await credit.locator('a[rel="license"]').getAttribute("href"),
      "https://creativecommons.org/licenses/by-nc-sa/4.0/",
    );
    assert.match(
      await credit.locator("a").first().getAttribute("href"),
      /NovaSquirrel\/NovaTheSquirrel/,
    );
  }
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
    await page.locator("#heat-toggle,#fit,#left,#right,#seed-label").count(),
    0,
  );
  assert.equal(await page.locator("#pause svg").count(), 1);
  assert.equal(
    await page.locator("#pause").getAttribute("aria-label"),
    "Resume Search",
  );
  assert.equal(await page.locator("#reset").innerText(), "Restart Search");
  assert.equal(await page.locator(".map-row").count(), 3);
  for (const id of [0, 49, 45])
    assert.equal(
      await page.locator(`.map-row[data-map="${id}"] canvas`).isVisible(),
      true,
    );
  await page.locator('.map-row[data-map="49"] canvas').focus();
  await page.keyboard.press("ArrowRight");
  await page.keyboard.press("Enter");
  assert.equal(await page.locator("#map").getAttribute("data-map"), "49");
  assert.equal(
    await page.locator("#map").evaluate((c) => c === document.activeElement),
    true,
    "Entering a cell on a stacked map must preserve keyboard focus",
  );
  await page.locator("#close-inspector").click();
  await page.locator('.map-row[data-map="45"] .area-label').click();
  assert.equal(await page.locator("#map").getAttribute("data-map"), "45");
  assert.match(await page.locator("#map-label").innerText(), /MAIN LEVEL/);
  await page.locator('.map-row[data-map="49"] .area-label').click();
  assert.match(await page.locator("#map-label").innerText(), /GARDEN/);
  await page
    .locator("#worlds")
    .getByRole("button", { name: "World 2", exact: true })
    .click();
  assert.equal(await page.locator(".level-card").count(), 8);
  await page.locator('.map-card[data-map="13"]').click();
  assert.equal(await page.locator("#map").getAttribute("data-map"), "13");
  assert.ok(
    Number(await page.locator("#map").getAttribute("data-map-height")) > 224,
    "Tall level must retain its vertical layout",
  );
  await page
    .locator("#worlds")
    .getByRole("button", { name: "World 1", exact: true })
    .click();
  await page.locator('.map-card[data-map="45"]').click();
  await page.locator("#zoom").click();
  await page
    .locator("#worlds")
    .getByRole("button", { name: "World 2", exact: true })
    .click();
  assert.equal(
    await page.locator("#map").evaluate((c) => c.height),
    320,
    "World navigation must preserve the zoomed map viewport",
  );
  await page
    .locator("#worlds")
    .getByRole("button", { name: "World 1", exact: true })
    .click();
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
    await page.locator(".exploration > .art-credit").isVisible(),
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
  await page.locator("#inspect-open").click();
  await page.waitForFunction(
    () => !document.querySelector("#take-control").disabled,
  );
  const collapsedHeight = (await page.locator("#inspector").boundingBox())
    .height;
  await page.locator("#expand-inspector").click();
  assert.ok(
    (await page.locator("#inspector").boundingBox()).height > collapsedHeight,
  );
  await page.locator("#take-control").click();
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
    "Browser search, exact replay, scrubbing, downloads, import, room controls, reset and mobile layout passed.",
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
  await browser.close();
}
