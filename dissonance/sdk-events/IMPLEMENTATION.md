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
  JSON** over `/dev/harmony`. `antithesis_assert` → occurrence/property evidence.
  The **message** is the property identity (`DISSONANCE-STRATEGY`: "the assertion
  message identifies the property and multiple sites may contribute to it"), so
  records from different sites — with different per-site `id`s — aggregate into one
  property; the `id` and `location` are preserved as the assertion `SiteId`
  (provenance/coverage, never a property verdict).
  `antithesis_guidance` (numeric) → a **monotone extremum only** (`maximize` →
  `Max`/`Min`, never `set`), the metric kept as its original token, report-only.
  `antithesis_setup` → a lifecycle occurrence (a **string** `status`, or its absence
  → `complete`); a present-but-non-string `status` is malformed and stays **raw**
  rather than fabricating a lifecycle point, mirroring `site_of` (bead `hm-jyj`).
- **`decode_binary`** (`src/binary.rs`) — the internal binary Event wire. **v1**
  preserves point identity and each fired operation but declares no reducer: a
  declared-but-never-fired state point is reportable coverage with `base_op =
  None` (unresolved), and the decoder never promotes a v1 firing into a declared
  reducer. **wire-v2** (`encode_v2_declaration` / `DeclaredPoint`, format in
  `src/wire.rs`) carries occurrence/state classification, value shape, and base
  update operation, so a v2 state point is reducible before it ever fires. The
  firing codec honors **all four** operations (`set`/`max`/`min`/`accumulate`),
  and the value the binary path carries is the cooperative vertical's bounded
  integer (`u64`). `min`/`accumulate` are wire-v2 firing extensions, so under a v1
  (or declaration-less) stream those op bytes stay raw rather than fabricating a
  state update. The v2 declaration encoder is **canonical** — points serialize
  sorted by `(namespace, local)`, so a caller's incidental order (e.g. from a
  `HashMap`) never reaches the persisted declaration bytes. A v2 declaration is accepted (identically on encode and decode,
  so schema and event evidence can never disagree) only if the emission path can
  report it: its classification matches the one the namespace's firings decode to
  (`NS_STATE`⇒state, `NS_ASSERT`/`NS_BUGGIFY`/`NS_LIFECYCLE`⇒occurrence); a state
  point declares a base op + a `u64` shape and an occurrence declares neither;
  every local id fits the 24-bit runtime field; a lifecycle declaration sits at the
  only decodable local (`setup_complete`, local 0); no coordinate is declared twice
  (in **v1 or v2** — a firing cannot distinguish two kinds at one coordinate); and
  no name overflows its `u16` length prefix — each otherwise a typed error.
  **v1** declares neither value shape nor base op (both `None` — unresolved, never
  invented), and a v1 `always` point carries **no** expectation (this wire emits
  only violations, so a passing `always` produces no event and must not read as an
  unsatisfied must-hit). A catalog naming an unsupported wire version is refused
  (`UnsupportedVersion`), and a stream carrying more than one catalog declaration
  is refused (`MultipleDeclarations`) — neither is decoded under a guessed layout.
  The declaration must **precede every firing** (`DeclarationAfterFirings`), so a
  later format claim can never retroactively reassign semantics to prior bytes, and
  a catalog blob must end exactly at its declared record count
  (`TrailingDeclarationBytes`), so it cannot silently omit declared identities.
- **`SdkSchema` / `SchemaEntry` / `SdkEvent` / `Normalized`** (`src/schema.rs`,
  `src/event.rs`) — the normalized model: source provenance, observation identity,
  value, and classification are kept as separate roles (cell projection is *not*
  owned here). Canonical serde (sorted, unique entries; no `HashMap` order; no
  float), so output is byte-identical on macOS/Linux. `original_declaration` and
  per-event `raw` keep the source bytes recoverable for audit/migration. `Normalized`
  is the persisted artifact and the **only** publicly-deserializable type — `SdkEvent`
  and `SdkSchema` are serialize-only, loadable only inside a validated `Normalized`
  (see "the only public deserialization entry" below).
- **`NumericToken` / `BoundedNumeric`** (`src/numeric.rs`) — a guidance number
  enters as its exact original token and stays report-only until it validates into
  a bounded exact sign/coefficient/base-10-scale decimal with a deterministic
  total order. **No `f64` touches a value anywhere**; non-finite / over-precise /
  out-of-range input fails validation and remains report-only evidence.
