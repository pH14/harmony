// SPDX-License-Identifier: AGPL-3.0-or-later
import { chromium } from "@playwright/test";
import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { TOUR_KEY } from "../src/tour.js";
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
async function next(page, step) {
  await page.locator("#tour-next").click();
  await ready(page, step);
  await layout(page);
}
try {
  await mkdir("test-results", { recursive: true });
  const page = await open({ viewport: { width: 1440, height: 1100 } });
  await page.goto(url);
  await ready(page, 0);
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
  await next(page, 1);
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
  await page.screenshot({ path: "test-results/tour-desktop-routes.png" });
  for (let step = 2; step < 6; step++) await next(page, step);
  assert.equal(
    await page.locator("#branch-choice option").count(),
    1,
    "The tour must not create a search branch",
  );
  await page.locator("#tour-back").click();
  await ready(page, 4);
  await layout(page);
  await next(page, 5);
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
  await page.locator("#tour-open").click();
  await ready(page, 0);
  await next(page, 1);
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
  for (let step = 1; step < 6; step++) await next(page, step);
  await page.locator("#tour-try").click();
  assert.equal(await page.locator("#guided-tour").isVisible(), false);
  assert.equal(
    await page.locator("#take-control").getAttribute("aria-pressed"),
    "true",
  );
  assert.equal(await page.locator("#tour-open").isDisabled(), true);
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
  await next(page, 1);
  await page.locator("#tour-skip").click();
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
    "Skipping a replayed tour must restore the existing human history and its frame",
  );
  assert.equal(
    await page.locator("#pause").getAttribute("aria-label"),
    "Resume Search",
    "An already-paused search must remain paused after the tour",
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
  await layout(phone);
  for (let step = 1; step < 6; step++) {
    await next(phone, step);
    if (step === 1 || step === 3 || step === 5)
      await phone.screenshot({ path: `test-results/tour-phone-${step}.png` });
  }
  await phone.locator("#tour-try").click();
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
  await blocked.keyboard.press("Escape");
  await blocked.close();
  assert.deepEqual(errors, []);
  console.log(
    "Guided tour: authentic routes, spotlight geometry, pause restoration, keyboard exit, persistence, explicit takeover, mobile and blocked storage passed.",
  );
} finally {
  await browser.close();
}
