// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { mkdir, stat, writeFile } from "node:fs/promises";
import { chromium } from "@playwright/test";
import { TOUR_KEY } from "../src/tour.js";

async function inspectVideo(page, source = "download", sampleTime = 1) {
  return page.evaluate(async ({ source, sampleTime }) => {
    const video = document.createElement("video");
    video.muted = true;
    const fileURL = source === "file" ? URL.createObjectURL(await window.recordingSavedFile.getFile()) : null;
    video.src = fileURL || document.querySelector("#download-recording").href;
    await new Promise((resolve, reject) => { video.onloadeddata = resolve; video.onerror = () => reject(new Error("Recorded video cannot decode")); });
    const sample = (y, height) => {
      const c = document.createElement("canvas"); c.width = 308; c.height = Math.ceil(height / 4);
      c.getContext("2d").drawImage(video, 24, y, 1232, height, 0, 0, c.width, c.height);
      return c.toDataURL();
    };
    const first = sample(88, 438), firstOverview = sample(548, 92);
    video.currentTime = sampleTime;
    await new Promise(resolve => { video.onseeked = resolve; });
    const changed = first !== sample(88, 438), overviewChanged = firstOverview !== sample(548, 92);
    const result = { width: video.videoWidth, height: video.videoHeight, changed, overviewChanged, time: video.currentTime };
    video.src = "";
    if (fileURL) URL.revokeObjectURL(fileURL);
    return result;
  }, { source, sampleTime });
}

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
    window.showSaveFilePicker = async () => {
      const directory = await navigator.storage.getDirectory();
      window.recordingSavedFile = await directory.getFileHandle("search-recording.webm", { create: true });
      return window.recordingSavedFile;
    };
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
  const captureStarted = Date.now();
  await page.locator("#record-view").click();
  await page.waitForFunction(before => Math.abs(Number(document.querySelector(".area-map").dataset.centerX) - Number(before)) > 50, before, { timeout: process.env.LIVE_RECORDING ? 45000 : 15000 });
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
  const sampleTime = process.env.LIVE_RECORDING ? Math.max(1, (Date.now() - captureStarted) / 1000 - 1) : 1;
  const playback = await inspectVideo(page, "download", sampleTime);
  assert.deepEqual({ width: playback.width, height: playback.height, changed: playback.changed, overviewChanged: playback.overviewChanged }, { width: 1280, height: 720, changed: true, overviewChanged: true });
  assert.ok(Math.abs(playback.time - sampleTime) < 0.01, "The decoded clip reaches its sampled game frame");
  const firstURL = await page.locator("#download-recording").getAttribute("href");
  await page.locator("#record-view").click();
  await page.waitForTimeout(1200);
  await page.locator("#record-view").click();
  await page.waitForFunction(previous => document.querySelector("#download-recording").href !== previous, firstURL);
  assert.equal(await page.evaluate(async previous => { try { await fetch(previous); return false; } catch { return true; } }, firstURL), true);
  await page.locator("#record-to-file").click();
  await page.waitForFunction(() => document.querySelector("#recording-status").textContent.includes("Recording directly to file"));
  await page.waitForTimeout(2500);
  await page.locator("#record-view").click();
  await page.waitForFunction(() => document.querySelector("#recording-status").textContent.includes("Video saved to file"));
  const fileBytes = await page.evaluate(async () => {
    const file = await recordingSavedFile.getFile();
    return new Promise(resolve => { const reader = new FileReader(); reader.onload = () => resolve(reader.result.split(",")[1]); reader.readAsDataURL(file); });
  });
  const fileArtifact = `${output}/streamed-recording.webm`;
  await writeFile(fileArtifact, Buffer.from(fileBytes, "base64"));
  assert.ok((await stat(fileArtifact)).size > 1000);
  const filePlayback = await inspectVideo(page, "file");
  assert.deepEqual(filePlayback, { width: 1280, height: 720, changed: true, overviewChanged: true, time: 1 });
  await page.evaluate(() => { window.showSaveFilePicker = async () => { throw new DOMException("Canceled", "AbortError"); }; });
  await page.locator("#record-to-file").click();
  await page.waitForFunction(() => document.querySelector("#recording-status").textContent === "File recording canceled.");
  assert.equal(await page.locator("#record-view").isEnabled(), true);
  assert.equal(await page.locator("#download-recording").isVisible(), true);
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
  await page.evaluate(() => { dispatchEvent(new Event("pagehide")); });
  assert.equal(await page.locator("#download-recording").isVisible(), false, "Returning from page lifecycle cannot expose a revoked download");
  await page.evaluate(() => { MediaRecorder.isTypeSupported = () => false; dispatchEvent(new Event("pagehide")); });
  assert.equal(await page.locator("#record-view").isEnabled(), false, "Cleanup keeps unsupported capture disabled");
  assert.deepEqual(errors, []);
  console.log(JSON.stringify({ artifact, fileArtifact, composition, playback, filePlayback, before }));
  await page.close();
} finally { await browser.close(); }
