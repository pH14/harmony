<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Observatory qualification on msr1

These results were collected on 2026-09-20/21 on the dedicated msr1 worktree. The host is Arm64 with 12 logical CPUs and 54 GiB RAM. ClickHouse 26.3.33.24 ran in a loopback-only Podman container limited to 4 CPUs and 8 GiB RAM. The Rust query service and one React viewer used the same host through a private tunnel. No licensed ROM or core bytes are included in this repository or in the synthetic history. All measurements use a local licensed Metroid ROM and QuickNES core, 256 actions per execution, and a fixed seed per paired set.

## Reproduce

Set authenticated HARMONY_CLICKHOUSE_URL, HARMONY_CLICKHOUSE_USER, and HARMONY_CLICKHOUSE_PASSWORD, bootstrap the database, and build the release binary as described in [README.md](README.md). The benchmark script checks the authenticated connection before it starts. Use private asset paths:

    python3 workloads/nes-observatory/scripts/benchmark.py \
      --rom /private/metroid.nes --core /private/quicknes_libretro.so \
      --output /tmp/observatory-bench --workers 2 --repeats 3 \
      --executions 1000 --actions 256

Each pair runs disabled, healthy, and disconnected telemetry in rotating order. It records the campaign stream and report SHA-256, search/coordinator wall time, full-process wall and CPU time, peak process RSS, drain time, queue/spool losses, and remaining spool bytes in results.json. Disconnected mode points only its telemetry exporter at an unreachable loopback port. The campaign still runs. The script refuses a pair whose stream or report differs.

For storage load, scripts/synthetic_history.py --source-run RUN_ID --repeats N --period-ms PERIOD --label LABEL repeats a completed real run's event shapes inside the local ClickHouse database, with new synthetic run/session IDs and contiguous event IDs. It enforces a six-million-row default cap, checks the source run is complete, and labels its identity as synthetic. It does not copy a ROM, core, snapshot, or source file to the database.

## Fixed-work search

The two-worker, three-pair result is the main throughput sample. Negative overhead means that particular enabled run finished faster than its disabled pair; it is measurement noise, not a speedup claim.

| Workers | Pairs | Healthy search-wall difference vs disabled | Disconnected difference | Paired outputs |
| --- | ---: | --- | --- | --- |
| 1 | 1 | -0.13% | +1.87% | Identical |
| 2 | 3 | +0.32%, +1.64%, +2.14% (median +1.64%) | -0.30%, -0.62%, +0.54% (median -0.30%) | Identical in every pair |
| 4 | 1 | -1.29% | -3.32% | Identical |

The one- and four-worker rows are scaling spot checks, not variance estimates. The three-pair spread does not resolve a one-percent throughput change, so the <=1% target remains unproven. Two-worker enabled peak process RSS was about 42 MiB versus about 32 MiB disabled. Healthy two-worker drains were roughly 0.3-0.4 seconds and left zero spool bytes; disconnected drains stopped after about five seconds with roughly 3.2 MiB/1,000 executions on disk, with no queue or spool loss. One and four workers also had zero loss, and their authenticated healthy runs left no spool bytes. The spool byte cap is 512 MiB across run sessions; a regression test exercises the boundary with prior-session data. At the tested disconnected two-worker rate, an hour-long outage would exceed that cap and correctly surface loss; operators must restore or replay sooner.

The real 10,000-execution, two-worker run completed in 36.249 seconds of search wall time and 0.311 seconds of drain, charged 1,347,619 emulated frames, and retained 63,487 events. Status showed 10,000 admissions, 10,001 parent selections (one duplicate skip), 12 observed map cells, no sequence gaps or recorded loss, and a complete finalized watermark. During a separate 5,000-execution run polled every 250 ms, the visible-event lag across 69 samples had median 1.299 seconds, nearest-rank p95 2.217 seconds, and maximum 2.468 seconds. That run also finalized with zero loss.

