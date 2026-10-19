// SPDX-License-Identifier: AGPL-3.0-or-later
import { readFile } from "node:fs/promises";
import init from "../rust/pkg/twin_dragons_browser.js";

export async function initRustNode() {
  return init({ module_or_path: await readFile(new URL("../rust/pkg/twin_dragons_browser_bg.wasm", import.meta.url)) });
}
