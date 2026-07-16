# `sdk-events` — implementation notes (`hm-bbx.1`)

The host-side **SDK ingress boundary**: it decodes both LAYERS R-L3 ingress
formats into one normalized, persisted observation contract — `SdkSchema` (the
declarations) plus ordered `SdkEvent`s (`Normalized`). It **decodes and
normalizes; it does not judge**, reduce temporal state, assign cells, or run
archive policy. Implements `docs/DISSONANCE-STRATEGY.md` §"Cooperative
observation" and `docs/LAYERS.md` §R-L3. First implementation child of the
Differential epic (`hm-bbx`).

## What it adds (the normalized boundary)

- **`decode_antithesis`** (`src/antithesis.rs`) — the app-facing **Antithesis SDK
  JSON** over `/dev/harmony`. `antithesis_assert` → occurrence/property evidence
  with the aggregated property as the identity and the `location` preserved as a
  separate `SiteId` (provenance/coverage, never a property verdict).
  `antithesis_guidance` (numeric) → a **monotone extremum only** (`maximize` →
  `Max`/`Min`, never `set`), the metric kept as its original token, report-only.
  `antithesis_setup` → a lifecycle occurrence.
- **`decode_binary`** (`src/binary.rs`) — the internal binary Event wire. **v1**
  preserves point identity and each fired operation but declares no reducer: a
  declared-but-never-fired state point is reportable coverage with `base_op =
  None` (unresolved), and the decoder never promotes a v1 firing into a declared
  reducer. **wire-v2** (`encode_v2_declaration` / `DeclaredPoint`, format in
  `src/wire.rs`) carries occurrence/state classification, value shape, and base
  update operation, so a v2 state point is reducible before it ever fires. The
  firing codec honors **all four** operations (`set`/`max`/`min`/`accumulate`),
  and the value the binary path carries is the cooperative vertical's bounded
  integer (`u64`). A v2 declaration is accepted only if the emission path can
  report it: a state point declares a base op + a `u64` shape, an occurrence
  declares neither, and every local id fits the 24-bit runtime field — otherwise
  it is a typed error, on both encode and decode, so schema and event evidence can
  never disagree. A catalog naming an unsupported wire version is refused
  (`UnsupportedVersion`), never decoded under a guessed layout.
- **`SdkSchema` / `SchemaEntry` / `SdkEvent`** (`src/schema.rs`, `src/event.rs`) —
  the normalized model: source provenance, observation identity, value, and
  classification are kept as separate roles (cell projection is *not* owned here).
  Canonical serde (sorted, unique entries; no `HashMap` order; no float), so
  output is byte-identical on macOS/Linux. `original_declaration` and per-event
  `raw` keep the source bytes recoverable for audit/migration.
- **`NumericToken` / `BoundedNumeric`** (`src/numeric.rs`) — a guidance number
  enters as its exact original token and stays report-only until it validates into
  a bounded exact sign/coefficient/base-10-scale decimal with a deterministic
  total order. **No `f64` touches a value anywhere**; non-finite / over-precise /
  out-of-range input fails validation and remains report-only evidence.
- **`SdkError`** (`src/error.rs`) — typed, panic-free. Structural contradictions
  are errors (mixed operations, incompatible shapes, classification conflict,
  malformed declaration lengths, unknown declaration bytes); unrecognized data is
  never an error — it is preserved raw.

## Key decisions

- **The declaration is strict; the event stream is total.** A malformed *length*
  in a declaration (a record that overruns) is a typed `MalformedLength`; a
  garbled or unrecognized *event* (unknown namespace, unparseable JSON frame,
  unknown wrapper) becomes a `Payload::Unknown` carrying its raw bytes — nothing
  panics and nothing is dropped. This is the clean split behind "malformed lengths
  return typed errors" *and* "unknown bytes survive round-trip".
- **Coherence, not inference.** A second sighting of an identity must agree with
  the first (`merge_entry`): a differing op/shape/classification is a typed error.
  An unresolved slot is *refined* (`None` → `Some`) by a later resolved sighting,
  but a resolved value is never silently overwritten. v1 firings are checked for
  self-consistency but never resolve the schema's `base_op`.
