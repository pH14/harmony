# Task 66 — `dissonance/matcher`: implementation notes

The declarative signal DSL + role router, as one plugin crate living entirely
behind the Scoring seam. A generic `MatchSensor`/`MatchOracle` evaluates
config-authored match expressions over any spine `Matchable` and routes every
match by its one declared `Role`.

## Module map

- `signal.rs` — the DSL types (`SignalSet`/`SignalDecl`/`MatchExpr`/`Role`/
  `During`/`SignalId`) and the JSON parse (`from_json`) + serialize (`to_json`)
  path. Parsing goes through a private wire form so each malformed class maps to
  a typed `MatchError`.
- `glob.rs` — the hand-rolled two-pointer glob (no `regex`; not on the
  whitelist).
- `value.rs` — three canonical byte views of a spine `Value`: glob-comparison
  bytes, tagged hash bytes, and non-negative-integer extraction.
- `router.rs` — the `ChannelSource`/`ContextSource` seams and the generic
  `MatchSensor` (`impl Sensor`) / `MatchOracle` (`impl Oracle`).
- `catalog.rs` — `Catalog` + `CatalogReport` (the declared set is the catalog;
  `never_fired = declared − fired`).
- `stub.rs` — the shipped `ChannelSource`/`ContextSource` **test stubs**
  (`RecordRec`, `TraceRecords`, `OwnedRecords`, `FaultMoments`).
- `error.rs` — the `MatchError` enum.

## Design decisions (considered → chosen)

- **Per-signal channel = rank of the name in the sorted name set.** The router
  table says "channel = the signal", and gate 4 requires that permuting a
  signal's declaration position never change another signal's output. A
  *declaration-index* channel fails that (moving one signal renumbers the
  others); a *name-hash-into-u16* channel collides too often to keep totality
  clean. Sorted-name rank is both permutation-invariant (it ignores declaration
  order) and collision-free (names are unique). `never` signals occupy a rank
  too but emit no feature, so cross-role leakage is impossible by construction.
  `SignalSet` construction rejects a set larger than `u16::MAX + 1`
  (`MatchError::TooManySignals`), so a rank can never wrap and silently merge two
  signals onto one channel (codex P2 #2).
- **`never` tie-break by name, not declaration order.** `MatchOracle` iterates
  `never` signals in name order (`never_idx`), so when two `never` rules match
  the same record the earliest-verdict pick is permutation-invariant (gate 4)
  rather than a function of where they sit in the file.
- **"The matched value's canonical bytes" = kind ‖ the expr-constrained attrs'
  actual values, length-delimited and variant-tagged.** `Matchable` exposes only
  `attr(k)` lookup, not attribute *enumeration*, so the only value bytes the DSL
  can canonicalize are those of the keys the expression names. This is the
  natural reading of "the matched value" and keeps cell ids / never fingerprints
  stable with no codebook. A cell author who wants coarser cells uses an exact
  attr match instead of a glob (over-splitting is a quality knob, not a
  correctness issue — "a hash collision merely merges cells, safe").
- **`state_max` bucket = bit length of the running max** (`64 −
  leading_zeros`), emitted only when the bucket strictly increases — the IJON
  `IJON_MAX` register moved from source to config. A non-integer / absent /
  negative `attr_max` value is a **counted decode miss** (`decode_misses()`),
  never a panic and never a feature; it does not fail the match (`attr_max` is an
  extraction, not a predicate).
- **JSON, not YAML** (the task ruling): the whitelist stays untouched. Parsing
  is total on untrusted input — every malformed class (`UnknownRole`,
  `DuplicateName`, `TooManySignals`, bad type via `Parse`, `UnknownDuring`) is a
  typed error. **Fail-closed** (codex P2 #1): the top-level `signals` key is
  *required* (a config missing it is an error, not a silent empty set — an
  explicit `"signals": []` stays legal) and `deny_unknown_fields` at every level
  rejects a misspelled key rather than silently ignoring it (which could vacate
  the set or broaden a match).
- **Output ordering.** Both structs process records in a canonical
  `(Moment, index)` order and the sensor sorts its stream by
  `(Moment, channel, id)`, so evaluation is a deterministic, permutation- and
  emission-order-invariant function of the trace (no `HashMap`, no floats,
  seedless).

## Provisional / superseded elsewhere

- The `never` `Bug` fingerprint `sha2(name ‖ kind ‖ matched attr bytes)` is
  **provisional** per the spec: task 75 pins the authoritative stable-coordinate
  `Bug` fingerprint schema and supersedes this minting site.

## What the integrator must know

- **Catalog fired-marks are assembled by the caller.** A campaign unions
  `sensor.fired(t)` (the `sometimes`/`cell`/`state_max` matches) with
  `oracle.fired(t)` (the `never` matches) and passes the union to
  `Catalog::report(&fired)`. The two fired sets are disjoint by role.
- **Channel numbering is local to one `SignalSet`.** Channels are ranks within
  the set's name space; a campaign composing this crate's features with another
  plugin's should treat `ChannelId`s as a per-plugin namespace (the spine already
  models channels as a campaign convention). Adding a signal renumbers ranks —
  that is a config change, not an in-run event.
- **Stubs only.** `ChannelSource`/`ContextSource` are the seams; the concrete
  adapters (log records task 67, SDK/link events task 73, OTel spans task 74) and
  the production schema-aware fault index (task 69) are later tasks. `stub.rs`
  ships example/test implementations — `OwnedRecords` demonstrates the
  "records reassembled outside the trace verbatim" case the seam exists for.

## Known limitations

- `during:` ships exactly one predicate, `no_faults`; the `During` enum is
  `#[non_exhaustive]` so the vocabulary extends without a spine change.
- More than `u16::MAX + 1` signals in one set is a hard `TooManySignals` error
  (no real config approaches this).
- **Glob complexity** (codex P2 #3): `glob.rs` is the standard two-pointer
  star-backtracking matcher the spec names. Its guarantee is **no catastrophic /
  exponential backtracking** (the naive-recursion failure mode); the strict
  worst case is `O(pattern · text)` — a `*` followed by a literal run that
  repeatedly partially matches — *not* strict linear time. For the short
  attribute globs the DSL matches this is negligible; the doc claim was corrected
  to state this precisely, and `star_then_literal_run_completes_fast` regression-
  tests a large pathological input completing near-instantly. A true
  `O(pattern + text)` matcher would need KMP/Z per `*`-delimited segment —
  disproportionate here and out of scope; flag if genuine linear time is wanted.

## Gates

All green on macOS (dev) — `build`, `nextest` (32 tests), `clippy -D warnings`,
`fmt --check`, `cargo deny check`, all `--all-features`. No `unsafe`, so no Miri
gate applies. Property suites (≥256 cases each): router totality, catalog
partition, purity/determinism + permutation-invariance, glob-vs-reference (512),
config round-trip. Three added regression tests cover the codex P2 guards
(missing/unknown key rejected, over-capacity rejected, pathological glob fast).
Total test runtime well under the ~3-minute budget.
