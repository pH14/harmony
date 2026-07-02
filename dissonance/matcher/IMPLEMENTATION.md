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

- **Per-signal channel = `channel_base + rank`, rank = position in the sorted
  name set.** The router table says "channel = the signal", and gate 4 requires
  that permuting a signal's declaration position never change another signal's
  output. A *declaration-index* channel fails that (moving one signal renumbers
  the others); a *name-hash-into-u16* channel collides too often to keep totality
  clean. Sorted-name rank is both permutation-invariant (it ignores declaration
  order) and collision-free (names are unique). `never` signals occupy a rank
  too but emit no feature, so cross-role leakage is impossible by construction.
- **Channel allocation is the campaign's, not hardcoded (round-2 P1 + foreman
  ruling).** `MatchSensor::new` takes an explicit `channel_base: ChannelId`.
  **Channel 0 is coverage's** by convention (spine `COVERAGE_CHANNEL`); a matcher
  `Feature{channel:0, id:1}` would otherwise be indistinguishable from a coverage
  edge and the archive would dedup them. So the base is validated `>= 1`
  (`ReservedChannelBase`) and `base + signal_count <= u16::MAX`
  (`ChannelSpaceExhausted` — the round-1 capacity guard folded in here, where the
  base is known). The sensor occupies `[base, base + count)`; `channel_base()` /
  `next_free_channel()` expose the range so a later plugin (task 74 OTel) bases
  above it. The capacity check therefore lives at the sensor (which assigns
  channels), *not* at `SignalSet` (whose oracle-only `never` roles have no
  channel and no such limit).
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
  negative `attr_max` value on a matched record is a **counted decode miss**
  (`decode_misses()`), never a panic and never a feature; it does not fail the
  match (`attr_max` is an extraction, not a predicate). A `state_max` role that
  declares **no `attr_max` at all** is a different thing — a vacuous config that
  matches, emits nothing, yet reports fired — and is **rejected at validation**
  (`StateMaxWithoutAttrMax`, round-2 P2 #3).
- **JSON, not YAML** (the task ruling): the whitelist stays untouched. Parsing
  is total on untrusted input — every malformed class (`UnknownRole`,
  `DuplicateName`, `StateMaxWithoutAttrMax`, bad type via `Parse`,
  `UnknownDuring`) is a typed error. **Fail-closed** (round-1 P2 #1): the
  top-level `signals` key is *required* (a config missing it is an error, not a
  silent empty set — an explicit `"signals": []` stays legal) and
  `deny_unknown_fields` at every level rejects a misspelled key rather than
  silently ignoring it (which could vacate the set or broaden a match).
- **Content-based determinism — no emission-order leak** (round-3 P1). Output
  must be a pure function of record *content*, but `Matchable` exposes only
  `kind()` / `attr(k)` / `moment()` — no attribute *enumeration*, so a record has
  no full "canonical bytes" a generic sort could key on. Rather than a
  `(Moment, index)` tie-break (which would degenerate to the source's emission
  order for same-Moment records), each consumer keys on the content it actually
  uses: the sensor sorts its whole stream by `(Moment, channel, id)`;
  `state_max` folds per-Moment maxima through a `BTreeMap<Moment, u64>` (`max` is
  order-independent, so intra-Moment order cannot matter — and it emits one
  bucket per Moment, never an intermediate bucket from a mid-Moment partial
  fold); the oracle orders verdicts by `(Moment, fingerprint)` and `judge()`
  returns the min. There is no `HashMap` iteration, no floats, no seed — the
  same trace in *any* record order yields byte-identical output (the shuffle
  proptest pins this).

## Provisional / superseded elsewhere

- The `never` `Bug` fingerprint `sha2(name ‖ kind ‖ matched attr bytes)` is
  **provisional** per the spec: task 75 pins the authoritative stable-coordinate
  `Bug` fingerprint schema and supersedes this minting site.

## What the integrator must know

- **Catalog fired-marks are assembled by the caller.** A campaign unions
  `sensor.fired(t)` (the `sometimes`/`cell`/`state_max` matches) with
  `oracle.fired(t)` (the `never` matches) and passes the union to
  `Catalog::report(&fired)`. The two fired sets are disjoint by role.
- **Channel allocation is the caller's.** Pass `MatchSensor::new` a
  `channel_base >= 1` (0 is coverage's); the sensor occupies `[base, base +
  signal_count)`. When composing with another channel plugin, base it at
  `sensor.next_free_channel()` so the `Feature` spaces never overlap. Adding a
  signal renumbers ranks within the range — a config change, not an in-run event.
- **Stubs only.** `ChannelSource`/`ContextSource` are the seams; the concrete
  adapters (log records task 67, SDK/link events task 73, OTel spans task 74) and
  the production schema-aware fault index (task 69) are later tasks. `stub.rs`
  ships example/test implementations — `OwnedRecords` demonstrates the
  "records reassembled outside the trace verbatim" case the seam exists for.

- **Glob is genuinely linear** (round-2 P2 #2). Record bytes are guest-emitted
  (adversary-influenced in a fuzzer), so the round-1 two-pointer's
  `O(pattern · text)` worst case was a real replay-plane DoS. `glob.rs` is now
  **segment matching**: split at `*`, anchor head/tail literally, locate interior
  segments greedily with a hand-rolled **KMP** substring search (`find`) —
  `O(pattern + text)`, no adversarial blowup, and total on non-UTF-8 bytes (KMP
  over `[u8]` rather than `str::find`, which needs UTF-8). Verified against the
  naive recursive reference on 512+ proptest pairs;
  `segment_matching_stays_linear_on_pathological_input` runs megabyte inputs that
  would grind a quadratic matcher.

## Known limitations

- `during:` ships exactly one predicate, `no_faults`; the `During` enum is
  `#[non_exhaustive]` so the vocabulary extends without a spine change.
- A `MatchSensor` cannot address more than `u16::MAX - base + 1` signals
  (`ChannelSpaceExhausted`); the oracle-only `never` roles have no such limit.

## Gates

All green on macOS (dev) — `build`, `nextest` (37 tests), `clippy -D warnings`,
`fmt --check`, `cargo deny check`, all `--all-features`, plus the public-API
snapshot test (`tests/public_api.rs` + committed `tests/public-api.txt`, run in
the `public-api` CI job on the pinned nightly — the crate is now in that job's
package list). No `unsafe`, so no Miri gate applies. Property suites (≥256 cases
each): router totality, catalog partition, purity/determinism (declaration-
permutation, emission-reversal, and a **shuffle** proptest asserting identical
`FeatureSet` + `judge()`), glob-vs-reference (512), config round-trip. Regression
tests cover every review finding: missing/unknown key rejected, channel-base /
coverage-collision + space-exhaustion rejected, `state_max`-without-`attr_max`
rejected, same-Moment emission-order invariance, and megabyte pathological globs
staying linear. Total test runtime well under the ~3-minute budget.
