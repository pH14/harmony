# environment — implementation notes

The `decide` seam, the versioned fault catalog, the seeded backing, and the
recorded-replay reproducer (task 24) — the heart of dissonance's fault model.
Pure logic: no `/dev/kvm`, no guest, no real I/O, no wall-clock, no host entropy,
no sibling-crate dependencies. Builds and passes every gate on macOS and Linux.
No `unsafe`, so no Miri obligation.

## What was built

The public types exactly as the spec's Public API lists them: the catalog
(`DecisionClass`, `DecisionPoint`+`class`, `Answer`+`encode`/`decode`, `Fault`,
`CorruptSpec`, `BlockOp`, `Outcome`), the seam (`Environment::decide`), the
newtypes (`NodeId`, `ConnId`, `VTime`, `DecisionId`), the constants
(`CATALOG_VERSION`, `MAX_SUPPLY_LEN`), `FaultPolicy`
(`none`/`from_bytes`/`to_bytes`), `SeededEnv`, and the reproducer (`EnvSpec`
with `BLOB_VERSION`/`encode`/`decode`/`materialize`, `RecordedEnv`,
`StandingFault`, `EnvError`).

### Module layout

`catalog.rs` (the shared vocabulary + `Answer`/`DecisionPoint` methods) ·
`prng.rs` (the local xorshift64\* generator) · `policy.rs` (`FaultPolicy` +
per-class sampling) · `seeded.rs` (`SeededEnv`, two independent streams) ·
`recorded.rs` (`EnvSpec`, `RecordedEnv`, `StandingFault`) · `codec.rs` (the
shared little-endian writer + a forward-only bounds-checked `Reader`, and the
`Answer`/`Fault` byte forms) · `error.rs` (`EnvError`).

### Additions (allowed by conventions rule 3)

- `FaultPolicy::set_class(class, num, den, &[Fault])` — the only ergonomic way to
  build a non-baseline policy without hand-rolling `from_bytes` bytes; the
  explorer (task 12) needs it. Validates and canonicalizes (rejects supply
  classes, `den == 0`, foreign-class faults; sorts+dedups the eligible list).
- `DecisionPoint::admits(&Answer) -> bool` — the **single source of truth** for
  admissibility. `RecordedEnv` consults it to decide whether an override wins,
  and the spec says the reactive backend applies the same check to a decoded
  resolve answer ("checked as `RecordedEnv` does for overrides"); exposing it
  means the frontier reuses it rather than re-implementing a determinism-critical
  check that could drift.
- `DecisionClass::is_supply`/`is_fault` — convenience predicates services/the
  explorer want.

The frozen public surface is in `tests/public-api.txt`.

## Key design decisions

- **Determinism by construction.** The override map is a `BTreeMap<u64, Answer>`;
  the eligible-fault lists, the override vector, and the standing-fault vector are
  all canonicalized (sorted/deduplicated) before any byte is written, so equal
  values always encode identically and no iteration order reaches an answer or a
  byte. No floats, no `HashMap`/`HashSet`, no wall-clock, no unseeded RNG. The
  determinism clippy lints (`clippy.toml`) are inherited via `[lints] workspace`.

- **One PRNG algorithm, two independent streams.** The generator is exactly
  `hypercall-proto`'s xorshift64\* (multiplier `0x2545F4914F6CDD1D`, zero-seed
  fallback `0x9E3779B97F4A7C15`), re-implemented locally (rule 2). `SeededEnv`
  derives **two** streams from the seed: a *supply* stream (entropy / payload /
  scheduler) and a *fault* stream (`seed ^ 0xD1B5…` domain separation). This
  honors the spec's "fault sampling is independent of the guest entropy stream":
  tuning the `FaultPolicy` cannot shift the entropy a guest pulls, and vice
  versa.

- **Uniform stream advancement.** A fault-class decision draws **exactly one**
  fault-stream word regardless of outcome; that single word decides both the
  fixed-point Bernoulli trial (`w % den < num`) and, on a fault, the eligible
  index (`(w / den) % len`). `den ≥ 1` is a constructor invariant, so the modulo
  never divides by zero. An entropy/payload decision draws `ceil(bytes/8)` supply
  words; a scheduler decision draws one. Uniform, outcome-independent advancement
  keeps the streams easy to reason about and golden-stable.

- **Overrides cost no PRNG (the faithful reproducer).** `RecordedEnv` answers
  from an admissible override *without* drawing from the base — the base stream
  advances only when it actually answers (an absent or inadmissible override).
  This mirrors a real reactive session, where a surfaced decision is answered by
  the explorer and the seed is never consulted for it, so a `RecordedEnv`
  materialized from an `EnvSpec::Recorded` reproduces the session bit-for-bit.
  The global decision counter still advances by exactly one per `decide`.

