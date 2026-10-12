// SPDX-License-Identifier: AGPL-3.0-or-later
import { chromium, expect } from "@playwright/test";
import { mkdir } from "node:fs/promises";
const browser = await chromium.launch({
  headless: true,
  ...(process.env.CHROME ? { channel: "chrome" } : {}),
});
const errors = [];
try {
  const page = await browser.newPage({
    viewport: { width: 1440, height: 1100 },
    reducedMotion: "reduce",
  });
  page.on("pageerror", (e) => errors.push(e.message));
  await page.goto(process.env.DEMO_URL || "http://127.0.0.1:4174/");
  await page.locator(".code-line").first().waitFor();
  await page.locator('#phases button[data-step="4"]').click();
  await expect(page.locator("#verdict")).toContainText("Violated");
  await expect(page.locator("#logs")).toContainText("acknowledged");
  await page.locator('#phases button[data-step="1"]').click();
  await expect(page.locator("#logs")).not.toContainText(
    "replica acknowledgments",
  );
  await page.locator("#trace").click();
  await page.locator('#phases button[data-step="4"]').click();
  await expect(page.locator("#logs")).toContainText(
    "replica acknowledgments=1; required=2",
  );
  await expect(page.locator("#command")).toContainText(
    "--exec '/app/control trace'",
  );
  await mkdir("test-results", { recursive: true });
  await page.screenshot({ path: "test-results/desktop.png", fullPage: true });
  await page.locator("#quorum").click();
  await page.locator('#phases button[data-step="4"]').click();
  await expect(page.locator("#verdict")).toContainText("No violation");
  await expect(page.locator("#client")).toContainText("no acknowledgment");
  await page.locator("#original").click();
  await page.locator("#shell").click();
  await expect(page.locator("#explanation")).toContainText(
    "requires the live Linux runtime",
  );
  await page.setViewportSize({ width: 390, height: 844 });
  await page.screenshot({ path: "test-results/mobile.png", fullPage: true });
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
  const nodes = await page.locator(".node").evaluateAll((elements) =>
    elements.map((element) => {
      const r = element.getBoundingClientRect();
      return { left: r.left, right: r.right, top: r.top, bottom: r.bottom };
    }),
  );
  for (let i = 0; i < nodes.length; i++)
    for (let j = i + 1; j < nodes.length; j++) {
      const a = nodes[i],
        b = nodes[j];
      expect(
        a.right <= b.left ||
          b.right <= a.left ||
          a.bottom <= b.top ||
          b.bottom <= a.top,
      ).toBe(true);
    }
  expect(errors).toEqual([]);
  console.log(
    "PASS: real evidence, rewind tracing, counterfactual, shell guidance, mobile layout",
  );
} finally {
  await browser.close();
}
