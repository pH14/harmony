<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Observatory viewer

This React/Vite client reads the Rust `nes-observatory` API. Run `npm ci && npm run build`, then serve `dist/` with the Rust service. It has no direct ClickHouse credential or browser-side database connection. The map is schematic: observed Metroid map cells are positioned by decoded area and coarse map coordinates, with a stable logarithmic color scale per metric. Timeline scrubbing uses bounded API windows and an aborted, debounced request when the pointer moves again. Filters ask for sampled joint observations, with paginated example identities. A time or filter change hides the previous answer until the user runs the query again; overlapping requests are aborted.