- **Admissibility is the untrusted-bytes guard, not a clamp.** An override whose
  answer is inadmissible for its decision (wrong class, or out of the point's
  bounds — a `Supply` length ≠ the request, a 3-byte or out-of-range `Scheduler`
  selection, a `BlockTorn(n)` with `n > len`) is deterministically **ignored**
  (the base answers), never clamped, exactly as the spec dictates. By contrast a
  policy-sampled fault is emitted verbatim — `SeededEnv` may emit
  `BlockTorn(n > len)` for a particular point, and the **block service** clamps
  it, the same division of labor as pv-net clamping a corrupt offset modulo the
  frame length. The admissibility check is the single `DecisionPoint::admits`,
  cross-checked in tests against an independent restatement of the rule.

- **Strict, total codecs.** `Answer`/`FaultPolicy`/`EnvSpec` decode is strict and
  total: bad magic, truncation, trailing bytes, a non-canonical
  (unsorted/duplicate) section, an unknown tag, or `den == 0` all yield
  `EnvError::Malformed`; an off-version blob (correct magic, wrong version field)
  yields `EnvError::BadVersion`. The `Reader` bounds-checks every length against
  the actual buffer before slicing, so a hostile length can neither over-read nor
  force an unbounded allocation (a `Supply` longer than `MAX_SUPPLY_LEN` is also
  rejected). Decode commits no partial state.

## Deviations considered and rejected

- **A per-`Answer` version byte.** Rejected. `Answer::encode` is on the hot
  control-plane path (one per resolved decision) and is carried inside a
  control-proto frame that already version-negotiates at `hello()`. Its tag bytes
  are stable discriminants governed by `CATALOG_VERSION` (the catalog is the unit
  that versions), so a redundant per-answer version would only add overhead. The
  reproducer blob that *is* stored long-term (`EnvSpec`) and the policy blob both
  carry their own magic + version.

- **A single fused PRNG stream.** Rejected in favor of the supply/fault split, to
  satisfy the independence requirement above.

- **Clamping inadmissible overrides instead of ignoring them.** Rejected — the
  spec is explicit that an inadmissible override is ignored and the seeded base
  answers; a clamp would silently fabricate an answer the recorder never emitted.

- **Storing `StandingFault`s inside `RecordedEnv`.** Rejected: standing faults
  are applied imperatively by the frontier (e.g. pv-net `set_partition`), never
  through `decide`. They live in the `EnvSpec` (public fields), so the frontier
  reads them off the spec before/around `materialize`; threading them through the
  decide-backing would imply `decide` enforces them, which it must not.

## Known limitations / integrator notes

- **`StandingFault::target` is opaque here.** This crate only carries and
  canonically orders it; the owning service interprets it (e.g. pv-net decodes a
  `(NodeId, NodeId)` link). Branch/replay re-applies each entry via the service's
  standing-fault API; arming one out-of-band would escape replay.

- **`SeededEnv` emits policy faults verbatim.** Per-point clamping of a fault
  parameter (notably `BlockTorn(n)` vs the I/O length) is the consuming service's
  job, mirroring pv-net. `RecordedEnv` overrides *are* bounds-checked because they
  are untrusted reproducer bytes that must reproduce one recorded answer exactly.

- **Reactive backing is out of scope (frontier, vmm-core).** `Outcome::NeedsHost`
  exists so the seam is stable, but both backings here are pure and always return
  `Resolved`. The socket server, the suspend/`run(resolve)` loop, and binding
  `Environment` to the services live in vmm-core.

- **CI wiring left to the integrator (root files are off-limits, rule 1):** add
  `environment` to the `public-api` job's `-p` list in
  `.github/workflows/quality.yml` (the `tests/public_api.rs` guard +
  `tests/public-api.txt` snapshot are in place and pass on the pinned nightly;
  the test skips cleanly when the tooling is absent). No `miri` entry is needed
  (no `unsafe`). `Cargo.lock` is regenerated by the integrator — this branch does
  not commit it (matching the existing dissonance crates).

## Gates

`cargo build/nextest/clippy(-D warnings)/fmt -p environment --all-features` and
`cargo deny check` all pass: 39 tests (replay determinism ≥256-case property;
override semantics property cross-checked against an independent rule + targeted
inadmissibility cases; per-class golden answer sequence; codec round-trip +
never-panic-on-arbitrary-bytes + off-version `BadVersion`; `FaultPolicy`
byte-determinism; no-order-leakage permutation tests) plus the ignored,
nightly-only public-api guard. Suite runtime ≈ 0.5 s. As with the other
proptest-using crates, the clippy run surfaces the pre-existing workspace
`clippy.toml` meta-diagnostics about the unresolvable `rand::*` disallowed-method
paths (proptest pulls `rand` into the dev graph); they cite no crate code and do
not fail `-D warnings`.
