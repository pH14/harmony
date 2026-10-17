// SPDX-License-Identifier: AGPL-3.0-or-later
import initRust from "../rust/pkg/twin_dragons_browser.js";

export async function loadRust() {
  if (!globalThis.process?.versions?.node) return initRust();
  const helper = "../tools/rust-node.mjs";
  return (await import(/* @vite-ignore */ helper)).initRustNode();
}
