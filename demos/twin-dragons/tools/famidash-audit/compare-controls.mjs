// SPDX-License-Identifier: AGPL-3.0-or-later
import { readFile, writeFile, readdir } from "node:fs/promises";
import { join } from "node:path";
import { gunzipSync } from "node:zlib";
import { machine, routeTape, expand, hash } from "./runtime.mjs";

const [indexPath, originalDirectory, patchedDirectory, runtime, inputs, output] = process.argv.slice(2);
if (!output) throw new Error("compare-controls.mjs INDEX ORIGINAL_ROMS PATCHED_ROMS RUNTIME ROUND OUTPUT");
const index = JSON.parse(await readFile(indexPath)), machines = new Map();
const result = { schema: 1, purpose: "paired ordinary-input reference and control replays; completion recorded per input", levels: [] };
const coinInputs = [];
for (const name of await readdir(new URL("findings/dissonance/coin-routes/", import.meta.url))) {
  if (!name.endsWith(".json.gz")) continue;
  const path = new URL(`findings/dissonance/coin-routes/${name}`, import.meta.url);
  const witness = JSON.parse(gunzipSync(await readFile(path)));
  coinInputs.push({ side: witness.side ?? witness.options?.side, level: witness.level ?? witness.boot_level,
    path: path.pathname, tape: expand(witness.actions) });
}
async function play(m, side, level, tape) {
  m.boot(side, level);
  const trace = [], coins = [];
  let previous = [0, 0, 0];
  for (const button of tape) {
    m.core.step(button, 1);
    const o = m.observe(), timers = [...m.ram().slice(0x4b3, 0x4b6)];
    trace.push(o);
    for (let coin = 0; coin < 3; coin++)
      if (!previous[coin] && timers[coin]) coins.push({ coin: coin + 1, frame: trace.length });
    previous = timers;
    if (o.debug || o.paused) throw new Error("debug/paused control");
    if (o.dead || o.state !== 2) break;
  }
  return { trace, coins };
}
for (const layout of index.levels) {
  if (!machines.has(layout.rom)) machines.set(layout.rom, await Promise.all([
    machine(join(originalDirectory, layout.rom), runtime), machine(join(patchedDirectory, layout.rom), runtime)]));
  const [original, patched] = machines.get(layout.rom);
  const sources = [];
  try {
    const path = join(inputs, `${layout.side}-${layout.level}`, "clearing-input.json");
    sources.push({ path, tape: expand(JSON.parse(await readFile(path)).actions) });
  } catch {}
  sources.push(...coinInputs.filter(input => input.side === layout.side && input.level === layout.level));
  try {
    const path = new URL(`../famidash-routes/${layout.side}.json`, import.meta.url);
    const routes = JSON.parse(await readFile(path));
    sources.push({ path: path.pathname, tape: routeTape(routes.routes[layout.level]) });
  } catch {}
  const entry = { side: layout.side, level: layout.level, name: layout.name,
    original_rom_sha256: original.rom_sha256, patched_rom_sha256: patched.rom_sha256, controls: [] };
  for (const source of sources) {
    const originalRun = await play(original, layout.side, layout.level, source.tape);
    const patchedRun = await play(patched, layout.side, layout.level, source.tape);
    const a = originalRun.trace, b = patchedRun.trace;
    const first = a.findIndex((o, i) => JSON.stringify(o) !== JSON.stringify(b[i]));
    entry.controls.push({ source: source.path, input_sha256: hash(JSON.stringify(source.tape)),
      original_frames: a.length, patched_frames: b.length, original_endpoint: a.at(-1), patched_endpoint: b.at(-1),
      original_coins: originalRun.coins, patched_coins: patchedRun.coins,
      identical: first === -1 && a.length === b.length,
      first_difference: first === -1 ? null : { frame: first + 1, original: a[first], patched: b[first] },
      original_trace_sha256: hash(JSON.stringify(a)), patched_trace_sha256: hash(JSON.stringify(b)) });
  }
  result.levels.push(entry);
  await writeFile(output, JSON.stringify(result, null, 2));
  console.log(layout.side, layout.level, entry.controls.map(c => `${c.original_endpoint.state === 3}:${c.identical}:${c.first_difference?.frame ?? "-"}`).join(" "));
}
