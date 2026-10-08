// SPDX-License-Identifier: AGPL-3.0-or-later
import { chromium } from "@playwright/test";
import assert from "node:assert/strict";
import { readFile, mkdir } from "node:fs/promises";
const browser = await chromium.launch({
  headless: true,
  ...(process.env.CHROME_CHANNEL
    ? { channel: process.env.CHROME_CHANNEL }
    : {}),
});
const page = await browser.newPage({ viewport: { width: 1440, height: 1100 } }),
  errors = [];
page.on("pageerror", (error) => errors.push(error.message));
try {
  await page.goto(process.env.DEMO_URL || "http://127.0.0.1:4173");
  await page.waitForFunction(
    () =>
      Number(
        document.querySelector("#attempts").textContent.replaceAll(",", ""),
      ) > 30,
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
  const rect = await page.locator("#map").boundingBox();
  await page
    .locator("#map")
    .click({
      position: {
        x: (rect.width * x) / 1280,
        y: (rect.height * (y - 8)) / 224,
      },
    });
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
  await page.locator("#history-file").setInputFiles(path);
  await page.waitForFunction(
    () =>
      document.querySelector("#verification").textContent ===
      "Saved controller history",
  );
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