- **`SdkError`** (`src/error.rs`) — typed, panic-free. Structural contradictions
  are errors (mixed operations, incompatible shapes, classification conflict,
  malformed declaration lengths, unknown declaration bytes, a malformed schema entry,
  a `DeclarationMismatch` between a persisted schema and its recorded declaration, and
  an `IncoherentEvent` whose payload disagrees with the schema); unrecognized data is
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
  self-consistency but never resolve the schema's `base_op`. The **same** coherence
  is re-checked when a `Normalized` is loaded from persisted input, so a decode and a
  deserialize are held to one contract (a `set` firing at a `max`-declared coordinate
  is `MixedOperations` either way).
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
- **A recognized JSON record carries exactly one wrapper, and structural
  ambiguity is preserved raw.** A `DupCheck` visitor walks the whole frame and
  rejects a duplicate key at **any** depth (`serde_json::Value` would silently keep
  the last of a repeated key — e.g. two `maximize` fields choosing `Min` — and this
  is robust under `arbitrary_precision`, where a number is a single-key map). A
  duplicate key, zero or more-than-one recognized wrapper, or a wrapper whose value
  is not a JSON object all become `Payload::Unknown` with raw bytes intact —
  malformed input can neither drop a member silently nor fabricate a
  `setup_complete` occurrence from field defaults. A valid `antithesis_setup`
  registers its fixed occurrence identity in `SdkSchema` (like assertions and
  guidance), so a setup event can be validated/materialized against the schema.
  Site coordinates (`begin_line`/`begin_column`) are `u64`, preserved exactly
  rather than truncated into a colliding site.
- **`Normalized` is the only public deserialization entry, and it re-validates the
  whole artifact on load.** Persisted input is never trusted more than a live decode:
  `SdkEvent` and `SdkSchema` carry **no** bare `Deserialize`; the only way to read
  one back is inside a `Normalized`, whose `#[serde(try_from)]` runs the same
  contract the decoders enforce. Concretely it (a) routes every entry through the one
  `SchemaEntry::validate` choke point (sorted + unique, and the full source-specific
  invariant family — id↔source, point-namespace↔classification, lifecycle⇔occurrence,
  **expectation legal only on an assertion point**, occurrence-inert, and the v1/v2/
  antithesis state shape/op rules); (b) re-parses `original_declaration` and requires
  it to reproduce exactly this schema's source and entries — a null, garbled, or
  wrong-source blob (or one present where the source mints none) is a typed
  `DeclarationMismatch`, since a binary firing adds no entry and the declaration
  fully determines them; and (c) checks each event **coheres** with the schema — its
  `source` agrees, ordinals strictly increase, a classified payload sits only at a
  coordinate whose classification matches, and a reducible op matches the declared
  base op and every earlier firing for its identity. A persisted `set` firing at a
  `max`-declared coordinate therefore fails load with the same `MixedOperations` the
  decoder raises live; other incoherence is a typed `IncoherentEvent`. Component
  value types (`SchemaEntry`, `Payload`, `Raw`, …) keep `Deserialize` — they have no
  independent load path, so they are only ever read back *within* a validated
  `Normalized`. (The `cargo public-api` snapshot runs at `-sss`, which omits
  auto-derived impls, so this removal is enforced by a compile-time bound in
  `tests/load_validation.rs`, not a snapshot line.)

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

- `cargo build/nextest/clippy -D warnings/fmt/deny -p sdk-events` — 140 tests
  (goldens for Antithesis assertions, max/min guidance, binary v1, wire v2; typed
  errors; totality; numeric total-order laws; serde + wire-v2 round-trips) plus
  the ≥256/512-case proptests. `tests/load_validation.rs` holds the artifact-level
  load probes — one per invariant family (F1a declaration provenance, F1b
  lifecycle⇔occurrence, F1c expectation legality, F1d event↔schema coherence), the
  setup status-fabrication guard (F2, `hm-jyj`), and a compile-time bound proving
  `Normalized` is the only publicly-deserializable type.
- **Scoped mutation testing** (`cargo mutants --in-diff`) on `schema.rs` / `event.rs`
  / `binary.rs` / `antithesis.rs`: 0 missed.
- `tests/public-api.txt` refreshed deliberately: two new error variants
  (`DeclarationMismatch`, `IncoherentEvent`). The `Deserialize` removal from
  `SdkEvent`/`SdkSchema` is not a snapshot line (the snapshot runs at `-sss`, which
  omits auto-derived impls) and is instead enforced by the compile-time bound above.
  The legacy compat surface is unchanged.
