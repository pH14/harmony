// SPDX-License-Identifier: AGPL-3.0-or-later
import { readFile, writeFile, mkdir } from "node:fs/promises";
import { createHash } from "node:crypto";
const root = new URL("../", import.meta.url);
const lock = JSON.parse(await readFile(new URL("runtime-lock.json", root)));
await mkdir(new URL("public/evidence/", root), { recursive: true });
for (const asset of lock.evidence) {
  const target = new URL(`public/evidence/${asset.name}`, root);
  const valid = (bytes) =>
    bytes.length === asset.bytes &&
    createHash("sha256").update(bytes).digest("hex") === asset.sha256;
  let bytes = await readFile(target).catch(() => null);
  if (bytes && valid(bytes)) continue;
  const response = await fetch(asset.url);
  if (!response.ok) throw new Error(`${asset.url}: ${response.status}`);
  bytes = Buffer.from(await response.arrayBuffer());
  if (!valid(bytes))
    throw new Error(`Evidence checksum mismatch: ${asset.name}`);
  await writeFile(target, bytes);
}
