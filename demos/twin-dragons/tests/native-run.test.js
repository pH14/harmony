// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import test from "node:test";
import { detectLauncher, nativePanel } from "../src/native-run.js";

test("Native Runs distinguishes archive selection from the chosen whole-run limit", async (t) => {
  class Node {
    constructor(tag) { this.tag = tag; this.children = []; this.value = ""; }
    append(...children) { this.children.push(...children); }
    replaceChildren(...children) { this.children = children; }
    setAttribute() {}
    addEventListener() {}
  }
  const original = Object.fromEntries(["document", "location", "fetch"].map((key) => [key, Object.getOwnPropertyDescriptor(globalThis, key)]));
  t.after(() => {
    for (const [key, descriptor] of Object.entries(original)) {
      if (descriptor) Object.defineProperty(globalThis, key, descriptor);
      else delete globalThis[key];
    }
  });
  const body = new Node("body"), mount = new Node("main");
  globalThis.document = { body, createElement: (tag) => new Node(tag) };
  globalThis.location = { hostname: "localhost", href: "http://localhost/" };
  const machines = [
    { name: "small", workers: 8, memory_mib: 4096, memory_max_mib: 8192 },
    { name: "large", workers: 128, memory_mib: 65536, memory_max_mib: 81920 },
  ];
  let posted;
  globalThis.fetch = async (url, options) => {
    if (options?.method === "HEAD") return { headers: { has: () => true } };
    if (url === "/launcher/machines") return { ok: true, json: async () => ({ machines, games: ["famidash"] }) };
    if (options?.method === "POST") posted = JSON.parse(options.body);
    return { ok: true, json: async () => ({ runs: [] }) };
  };
  const catalog = [{ id: "famidash", title: "Famidash", rom: {} }];
  const options = await detectLauncher();
  assert.deepEqual(options.machines, machines);
  const panel = nativePanel(options, catalog, "famidash");
  const all = (node) => [node, ...node.children.flatMap((child) => typeof child === "object" ? all(child) : [])];
  const nodes = all(panel.element), form = nodes.find((node) => node.tag === "form");
  const archiveLabel = nodes.find((node) => node.tag === "label" && node.children[0].textContent === "Archive (GiB)");
  assert.ok(archiveLabel);
  const archive = archiveLabel.children[1], note = nodes.find((node) => node.className === "native-note");
  assert.equal(archive.max, 4);
  assert.match(note.textContent, /share 8 GiB/);
  assert.deepEqual(panel.machines, ["small", "large"]);
  panel.setMachine("large");
  assert.equal(archive.max, 64);
  assert.match(note.textContent, /share 80 GiB/);
  archive.value = 16;
  form.onsubmit({ preventDefault() {} });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(posted.memory_mib, 16384);
  assert.equal(posted.machine, "large");
  assert.equal(posted.game, "famidash");
  assert.equal(Object.hasOwn(posted, "memory_max_mib"), false);
});
