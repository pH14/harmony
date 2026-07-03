# oracle-elle — implementation notes (task 75, Surface 1)

An Elle-*shaped* transaction-isolation trace oracle: a `dissonance/explorer`
plugin (`impl Oracle`) that judges whether a recorded run violated a declared
isolation level, entirely offline. This is the **delegable** surface of task 75
(worker A); the probe mechanism and box gates are the frontier surface (see
`../explorer/IMPLEMENTATION-task75.md`).

## What it is

- **Op model** (`op.rs`) — `Op { session, txn, kind: Read|Write|Append, key,
  value(s), at }`, `Transaction` (+ commit/abort `TxnOutcome`), `History`. All
  `BTreeMap`-keyed, so every traversal is deterministic.
- **`OpDecode` seam** (`decode.rs`) — defined locally (rule 2). Two decoders,
  both fail-loud: `RecordDecoder` over `RunTrace.records` (the scrape-tier
  `elle op s=.. t=.. k=.. W|A|R=..` line format; non-`elle` lines ignored so op
  records ride alongside logs) and `EventDecoder` over `RunTrace.events` (the
  same fields as link-tier `GuestEvent` attributes). Both stamp ops with the
  record/event `Moment`.
- **Dependency graph** (`graph.rs`) — write-read, write-write, and read-write
  anti-dependency edges, plus **model-aware version-order recovery**: append
  keys recover order from the longest observed list (reads must be prefixes, a
  fork → `InconsistentOrder`); register keys recover order from the distinct
  observed values in first-observation time order. Deterministic iterative
  DFS `ww_cycle` (sorted nodes + neighbours, stack-safe on untrusted input).
- **Anomaly ladder v1** (`anomaly.rs`) — G0 dirty write (ww cycle), G1a aborted
  read (committed read of an aborted write), lost update (two committed
  read-modify-writes on one key based on the same version). Each carries a
  constructive witness (participating txns + keys + earliest violating moment).
  `IsolationLevel` (RU/RC/SI/Serializable) gates which rungs are forbidden.
- **`ElleOracle`** (`oracle.rs`) — the pure `Oracle`. `analyze` returns the
  witness-bearing `Anomaly`; `judge_checked` wraps it into a `Bug`; `judge`
  (the trait) reports no bug on a `DecodeError` (never a guessed anomaly — see
  below). The `Bug` carries the run's own terminal `StopReason` (usually
  `Quiescent`); the finding lives in the shared fingerprint's terminal
  signature (`oracle="elle"` + anomaly class + participating key set).

## Gates (all green, macOS)

1. **Standard suite** — `build` / `nextest` (25 tests) / `clippy -D warnings` /
   `fmt --check` / `cargo deny check` all pass. No `unsafe` → no Miri needed.
2. **Checker proptests (≥256 cases)** — `lost_update_is_always_caught_with_witness`
   (planted lost update + arbitrary anomaly-free noise → the exact `{T,T'}`/key
   witness, and a byte-equal re-judge), `serial_chains_are_always_clean`, and
   `judging_is_deterministic`; plus planted unit witnesses for G0/G1a and exact
   level gating.
3. **Offline property** — `offline.rs`: a `RunTrace` corpus is written to disk
   (per-run JSON — the shape task 65's `TraceStore` persists), reloaded, and
   re-judged by a **freshly-constructed** `ElleOracle`; the planted anomalies
   surface and a verb-counting mock `Machine` records **zero** calls. A second
   test re-judges the same corpus at a stricter level with no re-execution.

## Deviations considered

- **Schema-blind fault fingerprint coordinate.** The pinned fingerprint's
  coordinate 2 is "the set of fault-classed `Action`s in `env`". A pure trace
  oracle sees an *opaque* `Environment` and cannot enumerate faults, so
  `ElleOracle` mints `FaultCoord::none()`. Hashing the opaque blob instead was
  rejected (it carries `Moment`s and over-splits so hard it defeats mint-time
  grouping). The `FaultCoord` input is first-class, so the campaign's
  schema-aware path (and task 76) populate it with zero API change. Full
  rationale in `../explorer/src/fingerprint.rs`.
- **`DecodeError` vs. `Oracle::judge`.** `judge(&RunTrace) -> Option<Bug>` cannot
  return an error, and the checker must never *guess* an anomaly from an
  unrecoverable history. So `judge` returns `None` on a `DecodeError` (not a
  fabricated bug), and `judge_checked`/`analyze` surface the error for a campaign
  that wants it loud. This matches the thin-SDK ruling ("never a guess").
- **Model-aware version recovery.** An earlier draft applied append-model
  prefix-consistency to register reads too, which wrongly rejected legitimate
  register histories (two reads of different single values). Fixed: recovery
  branches on whether a key is ever appended.

## Known limitations (task-75 non-goals, deliberately)

- **Not a full Elle port.** Cycle-typed SI/serializability anomalies (G-single,
  G2) and linearizability are the follow-on ladder; v1 anchors on G0/G1a/lost
  update. `Serializable` here checks the same forbidden set as SI.
- **Version-order recovery is v1.** Append keys use final/longest reads; register
  keys use first-observation time order. Richer recovery (partial-order merge,
  intermediate-read version chains) is future work.
- **No public-API snapshot** committed (the quality-d gate can add one); the
  crate's surface is small and re-exported from `lib.rs`.

## For the integrator

- The crate depends **only** on `explorer` (the task-64 plugin pattern). It adds
  no root/workspace edits (the `dissonance/*` glob picks it up).
- The `OpDecode` seam is the integration point for the real workloads: the
  Postgres txn driver emits `elle`-tagged records (or `GuestEvent`s) per the
  formats in `decode.rs`; OTel spans (task 74) and SDK events (task 73) plug in
  as additional `OpDecode` implementations.
