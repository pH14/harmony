<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Metroid search observatory

`nes-observatory` is the native Metroid campaign plus a bounded, best-effort telemetry sidecar. The actual campaign stream and report remain the deterministic source of search results. The sidecar records actual parent selections (including duplicate skips), admitted measured emulated frames, valid gameplay action observations, and first observed coarse map cells. It uses the existing `AlphabetOnly` input policy and does not modify selection or RNG state. The Rust CLI and HTTP service share one typed query layer. The React viewer is in `../../website/observatory`.

[BENCHMARKS.md](BENCHMARKS.md) defines the qualification procedure and supported limits; measured evidence is linked there.

This is a composition package so HTTP and ClickHouse dependencies stay outside `searcher` and `nes-workload`. The generic campaign observer receives reservation and ordered admission callbacks. The no-op observer retains the ordinary campaign call path.

## Start locally

Use a licensed local Metroid ROM and QuickNES core. Neither is part of this repository or telemetry. On the telemetry host:

```sh
export HARMONY_CLICKHOUSE_URL=http://127.0.0.1:8123
export HARMONY_CLICKHOUSE_USER=observatory
export HARMONY_CLICKHOUSE_PASSWORD="$(openssl rand -hex 24)"
workloads/nes-observatory/scripts/dev-clickhouse.sh
cargo run --locked --release --manifest-path workloads/nes-observatory/Cargo.toml -- bootstrap
```

Keep the same password in a host secret store for subsequent commands. The development container binds ClickHouse only to loopback, has a 4 CPU / 8 GiB limit, and uses the pinned `26.3.33.24` image. Its data directory defaults to `.observatory-dev/clickhouse`. The table TTL is seven days by run start; use a bounded volume with at least 1 GiB free and monitor disk utilization. The script can use Docker by setting `HARMONY_CONTAINER_ENGINE=docker`, with a compatible volume mount policy.

On the search host, use a private URL for ClickHouse and set the same credential. Give each run a distinct empty output directory:

```sh
cargo run --locked --release --manifest-path workloads/nes-observatory/Cargo.toml -- run \
  --rom /private/metroid.nes --core /private/quicknes_libretro.so \
  --output /tmp/metroid-observatory-run --spool /tmp/metroid-observatory-spool \
  --seed 1 --workers 2 --executions 1000 --actions 4096
```

The command returns a 32-character run ID and report paths. The spool is local to the search host. It buffers at most 512 MiB across all run sessions when ClickHouse is down. After a restart, replay remaining batches with `export --spool-dir /tmp/metroid-observatory-spool/RUN_ID/SESSION_ID`; a repeated upload has the same batch token. Unreplayed batch payloads older than seven days are atomically replaced with a loss event at the next run or export, and every minute while a producer is active. The local loss-count file records discarded event rows. Replaying the marker makes the run visibly incomplete; expired telemetry cannot be recovered from the spool. A malformed batch is preserved and causes an explicit expiry error. Keep a single producer or exporter per spool root. `--no-telemetry` runs the same fixed-work campaign with the no-op observer for comparison. Source snapshots and ROM/core paths never enter event payloads; the identity includes hashes and search configuration.

Build the viewer with `cd website/observatory && npm ci && npm run build`, then start `nes-observatory serve --bind 127.0.0.1:8787 --static-dir website/observatory/dist` from the repository root. Open `http://127.0.0.1:8787` through a private tunnel. For a separate telemetry host, keep ClickHouse on its loopback interface and run the Rust query service there. Send batches over an authenticated private network or HTTPS proxy; do not publish ClickHouse or the unprotected viewer directly to the Internet. The browser calls only the Rust API. `/api/v1/health` reports database readiness.

## Query contract

The CLI returns JSON and the service exposes equivalent `/api/v1/runs`, `/runs/{id}/status`, `/runs/{id}/map`, `/runs/{id}/timeline`, `/runs/{id}/observations`, and `/runs/{id}/draw-table` routes under `/api/v1`. Intervals are half open `[from_ms,to_ms)`, measured from search start. Map queries allow up to one hour; timeline queries allow up to one day and 360 run-start-aligned buckets. An unaligned window that touches 361 buckets is rejected instead of truncating its final bucket. A map has 1 ms timestamp resolution and 32 by 32 cells per area. Common questions:

