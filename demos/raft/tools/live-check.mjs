import { chromium } from "@playwright/test";
import { writeFile } from "node:fs/promises";
const browser = await chromium.launch({
    headless: true,
    ...(process.env.CHROME ? { channel: "chrome" } : {}),
  }),
  page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
page.on("pageerror", (e) => console.log("ERROR", e.stack));
page.on("console", (m) => console.log("BROWSER", m.type(), m.text()));
await page.goto(process.env.DEMO_URL || "http://127.0.0.1:4174/");
await page.locator(".code-line").first().waitFor();
await page.locator("#boot").click();
const progress = setInterval(async () => {
  try {
    const status = await page.locator("#runtime-status").textContent(),
      term = await page.locator(".xterm-rows").textContent();
    console.log("LIVE", status);
    await writeFile("/tmp/raft-live-ui.log", status + "\n" + term);
  } catch {}
}, 15000);
try {
  await page.waitForFunction(
    () => document.querySelector("#mode").textContent.startsWith("Live Linux"),
    {},
    { timeout: 600000 },
  );
  console.log("LIVE_BASELINE_READY");
  await page.locator("#trace").click();
  await page.waitForFunction(
    () =>
      document
        .querySelector("#location")
        .textContent.startsWith("investigation-"),
    {},
    { timeout: 600000 },
  );
  console.log("LIVE_TRACE_READY");
  await page.locator('#phases button[data-step="1"]').click();
  await page.locator("#shell").click();
  await page.waitForFunction(
    () =>
      Array.from(document.querySelectorAll(".xterm-rows>div")).some((e) =>
        /^#\s*$/.test(e.textContent),
      ),
    {},
    { timeout: 600000 },
  );
  console.log("LIVE_SHELL_READY");
  await page.locator(".xterm-helper-textarea").focus();
  await page.keyboard.insertText(
    "touch /tmp/browser-proof; echo BROWSER_SHELL_OK",
  );
  await page.keyboard.press("Enter");
  await page.waitForFunction(
    () =>
      Array.from(document.querySelectorAll(".xterm-rows>div")).some(
        (e) => e.textContent.trim() === "BROWSER_SHELL_OK",
      ),
    {},
    { timeout: 300000 },
  );
  await page.keyboard.insertText("exit");
  await page.keyboard.press("Enter");
  await page.waitForFunction(
    () =>
      document.querySelector("#location").textContent.startsWith("inspected-"),
    {},
    { timeout: 600000 },
  );
  console.log("LIVE_SHELL_SAVED");
  await page.screenshot({ path: "/tmp/raft-live-ui.png", fullPage: true });
} finally {
  clearInterval(progress);
  await browser.close();
}
