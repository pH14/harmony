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
  const rect = await page.locator("#map").boundingBox(),
    mapWidth = Number(await page.locator("#map").getAttribute("width"));
  await page.locator("#map").click({
    position: {
      x: (rect.width * x) / mapWidth,
      y: (rect.height * (y - 8)) / 224,
    },
  });
  await page.waitForFunction(
    (expected) =>
      document.querySelector("#cell-title").textContent === expected,
    `Cell ${Math.floor(x / 32)}, ${Math.floor(y / 32)}`,
  );
  assert.match(
    await page.locator("#selection-hint").innerText(),
    /Choose a retained state/,
  );
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
  for (const file of [
    "licenses/CREDITS.md",
    "licenses/harmony-source.tar.gz",
    "licenses/nova-source.tar.gz",
    "licenses/quicknes-source.tar.gz",
    "level-one-main.png",
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
  await page.locator("#room").selectOption("40");
  assert.match(await page.locator("#map-label").innerText(), /MAIN ROOM/);
  await page.locator("#zoom").click();
  await page.locator("#right").click();
  await page.locator("#fit").click();
  await page.locator("#reset").click();
  await page.waitForFunction(
    () =>
      document.querySelector("#seed-label").textContent === "seed 2" &&
      document.querySelector("#status").textContent === "Exploring",
  );
  await page.setViewportSize({ width: 390, height: 844 });
  assert.equal(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
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
