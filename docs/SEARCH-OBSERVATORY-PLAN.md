# Metroid search observatory: implementation handoff

Status: proposed implementation plan, 2026-09-20. Intended implementer: Codex
Sol with high reasoning effort. This document authorizes implementation choices
within the scope below; it does not claim the system already exists.

## Outcome

Build a usable way to watch an actual Harmony Metroid search while it runs:
a spatial heatmap changes as the search allocates effort and discovers places;
the user can pause, scrub through earlier search history, inspect the input draw
table, and ask structured questions about observations. Agents must have the
same query capabilities through a documented API and JSON CLI. The website
uses that API. ClickHouse stores and queries telemetry. All application backend,
collector, exporter, API, and CLI code is Rust, following repository conventions.

Deliver working software and measured resource costs, not just a dashboard
mockup or architecture proposal. Start with Metroid; preserve a clean workload
boundary without implementing distributed-systems views or other game adapters.
Do not change search policy to improve how the visualization looks.

## Working instructions

- Do implementation, builds, tests, benchmarks, and runtime validation on
  `msr1`. Use a dedicated Git worktree there for this task, on a `codex/` branch.
  Inspect the existing repository and worktrees on `msr1` before creating it;
  preserve other tasks' checkouts and uncommitted changes. Bring this plan into
  that worktree as the handoff. Do not implement in the local Mac checkout or
  directly in the shared primary checkout on `msr1`. If access to `msr1` is
  unavailable, report the blocker rather than silently switching hosts.
- Read AGENTS.md, CONTRIBUTING.md, REVIEWING.md, dependency-boundary rules,
  relevant component READMEs, and applicable skills before implementing.
- Ship this multi-milestone plan as one PR with a semantic commit per milestone.
  Preserve unrelated work. At plan-writing time `website/` and several
  experiment directories are untracked; inspect current status again.
  Do not assume the existing website is disposable or owned by this task.
- Inspect the actual stack and choose crate placement, Rust HTTP/runtime and
  ClickHouse client libraries consistent with current boundaries. Do not add
  ClickHouse/network dependencies to the deterministic execution core.
- Choose and pin a maintained frontend stack after checking current official
  documentation. TypeScript and a mainstream component framework with a
  canvas/WebGL or SVG map are reasonable. Use existing frontend work if it fits
  and can be extended without disturbing unrelated changes. Production backend
  endpoints must remain Rust; no parallel JavaScript API server.
- Rust source comments are prohibited except SPDX and unsafe safety invariants.
  Put architecture and usage knowledge in nearby READMEs. Avoid new unsafe code;
  exercise any required unsafe logic under Miri.
- Make routine engineering decisions autonomously. Record meaningful tradeoffs
  and measured limits in the PR and component docs. Record deferred work as
  GitHub issues. Use the shipping-code skill once implementation is pushed.

## Existing integration points to verify

- `dissonance/searcher/src/search/campaign.rs`: workload contracts, reservation
  and ordered admission, and live progress writing. The current progress writer
  serializes and flushes synchronously; do not attach large payloads there.
- `dissonance/searcher/src/search/archive.rs`: parent selections, retention,
  eviction, archive identity, and existing selector accounting.
- `dissonance/searcher/src/search/draw_tables.rs` and `empirical_steps.rs`:
  historical frequency map (currently capped at 4,096 distinct actions), recent
  retained suffixes, weights, table publication cadence, and replay versions.
- `workloads/nes/src/metroid/`: coordinate decoding, resource observations,
  equipment, named progress, terminal policy, and reporting. Existing diagnostics
  count visited map cells; final censuses summarize retained endpoints.
- `workloads/nes/src/bin/nes-eval.rs` and `benchmarks/search/`: the actual Metroid
  launch and fixed-work evaluation paths. The shared `harmony search` CLI does
  not currently dispatch Metroid; don't assume a documented SMB command does.
- `consonance/telemetry`, `docs/ARCHITECTURE.md`, and dependency tooling: inspect
  their responsibilities before reusing a similarly named crate.

## Product contract

