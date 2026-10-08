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
      document.querySelector("#verification").textContent === "Original game",
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
  const [x, y] = (await page.locator("#details b").first().innerText())
    .split(",")
    .map(Number);
  const click = await page.locator("#map").evaluate(
    (canvas, position) => {
      const width = Number(canvas.dataset.mapWidth),
        height = Number(canvas.dataset.mapHeight),
        scale = Math.min(canvas.width / width, canvas.height / height),
        rect = canvas.getBoundingClientRect();
      const px = position.x % width,
        py = Math.floor(position.x / width) * 224 + position.y - 8;
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
  assert.equal(await page.locator("#selection-hint").isVisible(), false);
  assert.equal(await page.locator("#verification").isVisible(), false);
  await page.waitForFunction(
    () =>
      document.querySelector("#verification").textContent === "Exact replay ✓",
    { timeout: 60000 },
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
  const rootHash = await snapshotHash(emulator.capture());
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
    () => document.querySelector("#play").textContent === "Seeking…",
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
  assert.notEqual(
    await page.locator("#map").evaluate((c) => c.toDataURL()),
    before,
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
  await mkdir("test-results", { recursive: true });
  await page.screenshot({ path: "test-results/mobile.png", fullPage: true });
  await page.setViewportSize({ width: 1440, height: 1100 });
  await page.waitForTimeout(2500);
  await page.screenshot({ path: "test-results/desktop.png", fullPage: true });
  assert.equal(await page.locator("#error").isVisible(), false);
  assert.deepEqual(errors, []);
  console.log(
    "Browser search, exact replay, scrubbing, downloads, import, room controls, reset and mobile layout passed.",
  );
} finally {
  await browser.close();
}