```sh
nes-observatory runs --json
nes-observatory status --run RUN_ID --json
nes-observatory map --run RUN_ID --metric selections --from-ms 0 --to-ms 30000 --area 16 --json
nes-observatory map --run RUN_ID --metric work --from-ms 0 --to-ms 30000 --as-of-ms 60000 --area 16 --json
nes-observatory map --run RUN_ID --metric new_places --from-ms 0 --to-ms 30000 --json
nes-observatory observations --run RUN_ID --from-ms 0 --to-ms 30000 --area 16 --map-x 3 --map-y 14 --min-missiles 5 --equipment-bits 4 --json
nes-observatory draw-table --run RUN_ID --at-ms 30000 --json
```

`work` is measured emulated frames charged to the selected parent cell and its selection interval. Because admission can occur later, `--as-of-ms` specifies what was known by that point; without it the answer is provisional until the status watermark passes the end of the interval. `positions` counts valid gameplay action observations and `territory` is cumulative first-seen coarse cells. `new_places` is a first observation in the chosen window, independent of archive novelty. Branches do not form one playthrough. The map color scale is fixed per metric, with cell numbers and a tabular top-cell alternative.

Observation examples preserve joint health, missiles, and equipment from one action observation. At most four valid details per admission are retained by uniform index stride; no matching detail means only that none was observed in this sample. Presence and first-seen cells use all valid action observations. Invalid menu, ending, death, BCD health underflow, and door-transition observations are excluded. Health and missiles are raw decoded values. The current Metroid policy has no empirical draw table: the draw-table endpoint explicitly returns `available: false` and `policy: alphabet_only`. It cannot provide historical probabilities unless Metroid adopts a stateful table policy in a separate search-policy change.

Each response identifies schema version, units, window, resolution, completeness, freshness, and ClickHouse query cost. Search-thread send uses a bounded 8192-event queue. Background batches cap at 1024 events or 1 MiB. Shutdown waits for the writer, then at most five seconds for upload. Queue overflow, spool overflow, and seven-day spool expiry are reported as loss; `run_end` or sequence gaps missing from storage make the run incomplete. ClickHouse queries cap execution at two seconds, memory at 256 MiB, bytes read at 1 GiB, results at 10,000 rows or 8 MiB, and concurrent HTTP queries at four. The viewer caches at most 24 map responses, debounces scrubs, and aborts obsolete requests.

Complete run status is cached for 30 seconds and active status for 1.5 seconds so historical scrubs do not rescan full-run status on each request.

The database stores raw events for seven days and uses `ReplacingMergeTree` plus insert tokens for retry safety. The typed query API applies `FINAL` so retries cannot double count even before background merges. Run identity and selection/admission timestamps are separate. Retention is by run start; after TTL removes a run, it is unavailable rather than reported as zero. Cumulative territory uses a 640-byte five-area bitmap checkpoint every minute of active admissions, then first-seen deltas from that checkpoint. A missing checkpoint falls back to discoveries since run start. There are no materialized aggregate tables. The one-hour window map cap and bounded ClickHouse read cap constrain history scans; larger raw histories require a future rollup design before extending retention or producer rate.

## Checks

```sh
cargo fmt --manifest-path workloads/nes-observatory/Cargo.toml --check
cargo clippy --locked --manifest-path workloads/nes-observatory/Cargo.toml --all-targets -- -D warnings
cargo test --locked --manifest-path workloads/nes-observatory/Cargo.toml
HARMONY_CLICKHOUSE_USER=observatory HARMONY_CLICKHOUSE_PASSWORD=... \
  cargo test --locked --manifest-path workloads/nes-observatory/Cargo.toml --test clickhouse -- --ignored --test-threads=1
```