The initial screen lists runs and opens a selected run directly into a useful
map. It displays run status, telemetry freshness/completeness, search work,
elapsed time, and discovery history. A real run must visibly update within a few
seconds under the documented deployment budget.

The main map supports these clearly distinguished metrics:

1. Selections originating in each coarse spatial cell during a selected window.
2. Execution work attributable to those selections, with declared units.
3. Observed positions during a window and cumulative observed territory.
4. Newly observed places during a window, distinct from archive novelty and
   resource improvements at already observed places.

Use stable game/world coordinates with area separation and useful room-level
zoom. Do not overlay unrelated screens. A schematic map reconstructed from
observed coordinates is sufficient; no commercial ROM assets or external map
art are required. Include a scale legend, explicit metric and time window,
hover/click details, and accessible alternatives to color alone. Keep color
scales stable while scrubbing; make normalization choices visible.

Provide live-follow, pause, a draggable timeline, selectable window size, and
return-to-live. Scrubbing must reconstruct the selected historical interval or
as-of state, not recolor today's totals. Show available time resolution and data
gaps. New discoveries and recent activity should be visually distinguishable
from the faint cumulative footprint. No autoplay motion when reduced motion is
requested. Handle empty, loading, disconnected, completed, and partial runs.

A draw-table panel shows named controller combinations, duration where it is
actually part of the action, recent and historical contributions, weighted
empirical probability, publication/version position, and change over time.
Explain the distinction between the empirical table and the overall input
policy, including fresh alphabet or other mutation paths. Do not interpret a
retained suffix's actions as causal proof of success. Snapshot published state
without flushing pending table changes or advancing search RNG/state.

Include a query/filter panel usable without SQL: area/room or selected map
region, time interval, minimum health/missiles, equipment bits, outcome, and
metric. Supply saved examples. An agent answers natural-language questions by
calling the CLI/API; building an embedded LLM chatbot is outside this release.

Required questions, answerable through both UI and API/CLI:

- Where were selections concentrated in the last window, and in an earlier one?
- Which regions consumed the most execution work? Which yielded new places?
- Did any recorded observation near a selected doorway have at least five
  missiles and the requested equipment? Return counts and example identities.
- Where did we observe high health or missiles, under the same-state filters?
- What did the empirical draw table contain at a selected historical point?
  How did its distribution change between two points?
- How complete are these answers, and what temporal/spatial resolution was used?

## Telemetry semantics and collection

Keep observation collection independent of selection, RNG, admission ordering,
retention decisions, replay state, and deterministic campaign bytes. Enabled,
disabled, slow, and failed telemetry must produce the same fixed-work search.
Wall-budget runs can change outcomes with speed; use fixed-work comparisons.

Define versioned event/batch contracts and identities before adding hooks:
run/build/workload/policy identity; unique producer session and batch/event IDs;
selection/reservation ID; admission sequence; parent/archive identity where
available; execution/action position; monotonic elapsed time; stable units.
Preserve branch links sufficient to identify related records, without building
an arbitrary temporal tree-query engine. Distinguish reservation time, admission
time, and workload trajectory position. Never imply that branches form one play.

Count actual selections when they occur; don't reconstruct historical selection
counts from surviving archive members. Keep skipped/failed selections visible.
Attribute measured execution work when results arrive, specifying how delayed
work maps to its originating selection window. Don't substitute path length,
emulated lifetime reset counters, or wall CPU estimates for measured work.

Keep Metroid position/resource interpretation in the workload. Preserve joint
fields from the same observation: independent maxima and equipment unions cannot
answer conjunctive queries. Define which observation boundary is collected and
which invalid/menu/terminal/transition observations are excluded. Existing
action observations are a starting point; full-frame logging is not a default.
Handle known transient terminal/health decoding semantics explicitly.

Use exact coarse selection summaries. Investigate bounded detailed endpoint
observations versus joint aggregates with enough fidelity for required queries.
State explicitly where sampling, quantization, omission, or aggregation limits
answers. No matching sampled row means 'not observed in this sample,' not
'never happened.' Under loss, even otherwise exact summaries are incomplete.
The API must carry that qualification, not just a UI warning.

