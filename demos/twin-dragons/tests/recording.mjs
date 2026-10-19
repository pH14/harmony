// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { mkdir, stat, writeFile } from "node:fs/promises";
import { chromium } from "@playwright/test";
import { TOUR_KEY } from "../src/tour.js";

const url = process.env.DEMO_URL || "http://127.0.0.1:4173";
const browser = await chromium.launch({ headless: true, ...(process.env.CHROME_CHANNEL ? { channel: process.env.CHROME_CHANNEL } : {}) });
const errors = [];
const output = process.env.LIVE_RECORDING ? "test-results/recording-native" : "test-results/recording";
await mkdir(output, { recursive: true });
try {
  const page = await browser.newPage({ viewport: { width: 1440, height: 1000 }, acceptDownloads: true });
  page.on("pageerror", error => errors.push(error.message));
  await page.addInitScript(key => {
    localStorage.setItem(key, "seen");
    // Keep local browser search small while exercising real game and camera rendering.
    Object.defineProperty(navigator, "hardwareConcurrency", { get: () => 3 });
    const capture = HTMLCanvasElement.prototype.captureStream;
    HTMLCanvasElement.prototype.captureStream = function (...args) {
      window.recordedCanvas = this;
      const stream = capture.apply(this, args);
      window.recordedTracks = stream.getTracks();
      return stream;
    };
  }, TOUR_KEY);
  await page.goto(`${url}/?game=famidash&level=0&workers=1&pace=0${process.env.LIVE_RECORDING ? "&live" : ""}`);
  await page.waitForFunction(() => Number(document.querySelector("#hero-timelines")?.textContent.replace(/\D/g, "")) > 30, null, { timeout: 60000 });
  assert.equal(await page.locator("#record-view").isEnabled(), true);
  const before = await page.locator(".area-map").first().getAttribute("data-center-x");
  await page.locator("#record-view").click();
  await page.waitForFunction(before => Math.abs(Number(document.querySelector(".area-map").dataset.centerX) - Number(before)) > 50, before, { timeout: 15000 });
  await page.waitForTimeout(2500);
  assert.equal(await page.locator("#record-view").getAttribute("aria-pressed"), "true");
  // Verify the captured source contains the actual camera and mini map, not an independent renderer.
  const composition = await page.evaluate(() => {
    const target = window.recordedCanvas, ctx = target.getContext("2d");
    const diverse = (x, y, w, h) => {
      const pixels = ctx.getImageData(x, y, w, h).data, colors = new Set();
      for (let i = 0; i < pixels.length; i += 4) colors.add(`${pixels[i]},${pixels[i + 1]},${pixels[i + 2]}`);
      return colors.size;
    };
    return { width: target.width, height: target.height, mainColors: diverse(24, 88, 1232, 438), overviewColors: diverse(24, 548, 1232, 92), camera: document.querySelector(".area-map").dataset.centerX };
  });
  assert.equal(composition.width, 1280);
  assert.equal(composition.height, 720);
  assert.ok(composition.mainColors > 20, JSON.stringify(composition));
  assert.ok(composition.overviewColors > 10, JSON.stringify(composition));
  await writeFile(`${output}/recorded-frame.png`, Buffer.from(await page.evaluate(() => recordedCanvas.toDataURL().split(",")[1]), "base64"));
  await page.locator("#record-view").click();
  await page.locator("#download-recording").waitFor({ state: "visible" });
  assert.equal(await page.evaluate(() => recordedTracks.every(track => track.readyState === "ended")), true);
  const downloading = page.waitForEvent("download");
  await page.locator("#download-recording").click();
  const download = await downloading;
  const artifact = `${output}/${download.suggestedFilename()}`;
  await download.saveAs(artifact);
  assert.ok((await stat(artifact)).size > 1000);
  // Decode the downloaded bytes and verify actual playable frames and changing search visuals.
  const playback = await page.evaluate(async () => {
    const video = document.createElement("video");
    video.muted = true;
    video.src = document.querySelector("#download-recording").href;
    await new Promise((resolve, reject) => { video.onloadeddata = resolve; video.onerror = () => reject(new Error("Downloaded video cannot decode")); });
    const sample = () => {
      const c = document.createElement("canvas"); c.width = 320; c.height = 180;
      c.getContext("2d").drawImage(video, 0, 0, 320, 180);
      return c.toDataURL();
    };
    const first = sample();
    video.currentTime = 1;
    await new Promise(resolve => { video.onseeked = resolve; });
    const changed = first !== sample();
    return { width: video.videoWidth, height: video.videoHeight, changed, time: video.currentTime };
  });
  assert.deepEqual(playback, { width: 1280, height: 720, changed: true, time: 1 });
  const firstURL = await page.locator("#download-recording").getAttribute("href");
  await page.locator("#record-view").click();
  await page.waitForTimeout(1200);
  await page.locator("#record-view").click();
  await page.waitForFunction(previous => document.querySelector("#download-recording").href !== previous, firstURL);
  assert.equal(await page.evaluate(async previous => { try { await fetch(previous); return false; } catch { return true; } }, firstURL), true);
  await page.evaluate(() => { recordedCanvas.captureStream = () => { throw new Error("Capture failed for test"); }; });
  await page.locator("#record-view").click();
  await page.waitForFunction(() => document.querySelector("#recording-status").textContent.includes("Capture failed for test"));
  assert.equal(await page.locator("#record-view").getAttribute("aria-pressed"), "false");
  assert.equal(await page.locator("#download-recording").isVisible(), true, "Failed recording preserves the last downloadable video");
  await page.evaluate(() => { delete recordedCanvas.captureStream; });
  await page.setViewportSize({ width: 390, height: 844 });
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, "Recording controls fit on mobile");
  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.screenshot({ path: `${output}/famidash-recording.png`, fullPage: true });
  assert.deepEqual(errors, []);
  console.log(JSON.stringify({ artifact, composition, playback, before }));
  await page.close();
} finally { await browser.close(); }
