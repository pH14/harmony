// SPDX-License-Identifier: AGPL-3.0-or-later
import http from "node:http";
import { readFile } from "node:fs/promises";
import path from "node:path";
const root = path.resolve(
  import.meta.dirname,
  "..",
  process.env.DIST === "1" ? "dist" : ".",
);
const port = Number(process.env.PORT || 4174);
http
  .createServer(async (req, res) => {
    try {
      const pathname = decodeURIComponent(
        new URL(req.url, "http://localhost").pathname,
      );
      const file = path.resolve(
        root,
        "." + pathname + (pathname.endsWith("/") ? "index.html" : ""),
      );
      if (!file.startsWith(root + path.sep)) {
        res.writeHead(403).end();
        return;
      }
      const body = await readFile(file);
      const headers = {
        "Content-Type":
          {
            ".html": "text/html",
            ".js": "text/javascript",
            ".mjs": "text/javascript",
            ".wasm": "application/wasm",
            ".css": "text/css",
            ".json": "application/json",
            ".md": "text/plain",
          }[path.extname(file)] || "application/octet-stream",
      };
      if (process.env.ISOLATE !== "0")
        Object.assign(headers, {
          "Cross-Origin-Opener-Policy": "same-origin",
          "Cross-Origin-Embedder-Policy": "require-corp",
        });
      res.writeHead(200, headers).end(body);
    } catch {
      res.writeHead(404).end();
    }
  })
  .listen(port, "127.0.0.1", () => console.log(`http://127.0.0.1:${port}`));