Collection must have bounded memory, payload sizes, queue depth, and shutdown
time. Expensive serialization/compression, disk/network I/O, and database waits
belong in background components. Bound any local spool by bytes and age. Define
and test overload behavior: coalescing summaries or dropping detail with loss
accounting is acceptable; blocking search indefinitely or silently claiming
completeness is not. Use an independent telemetry sequence for detectable gaps.
Aggregate loss markers in bounded state so a full queue cannot hide all loss.

Persist enough run metadata to recognize incomplete/crashed runs and distinguish
search completion from successful final export. Retries must not double-count
selections, work, observations, or materialized aggregates.

## ClickHouse and Rust query service

The worker owns researching, implementing, and validating the ClickHouse design.
Use current official docs and an actual pinned server in integration checks.
Choose tables, sort/partition keys, batching, compression, retention, rollups,
and deduplication based on measured event shapes. Avoid one insert per event,
one partition per tiny window, and unbounded high-cardinality rollup products.

Provide a reproducible local development deployment and documented deployment
with ClickHouse/export/query service on a separate telemetry host. Include
schema bootstrap/migrations, health/readiness, restart recovery, configurable
disk retention, and CPU/memory limits. Browser traffic goes to the Rust API,
not directly to ClickHouse. Bind local services locally by default; document
authenticated private remote operation and keep credentials out of the browser.
Do not upload ROMs, emulator cores, snapshots, or private asset inventories.

Build one typed query layer shared by HTTP and CLI. Support run discovery,
metadata/status, map aggregates, bounded timeline series, filtered observation
queries with paginated examples, draw-table history/diffs, and telemetry health.
Return machine-readable JSON with schema version, units, boundaries, resolution,
freshness watermark, completeness, and continuation tokens where relevant.
Document filter semantics and exact versus approximate answers.

Provide structured agent queries rather than requiring arbitrary SQL. Validate
and parameterize filters. Set query time, memory, scanned-data/result-size and
concurrency limits, cancel obsolete requests, and separate ingest capacity from
interactive query capacity. Expose the actual ClickHouse cost statistics needed
to assess queries. Read-only exploratory SQL can be deferred; it must not become
an unlimited bypass of those limits if provided.

For time scrubbing, use bounded multiresolution summaries and/or checkpoints
with deltas, selected based on required semantics. Preserve finer recent history
and coarser older history under explicit retention rules. Avoid full-history
rescans and one database query per pointer movement. Debounce/cancel requests,
cache completed intervals, prefetch small bounded neighborhoods, and bound
browser memory. A cumulative view must not sum repeated cumulative snapshots.
All UI/API/CLI consumers must agree on finalized windows versus provisional data.

Proposed command shape, subject to existing CLI integration:

```text
harmony telemetry runs --json
harmony telemetry status RUN --json
harmony telemetry map RUN --metric selections --from ... --to ... --json
harmony telemetry observations RUN --region ... --min-missiles 5 --json
harmony telemetry draw-table RUN --at ... --json
harmony telemetry serve ...
```

Document a real command sequence that starts Metroid with telemetry, opens its
viewer, and queries the same run from an agent. Integrate the actual Metroid
runner without requiring a general expansion of shared NES dispatch.

## Resource goals and evidence

The initial search-throughput regression target is <=1% in the normal telemetry
mode. This is a target to measure, not an assumed property. Use repeated paired
fixed-work runs on a documented host, report variance, and don't claim sub-1%
precision when benchmark noise cannot support it. Measure enabled healthy,
enabled disconnected/overloaded, and disabled modes. Include worker scaling,
coordinator time, host RSS, telemetry bytes/sec, spool growth, and final drain.

As a starting telemetry-box envelope, test a 4-vCPU/8-GiB deployment with a
documented disk cap, one full-rate search producer, one live viewer, and an
agent querying while scrubbing. Adjust only with measured justification and
state the supported event rate/run size explicitly. Target visible updates
within 2-5 seconds and p95 common query/scrub responses under 500 ms once data
is ingested. Set concrete default queue, spool, retention, query, and browser
caps in the first milestone; configuration must have bounded safe defaults.

