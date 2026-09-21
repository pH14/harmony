<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Observatory performance qualification

This document defines how to qualify the Metroid search observatory before changing its producer rate, retention, query limits, or viewer behavior. Run-specific measurements and decisions belong in the associated pull request or external benchmark record. The initial qualification evidence is in [PR #366](https://github.com/pH14/harmony/pull/366).

## Fixed-work search comparison

Use a licensed local Metroid ROM and QuickNES core. Set authenticated HARMONY_CLICKHOUSE_URL, HARMONY_CLICKHOUSE_USER, and HARMONY_CLICKHOUSE_PASSWORD as described in [README.md](README.md). Run the benchmark script with private asset paths and an output directory outside the repository:

    python3 workloads/nes-observatory/scripts/benchmark.py \
      --rom /private/metroid.nes --core /private/quicknes_libretro.so \
      --output /tmp/observatory-bench --workers 2 --repeats 3 \
      --executions 1000 --actions 256

The script rotates disabled, healthy, and disconnected telemetry in each pair. It records campaign stream and report SHA-256, search/coordinator wall time, full-process wall and CPU time, peak RSS, drain time, queue/spool losses, and remaining spool bytes in results.json. Disconnected mode points only the telemetry exporter at an unreachable loopback port. The script rejects a pair if its search stream or report differs.

Use paired runs at one, two, and four workers. Repeat until the spread can resolve a one-percent search-wall difference; a small sample that cannot resolve it does not establish the throughput target. Also confirm identical campaign streams and reports, no loss on a healthy connection, continued search progress while disconnected, bounded shutdown, and the 512 MiB spool cap across all sessions.

## History and query load

The synthetic_history.py script repeats event shapes from a completed local run into ClickHouse with new synthetic run and session IDs and contiguous event IDs. It checks source completeness, labels synthetic identity, and enforces a six-million-row default cap. It copies no ROM, core, snapshot, or source file.

Exercise a dense hour and a sparse day. Sample cold and warm status, timeline, selection/work maps, cumulative territory, and joint observation filters while one producer ingests. Record median, p95, maximum, rows and bytes scanned, database disk and memory, and visible event lag. Verify retry idempotence, finalized boundaries, incomplete status after missing events, and explicit 429 and 400 responses at query limits. Keep the raw measurements with the pull request or an external benchmark record.

The supported starting envelope is one producer, one viewer, and a pinned ClickHouse 26.3.33.24 instance limited to 4 CPUs and 8 GiB RAM. Map windows cap at one hour; timelines cap at one day and 360 buckets. Queries cap reads at 1 GiB, output at 8 MiB or 10,000 rows, execution at two seconds, and concurrent HTTP requests at four. The viewer caches at most 24 map responses and cancels obsolete requests.

Raw events have seven-day run-start TTL. The local spool has a 512 MiB global byte cap and replaces batches older than seven days with durable loss markers when a run or export starts and once per minute while a producer is active. The current database has no materialized aggregate rollups. Requalify dense history before raising producer rate, map span, or retention.