A disconnected 1,000-execution two-worker run produced seven spool batches. Replaying them into ClickHouse returned a complete run with 1,000 admissions, 6,577 retained events, zero gaps and losses. A second export uploaded zero batches, and the logical event count remained 6,577.

## History and queries

The full-rate synthetic hour repeated a completed four-detail Metroid run 889 times: 5,845,177 events over 3,601,339 ms. A separate sparse synthetic day used 30,218 events over 86,400,000 ms. Both are marked synthetic and query as complete. After these and other test runs, the local ClickHouse data directory occupied 463 MiB; an idle snapshot showed 1.058 GB container memory of its 8 GB limit. These are shared-database figures, not per-run allocations.

HTTP samples below came from ten or twelve sequential requests through the Rust API. They include JSON transfer on loopback; p95 uses nearest rank. A finalized status is cached for 30 seconds, while active status is cached for 1.5 seconds. The viewer fetches status before the related historical panels, so warmed values are the common scrub path.

| Query against synthetic history | Warm median | Warm p95 / maximum | Last ClickHouse scan |
| --- | ---: | ---: | ---: |
| Hour timeline, 60 s buckets | about 254 ms | about 268 ms | 5.88M rows / 559 MB |
| Hour selection map | 244 ms | 278 ms | 5.88M rows / 553 MB |
| Hour joint-observation examples | 250 ms | 288 ms | 255k rows / 34 MB |
| Hour cumulative territory map | 35 ms | 57 ms | recent checkpoint 131k rows / 11 MB, delta 22k rows / 2 MB |
| Sparse day timeline, 240 s buckets | 20 ms | 36 ms | 49k rows / 5 MB |
| Sparse day territory map, final hour | 46 ms | 54 ms | checkpoint 49k rows / 4 MB, delta 12k rows / 1 MB |

The cold first hour status read took 368 ms and scanned 5.88M rows / 677 MB. A timeline request that also had to compute this cold status took 637 ms. That cold case exceeds the 500 ms query target; the warmed common scrub path stayed below 500 ms in these samples. In an additional concurrent test, a 5,000-execution live producer ingested while the API alternated hour status, timeline, selection-map, and territory-map requests. Warm timeline requests were 233-284 ms, selection maps about 235-292 ms, and territory maps about 44-59 ms; no producer loss occurred. The viewer was also inspected with the real live and completed runs, paused scrubbing, filters, narrow and desktop viewports, and rapid cursor changes.

The current supported envelope is one full-rate, two-worker producer of roughly 6 million telemetry events/hour, one live viewer, and bounded query traffic on the tested 4-CPU/8-GiB ClickHouse host. An hour selection/timeline query scans over half a gigabyte, so higher event rates, longer high-resolution queries, or a larger seven-day history need aggregate tables and renewed qualification. Map windows are capped at one hour, timelines at one day and 360 buckets, query reads at 1 GiB, output at 8 MiB / 10,000 rows, execution at two seconds, and concurrent HTTP queries at four. A 12-request burst returned four successes and eight explicit 429 responses. Invalid run IDs, coordinates, cursors, and oversized limits returned 400. The browser limits map responses to 24 and cancels obsolete requests.

## Limits

The source Metroid search uses the AlphabetOnly policy and does not publish an empirical draw table. The endpoint and viewer report this as unavailable; they do not invent historical probabilities. Observation examples sample at most four valid action states per admission, while presence and first-seen territory use all valid states. The ClickHouse table has seven-day raw-event TTL and no aggregate rollup, so long-term dense history is not supported. The spool has a 512 MiB global byte cap and automatically replaces unreplayed batch payloads older than seven days with durable loss events during the next run or export, and once per minute while a producer is active. A synthetic ClickHouse integration test verifies that replayed expiry markers make a run incomplete. Without a running producer, a new run, or export, no process is available to sweep the spool. No confidential game asset is needed for CI.