Load tests need a reproducible hour of representative telemetry and a longer
synthetic history to exercise rollups/retention. Label synthetic results clearly.
Report ingest lag, database CPU/RAM/disk, rows/bytes scanned, query latency, and
browser responsiveness. Test concurrent live ingest and historical scrubbing.
The accepted product must fit its declared limits; if goals conflict, reduce
observation detail/time resolution explicitly rather than silently slowing
search or exhausting storage.

## Milestones and acceptance

### 1. Contracts and a runnable end-to-end slice

Inspect source and settle component placement and frontend stack. Implement
versioned contracts, ClickHouse bootstrap, bounded export, and Rust query/CLI
plumbing. Connect an actual Metroid selection/position stream to a minimal live
map through the real API. Add synthetic fixtures for CI and document the chosen
resource caps and sampling/window semantics. This milestone must exercise the
entire path; do not spend it building disconnected infrastructure.

Acceptance: a local real Metroid search changes the map; a CLI query returns
matching counts; stopping ClickHouse does not block the search. If private
assets are unavailable, complete the synthetic path and report the specific
missing asset qualification without fabricating real-run evidence.

### 2. Accurate history and useful queries

Implement selection/work accounting, Metroid joint observations, draw-table
snapshots, durable batch identity/retry semantics, retention and rollups, and
the full typed query surface. Resolve asynchronous admissions and provisional
windows. Add API/CLI examples for all required questions.

Acceptance: known fixtures prove exact expected counts and filters across
branches, retries, evictions, time boundaries, retention tiers, and gaps.
Historical draw-table probabilities match the actual published sampler inputs.
No future data leaks into an as-of answer.

### 3. A polished watch-and-investigate website

Finish live map layers, stable color scales, area/room navigation, timeline
scrubbing, filter/query examples, detail panel, draw-table history, freshness,
and failure states. Fetch through the same API an agent uses. Include responsive
layout, keyboard controls, reduced motion, and clear loading/error feedback.

Acceptance: visually inspect the running application, not just generated markup.
Record a short demonstration or screenshots of real live progress, paused
historical inspection, a resource-filtered query, and draw-table changes. Exercise
large histories and rapid scrubbing without unlimited requests or browser growth.
Mark any synthetic demonstration as such.

### 4. Performance qualification and shipping

Run the fixed-work and telemetry-host benchmarks, correct bottlenecks, verify
bounded outage recovery and shutdown, and complete deployment/operator docs.
Add checks to the applicable repository CI lanes: synthetic tests and pinned
ClickHouse integration must not depend on licensed game artifacts. Run relevant
Rust, frontend, dependency, formatting, and review checks from current CI.

Acceptance: deterministic stream/report comparisons with telemetry on/off and
under failure pass; repeated ingestion is counted once; malformed/oversized
requests are bounded; long-run storage and query load stay within declared
limits. Publish benchmark inputs/results and a concise limitations list. Supply
the one PR, semantic milestone commits, launch/query commands, and demo evidence.

## Deferred scope

No custom database, general-purpose tree computation engine, distributed-systems
visualizations, other game adapters, per-frame video streaming, full archive
snapshot replication, hosted multi-tenant product, or embedded LLM chat.
Example records may link to existing trace identities; guaranteed arbitrary
historical state restoration and click-to-film are follow-ups, since evicted
state and snapshot lifetime require separate design.

## Background sources

- [Antithesis: Metroid](https://antithesis.com/blog/2025/metroid/): streamed
  observations, spatial/resource queries, originally using BigQuery.
- [Antithesis: Pangolin](https://antithesis.com/blog/2025/testing_pangolin/):
  specialized computation over branching event histories.
- [ClickHouse asynchronous inserts](https://clickhouse.com/blog/asynchronous-data-inserts-in-clickhouse)
  and [materialized views](https://clickhouse.com/blog/using-materialized-views-in-clickhouse).
  Verify current version-specific behavior, especially retry/deduplication and
  aggregate consistency, before choosing an ingestion strategy.