- **`accumulate` is declaration-only.** Only a versioned source (wire v2) may
  declare `Accumulate`; v1 cannot claim an operation it cannot encode.
- **The declaration is schema, not an event.** Its stream slot is skipped from the
  event vector, but ordinals stay equal to persisted vector position (the
  rollout-local source ordinal — the contractual `OrderingScope`), so a boundary
  event is neither duplicated nor renumbered.
- **`arbitrary_precision` serde_json** is the mechanism that keeps every JSON
  number as its exact token without ever constructing an `f64`.
- **Accept only what the emission path can report** (spec rule, applied in the
  codec). `encode_v2_declaration` is fallible and validates each point with the
  same `validate_v2_point` the decoder uses, so an un-fireable id or a shape the
  binary wire cannot carry fails at construction, not silently downstream.
- **A recognized JSON record carries exactly one wrapper.** A frame with more than
  one Antithesis wrapper is ambiguous and preserved raw, never resolved to one
  branch with the rest dropped.
- **Deserialization re-verifies invariants.** `SdkSchema` deserializes through a
  `try_from` guard that rejects unsorted or duplicate entries, so `entry`'s binary
  search can never be silently defeated by a corrupted persisted schema.

## Deviations considered and rejected

- **Removing the legacy link-tier surface** (`decode_events`/`Catalog`,
  `LinkSensor`, `AlwaysViolation`, `LINK_*_CHANNEL`, packed `FeatureId`).
  `docs/DISSONANCE-STRATEGY.md` explicitly rules these "compatibility machinery to
  **delete during the Differential integration**" (`hm-bbx.4`), not to rename or
  remove here; `campaign-runner`'s game path still consumes `LinkSensor` /
  `decode_events` / `KIND_STATE` / `LINK_STATE_CHANNEL`. Rejected: kept the
  compat surface unchanged and made this task purely **additive**. "Keep assertion
  judgment out of the decoder crate" is honored by the *new* boundary carrying no
  Oracle/Sensor/policy — assertion events are available for Explorer judgment as
  plain evidence.
- **Computing the never-fired / never-satisfied absence claim.** Rejected: the
  boundary only *preserves* property `Expectation`s (`must_hit`, `unreachable`);
  the derived absence claim is a separate finalized view (reporting owns it), per
  the strategy. Likewise `must_hit`/site data is persisted, not evaluated.
- **Deriving a numeric guidance margin from an operand-pair `guidance_data`.**
  Rejected for now: operand pairs stay report-only with operands preserved in
  `raw`; only a scalar metric is normalized into a token. Exact decimal
  subtraction can promote it later without changing the persisted form.

## Known limitations / integrator notes

- **Open issue `hm-ynt`** (SDK event `Moment`s are V-time-anchor lower bounds, not
  emission `Moment`s) is neither fixed nor worsened: `SdkEvent::moment` carries the
  anchor through verbatim, documented as a lower bound.
- **Cross-source sequencing** is out of scope: `OrderingScope` is
  `RolloutLocalSourceOrdinal` (same-source order only). A shared machine-event
  ordinal — needed for cross-source predicates — is a downstream concern.
- **The wire-v2 encoder is host-side.** The canonical guest-side v2 encoder is a
  future `guest/sdk` deliverable (out of this task's surface); `wire.rs` and the
  `tests/*` goldens pin the host format so a later guest encoder must agree.
- **Downstream (`hm-bbx.4`)** consumes `Normalized` to build Differential
  relations, reducers, cells, and archive occupancy. Temporal reduction of `set` /
  `max` / `min` / `accumulate`, historical derivations, and `CellFn` all live
  there, not here.

## Gates (all green, Mac-local)

- `cargo build/nextest/clippy -D warnings/fmt/deny -p sdk-events` — 70 tests
  (goldens for Antithesis assertions, max/min guidance, binary v1, wire v2; typed
  errors; totality; numeric total-order laws; serde + wire-v2 round-trips) plus
  the ≥256/512-case proptests.
- `tests/public-api.txt` refreshed deliberately (additive; the compat surface is
  unchanged).
