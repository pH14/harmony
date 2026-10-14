// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { createEngine, GAMES } from "../src/emulator.js";
import { campaignSearch, campaignWorker, coreIdentity, runCampaign } from "../src/campaign.js";
import { initRustNode } from "../tools/rust-node.mjs";

const demo = fileURLToPath(new URL("..", import.meta.url));
const root = join(demo, "../..");
const base = new URL("../public/", import.meta.url);
const roms = `${homedir()}/workspace/roms`;
const seed = Number(process.env.ORACLE_SEED || 1);
const memoryMiB = 1024;
const CASES = [
  { id: "twin-dragons", rom: join(demo, "public/twin-dragons.nes"), executions: 600 },
  { id: "super-mario-bros", rom: process.env.SMB_ROM || `${roms}/Super Mario Bros. (World).nes`, eval: { game: "smb" }, executions: 400 },
];
const selected = process.env.ORACLE_GAMES?.split(",");

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
const sha256 = (path) => createHash("sha256").update(readFileSync(path)).digest("hex");
const cargo = (bin, args) =>
  execFileSync("cargo", [
    "run", "--quiet", "--release", "--locked", "--manifest-path", join(root, "workloads/nes/Cargo.toml"), "--bin", bin, "--", ...args,
  ], { stdio: ["ignore", "ignore", "inherit"] });

function nativeStream(game, executions, out) {
  const core = nativeCore();
  if (!game.eval) {
    cargo("twin-dragons-campaign", [
      "--core", core, "--rom", game.rom, "--output", out, "--executions", String(executions), "--workers", "16", "--seed", String(seed),
    ]);
    return readFileSync(join(out, "stream.jsonl"), "utf8");
  }
  const request = join(out, "request.json"), result = join(out, "result");
  writeFileSync(request, JSON.stringify({
    ...game.eval, rom: game.rom, core, rom_sha256: sha256(game.rom), core_sha256: sha256(core), seed, workers: 16, executions,
    memory_mib: memoryMiB, window: 1, wall_seconds: 3600, mixture: "energy_splice:6", verification: "campaign",
  }));
  cargo("nes-eval", [request, result]);
  return readFileSync(join(result, "stream.jsonl"), "utf8");
}

await initRustNode();
for (const game of CASES) {
  if (selected && !selected.includes(game.id)) continue;
  if (!existsSync(game.rom)) {
    console.log(`Skipping ${game.id}: no ROM at ${game.rom}.`);
    continue;
  }
  const executions = Number(process.env.ORACLE_EXECUTIONS || game.executions);
  const out = mkdtempSync(join(tmpdir(), `${game.id}-campaign-`));
  try {
    const native = records(nativeStream(game, executions, out));
    const engine = await createEngine(base, GAMES.get(game.id), {
      rom: readFileSync(game.rom),
      wasmBinary: readFileSync(new URL("engine/quicknes.wasm", base)),
    });
    const identity = coreIdentity("browser");
    const search = campaignSearch(engine, identity, { seed, executions, memoryMiB: game.eval ? memoryMiB : 0, record: true });
    runCampaign(search, campaignWorker(engine, identity));
    const browser = records(search.take_stream());
    assert.ok(native.length >= executions, `the native ${game.id} campaign recorded every execution`);
    assert.equal(browser.length, native.length, `both ${game.id} campaigns recorded the same number of events`);
    browser.forEach((line, i) => assert.deepEqual(JSON.parse(line), JSON.parse(native[i]), `${game.id} event ${i + 1} differs`));
    console.log(`Browser and native ${game.id} campaigns match for seed ${seed} over ${native.length} events`);
  } finally {
    rmSync(out, { recursive: true, force: true });
  }
}
