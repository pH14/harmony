# `link` (task 73) — implementation notes

The host-side link-tier plugin: it turns the events a cooperating in-guest
workload emits (via `harmony-sdk`) into the search-plane's replay-plane
vocabulary. Depends only on `explorer` (task-64 rule-2 layout), beside
`runtrace` (task 65).

## What it is

- **`decode_events`** (`src/decode.rs`): raw `(Moment, event_id, bytes)` →
  typed `(Moment, GuestEvent)` for `RunTrace::events`. **Total and panic-free**
  on arbitrary bytes — an unrecognized namespace or a malformed payload for a
  known one falls back to a `kind = "unknown"` event carrying the raw id + bytes,
  never a panic (≥512-case proptest).
- **`Catalog` / `CatalogReport`** (`src/catalog.rs`): the declared-at-init point
  set (parsed from the SDK catalog blob) folded against the fired set into
  `fired ⊎ never_fired`. Tier-blind, keyed by name. `Catalog::fold(decl, events)`
  is the one call a campaign makes per run.
- **`LinkSensor`** (`src/sensor.rs`): an `assert_sometimes`/`reachable` hit or a
  state-register change → a `(Moment, Feature)` on the link channels (assert =
  `ChannelId(16)`, state = `ChannelId(17)`).
- **`AlwaysViolation`** (`src/oracle.rs`): a `StopReason::Assertion` terminal → a
  `Bug` with the run's genesis-complete `env` and a stable fingerprint.

## Key decisions

- **Report format unified with task 66.** `CatalogReport` mirrors
  `matcher::Catalog`/`CatalogReport` (two disjoint `BTreeSet<String>` keyed by
  name). Because `link` may not depend on `matcher` (surface-list rule), this is
  the **minimal shared type** — *noted for task 66*: the integrator can unify by
  feeding `Catalog::declared()` names into `matcher::Catalog::declare(name, role)`.
  The report round-trips through serde (gate 2).
- **Fingerprint parity with the explorer.** `AlwaysViolation`'s `Bug`
  fingerprint restates the explorer's `dissonance.explorer.bug.v1` `Assertion`
  digest byte-for-byte, so a link-minted `Bug` dedups against an explorer-minted
  one. The parity is *tested* (`tests/sensor_oracle.rs` cross-checks against
  `explorer::TerminalOracle`), not just asserted. *Noted for the integrator:* a
  shared `fingerprint` helper (making the explorer's private one `pub`) would be
  cleaner; the scheme is restated here with a cross-reference.
- **State feature packing.** A state feature id packs `(reg & 0xFFFF) << 48 |
  (value & 0x0000_FFFF_FFFF_FFFF)`. The truncation only ever *collapses* two
  features into one cell — it never invents novelty, so it is a coverage
  trade-off, never a correctness bug, and both bounds are far beyond any
  realistic register count or state magnitude.
- **Sensor scope.** Only hits and state changes become features (per the spec).
  Buggify results are decoded and catalogued but are **not** features by default;
  a directed-search follow-on (task 70) is the never-fired report's consumer.
- **Wire mirror.** `src/wire.rs` privately mirrors `guest/sdk/src/wire.rs` (the
  canonical source). `tests/decode.rs` restates the byte format as goldens; if
  the two ever drift, a golden breaks on one side.

## Admission (gate 2)

A `sometimes` hit is admitted as a checkpoint candidate by the spine `Archive`
on the toy: `tests/sensor_oracle.rs` runs `CoverageArchive::admit` with the
`LinkSensor` and a `Fork` at the hit's `Moment`, and the link feature claims a
fresh cell. Per-hit checkpoint candidacy in a real campaign requires the
campaign's `CellFn` config to include the link channels — the sensor only
*produces* the features; the archive decides novelty (task 64 semantics).

## Gates

- `cargo test -p link` — 22 tests across decode / catalog / sensor+oracle, plus
  the ≥256/≥512-case proptests — green.
- `cargo clippy -p link --all-features --all-targets -- -D warnings`,
  `cargo fmt -p link -- --check` — clean.

## For the integrator

The end-to-end box path (SDK guest → doorbell → `Moment`-stamped EventSink →
wire → `RunTrace.events` → these four pieces) is in the repo-root
`docs/history/IMPLEMENTATION-task73.md`. The `RunTrace.events` population is in
`dissonance/conductor` (out of task 73's surface) — named there as the wire hop
the foreman sequences.
