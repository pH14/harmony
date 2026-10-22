// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import test from "node:test";
import { nativeRuns } from "../src/native-run.js";

test("Native Runs distinguishes archive selection from the chosen whole-run limit", async (t) => {
  class Node {
    constructor(tag) { this.tag = tag; this.children = []; this.value = ""; }
    append(...children) {
      this.children.push(...children);
      if (this.tag === "select" && !this.value) this.value = children[0]?.value || "";
    }
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
  await nativeRuns("famidash", mount);
  const all = (node) => [node, ...node.children.flatMap((child) => typeof child === "object" ? all(child) : [])];
  const nodes = all(body), form = nodes.find((node) => node.tag === "form");
  const machine = nodes.find((node) => node.tag === "select");
  const archiveLabel = nodes.find((node) => node.tag === "label" && node.children[0].textContent === "Archive budget (GiB)");
  assert.ok(archiveLabel);
  const archive = archiveLabel.children[1], whole = nodes.find((node) => node.className === "native-memory-limit");
  assert.equal(archive.max, 4);
  assert.match(whole.textContent, /8\.0 GiB, swap disabled/);
  machine.value = "large";
  machine.onchange();
  assert.equal(archive.max, 64);
  assert.match(whole.textContent, /80\.0 GiB, swap disabled/);
  archive.value = 16;
  form.onsubmit({ preventDefault() {} });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(posted.memory_mib, 16384);
  assert.equal(posted.machine, "large");
  assert.equal(Object.hasOwn(posted, "memory_max_mib"), false);
});
