// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { createEngine } from "../src/emulator.js";
import twinDragons from "../src/games/twin-dragons.js";
import { campaignSearch, campaignWorker, coreIdentity, runCampaign } from "../src/campaign.js";
import { initRustNode } from "../tools/rust-node.mjs";

const demo = fileURLToPath(new URL("..", import.meta.url));
const root = join(demo, "../..");
const base = new URL("../public/", import.meta.url);
const rom = join(demo, "public/twin-dragons.nes");
const seed = Number(process.env.ORACLE_SEED || 1);
const executions = Number(process.env.ORACLE_EXECUTIONS || 600);

function nativeCore() {
  if (process.env.HARMONY_QUICKNES_CORE) return process.env.HARMONY_QUICKNES_CORE;
  const dir = join(demo, ".build/native-quicknes"),
    core = join(dir, process.platform === "darwin" ? "quicknes_libretro.dylib" : "quicknes_libretro.so");
  if (!existsSync(core)) {
    mkdirSync(dir, { recursive: true });
    execFileSync("sh", [join(root, "scripts/build-quicknes-core.sh"), core], { stdio: ["ignore", "ignore", "inherit"] });
  }
  return core;
}

const records = (text) => text.split("\n").filter(Boolean).slice(1);

const out = mkdtempSync(join(tmpdir(), "twin-dragons-campaign-"));
try {
  execFileSync("cargo", [
    "run", "--quiet", "--release", "--locked", "--manifest-path", join(root, "workloads/nes/Cargo.toml"),
    "--bin", "twin-dragons-campaign", "--", "--core", nativeCore(), "--rom", rom, "--output", out,
    "--executions", String(executions), "--workers", "16", "--seed", String(seed),
  ], { stdio: ["ignore", "ignore", "inherit"] });
  const native = records(readFileSync(join(out, "stream.jsonl"), "utf8"));

  await initRustNode();
  const engine = await createEngine(base, twinDragons, {
    rom: readFileSync(rom),
    wasmBinary: readFileSync(new URL("engine/quicknes.wasm", base)),
  });
  const identity = coreIdentity("browser");
  const search = campaignSearch(engine, identity, { seed, executions, record: true });
  runCampaign(search, campaignWorker(engine, identity));
  const browser = records(search.take_stream());

  assert.ok(native.length >= executions, "the native campaign recorded every execution");
  assert.equal(browser.length, native.length, "both campaigns recorded the same number of events");
  browser.forEach((line, i) => assert.deepEqual(JSON.parse(line), JSON.parse(native[i]), `event ${i + 1} differs`));
  console.log(`Browser and native Twin Dragons campaigns match for seed ${seed} over ${native.length} events`);
} finally {
  rmSync(out, { recursive: true, force: true });
}
