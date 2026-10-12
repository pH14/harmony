// SPDX-License-Identifier: AGPL-3.0-or-later
import { readFile, writeFile, mkdir, cp, rm } from "node:fs/promises";
import "./fetch-runtime.mjs";
import "./fetch-evidence.mjs";
const root = new URL("../", import.meta.url),
  dist = new URL("dist/", root);
await rm(dist, { recursive: true, force: true });
await mkdir(dist, { recursive: true });
for (const path of [
  "index.html",
  "isolate-sw.js",
  "src",
  "public/evidence",
  "workload/raft.c",
  "runtime-lock.json",
  "NOTICES.md",
]) {
  const target = new URL(path, dist);
  await mkdir(new URL("./", target), { recursive: true });
  await cp(new URL(path, root), target, { recursive: true });
}
await mkdir(new URL("public/runtime/", dist), { recursive: true });
const lock = JSON.parse(await readFile(new URL("runtime-lock.json", root)));
for (const asset of lock.assets)
  await cp(
    new URL(`public/runtime/${asset.name}`, root),
    new URL(`public/runtime/${asset.name}`, dist),
  );
await mkdir(new URL("vendor/", dist), { recursive: true });
for (const [from, to] of [
  ["@xterm/xterm/lib/xterm.js", "xterm.js"],
  ["@xterm/xterm/css/xterm.css", "xterm.css"],
  ["xterm-pty/index.js", "xterm-pty.js"],
  ["@xterm/xterm/LICENSE", "xterm-LICENSE"],
  ["xterm-pty/LICENSE.txt", "xterm-pty-LICENSE"],
])
  await cp(
    new URL(`node_modules/${from}`, root),
    new URL(`vendor/${to}`, dist),
  );
await cp(new URL("../../LICENSE", root), new URL("LICENSE", dist));
await writeFile(new URL(".nojekyll", dist), "");
console.log("Built demos/raft/dist");
