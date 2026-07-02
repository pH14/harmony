# `dissonance/logtmpl` — implementation notes (task 67)

The log-template scrape sensor (Drain-style clustering + an internal codebook),
the `Matchable` adapter to the matcher DSL, and **CellFn v1**. Everything lives
behind the task-64 spine: this crate *imports* spine items and *implements*
`Sensor` / `CellFn` / `Matchable`; it edits nothing in `dissonance/explorer`.

## Module map

- `token.rs` — whitespace tokenize + digit pre-masking (the `<*>` knob).
- `cluster.rs` — `Codebook`: the deterministic Drain fold (fixed-depth tree →
  leaf → integer-similarity threshold → merge/mint), serialize/reload.
- `sensor.rs` — `LogSensor` (`impl Sensor`) + `LogSensor::adapt` (the record view),
  and `TEMPLATE_CHANNEL`.
- `record.rs` — `TemplateRecord` (`impl Matchable`): `kind`/`msg`/`template`/`param.N`/`moment`.
- `cell.rs` — `CellFnV1` (`impl CellFn`), `CellConfig`, `Quant`, and the
  injective `encode_cell_key`/`decode_cell_key`.
- `loader.rs` — `load_console_log`: fixture text → scrape-tier records (test
  scaffolding, **not** a decoder — that is task 65).

## Load-bearing design decisions

### The point-in-time slice contract for CellFn v1

`CellFn::key(at, feats)` receives only a `FeatureSet`. CellFn v1 is a **pure
function of that slice** and is moment-blind (it ignores `at`, exactly like the
spine's `IdentityCells`) — identical slices key identically, which is what makes
the distinct-cell count bounded rather than per-moment (gate 5). It reads three
channels off the slice:

1. **species-progress** = quantized count of features on the template channel;
2. **last-new-species** = the **max** template-channel `FeatureId`, folded `mod k`.
   Template ids are minted in first-seen order, so *the largest id present is the
   most recently first-seen species* — no ordering state is needed;
3. **each state channel** = the value on that channel, folded `mod k`.

The slice is therefore expected to be the point-in-time *behavioral* view: the
template channel **accumulates** (all species seen ≤ `at`, so channels 1–2 read
count and max id) while each state channel carries its **current** value (channel
3). The gate-5 harness and any future state-tracking `Archive` build the slice
that way; `Codebook::ingest` + a cumulative `FeatureSet` is all it takes for the
template channels (see `tests/common::timeline_cell_keys`).

**Deviation considered and rejected:** making `CellFnV1` *stateful* — owning the
run's ordered feature timeline and answering `key(at, …)` from precomputed
per-moment state (which would let it recover "latest-in-time" for a state channel
whose ids are content hashes, per task 66's `cell` role). Rejected: it breaks the
"pure per slice" spirit of the `CellFn` trait, couples one `CellFn` instance to
one run, and is unnecessary for the template channels (ids already encode order).
The cost is that a state channel's "latest value" must be supplied *in the slice*
by the driver (keep-latest-per-channel), not recovered by the `CellFn`; for a
cumulative slice, CellFn v1 folds the deterministic max-id representative — still
bounded and deterministic, just not time-latest. This is documented on
`cell.rs`'s "point-in-time slice contract".

### The codebook is a campaign fold over the run *sequence*

The spec is explicit: the codebook is "a stateful fold over the run sequence, not
just one run", and template ids are minted in first-seen order so the same
species keeps the same `FeatureId` across runs (a `Feature` carries only
`(channel, id)` — reminting per run would conflate distinct species downstream).
`LogSensor` therefore holds its `Codebook` as **campaign state** and
`observe`/`adapt` fold each trace into it. Because `Sensor::observe(&self, …)` is
immutable but the fold must persist, the codebook lives behind a `RefCell`;
`Box<dyn Sensor>` carries no `Send`/`Sync` bound, so this is sound (the campaign
drives one sensor sequentially). Re-folding a trace the codebook already absorbed
is idempotent (every line re-matches its existing template), so the spine's "same
trace, same stream" purity contract still holds, while genuinely new traces
extend the codebook. `adapt` shares the same codebook, so matcher ids and sensor
ids always agree. Persistence ("serialize → reload → continue is
indistinguishable") is `LogSensor::codebook()` (snapshot) + `with_codebook()`
(resume) on top of `Codebook::to_json`/`from_json`; the two-trace stability test
(`ids_are_stable_across_the_run_sequence`) and `snapshot_and_resume_continue_the_fold`
pin both. (Gate 2's "fresh codebook each derivation" tests the `Codebook`
primitive directly, which is the right unit for that determinism claim.)

**Deviation considered and rejected (round 2):** the original submission folded a
`Codebook::new()` per `observe`, so ids were stable only *within* one run — a
spec violation the reviewer flagged. The fix threads campaign state as above.

### Codebook internality (the EXPLORATION ruling)

Only stable `FeatureId`s cross the boundary (via `Feature` and the
`TemplateRecord::template` attribute). The `Codebook` type is public but is this
crate's own — it never appears in a spine signature, so the spine and explorer
cannot couple to template text, tree structure, or thresholds. `template_text()`
exists only for inspection/tests and returns a `String`, never a signal.

### Parameter extraction reads the *raw* tokens

Clustering compares the **masked** stream, but `param.N` is pulled from the
**raw** tokens at the template's wildcard positions — so a masked value survives
verbatim (`IPv4`, `0.0.0.0`, `5432` in the gate-6 line all extract intact even
though each was masked for clustering because it carries a digit).

### Determinism specifics

`BTreeMap`/`Vec` only (no `HashMap`/`HashSet` anywhere near the encoder — clippy's
determinism lints pass with zero `#[allow]`s). The parse tree is a
`BTreeMap<LeafKey, Vec<u64>>` serialized as an ordered pair sequence (JSON has no
struct-keyed maps; the same trick the spine `Frontier` uses for its cell index).
The similarity threshold is an integer cross-multiply widened to `u128`, so no
line length can overflow it (a debug-build overflow would be a panic on untrusted
input). Library code never `unwrap`s on untrusted input; the two decode paths are
hardened against adversarial bytes:

- `Codebook::from_json` returns a typed error for an unknown version **and** for a
  parse tree that references an out-of-range template id (`Error::DanglingTemplate`)
  — otherwise the next `ingest` would index `self.templates[id]` out of bounds and
  panic. Fuzzed by `corrupting_tree_ids_never_yields_a_panicking_codebook`.
- `decode_cell_key` bounds its `Vec::with_capacity` by the actual buffer size, so a
  forged 4-byte field count (up to `u32::MAX`) cannot drive a huge speculative
  allocation. Fuzzed by `decode_cell_key_is_total_on_arbitrary_bytes`.

(Both were round-1 review P1s.)

## Fixtures

No task-65 fixture drops existed at implementation time, so both are
**synthesized** (a seeded generator; only the committed `.log` output ships).
`k3s-console.log` is 5,200 lines (≥ 5,000, ~414 KB — well under 2 MB) across
kubelet / containerd / flanneld / etcd / apiserver / scheduler / proxy / coredns /
runc / … with realistic parameter churn (pids, IPs, durations, UUIDs, ports,
image refs, pod names, LSNs). `postgres-console.log` is 700 lines of
startup/WAL/checkpoint/autovacuum/connection traffic, with a fixed 4-line startup
preamble the gate-6 adapter test keys on.

**Format choice:** each line leads with a literal `component event …` pair. A
faithful klog/journald prefix (`I0702 14:23:01.1 1234 file.go:99]`) masks its
first several tokens to `<*>`, which with depth 2 and τ = ½ forces ~50 % spurious
similarity and *over-merges* unrelated messages. Leading with literal
`component`+`event` tokens gives the fixed-depth tree clean, meaningful leaves —
this is the realistic axis the spec cares about ("realistic parameter churn"),
and it makes the species count predictable.

**Cardinality (gate 5):** with default knobs the k3s timeline yields **78**
distinct cell keys from 78 species — comfortably inside `[32, 1024]`. Because
`last-new-species` folds the (unique, monotonically-minted) id `mod 64` and
`species-progress` log2-buckets the count, each new species produces a distinct
`(bucket, id mod 64)` pair for species counts up to a few hundred, so distinct
keys ≈ species count over that range. The bound holds with wide margin on both
ends.

## Gate status (all green, macOS; portable to Linux)

1. Standard suite — build / nextest (53 tests) / clippy `-D warnings` / fmt / deny,
   all-features. No `unsafe` ⇒ no Miri gate. A frozen public-API snapshot guard
   (`tests/public_api.rs` + `tests/public-api.txt`, the repo's standard
   `cargo public-api` pattern) is added and the crate is registered in the
   `public-api` CI job's package list (`.github/workflows/quality.yml`).
2. Stable species set — identical species, ids, and byte-identical codebooks over
   both fixtures (`gate2_*`).
3. Codebook reload — mid-fixture serialize→reload→finish matches the uninterrupted
   run at every split (`gate3_*`, and the round-trip proptest at *every* split).
4. Proptests (256 cases each) — totality on arbitrary bytes, masked-only
   differences share a template, codebook round-trip, and CellKey encoding
   injective + stable.
5. Cardinality — 78 ∈ `[32, 1024]` on the full k3s timeline (`gate5_*`).
6. Adapter — the documented `kind`/`msg`/`template`/`param.N`/`moment` values on
   known fixture lines (`gate6_*`).

## Portability & scope

Pure std collections + serde; no `#[cfg(target_os)]`, no platform syscalls, no
`unsafe`. The one sibling dependency is `dissonance/explorer` (the spine); the
matcher's `cell`-role channels reach CellFn v1 **only through the spine** (as
configured `ChannelId`s in `CellConfig::cell_channels`) — there is no crate
dependency on `dissonance/matcher`.

## For the integrator

- `Cargo.lock` is left uncommitted: the delta is purely the additive `logtmpl`
  entry, and task 64 (which added `explorer`) likewise did not commit its lock
  delta. Regenerate on merge.
- `CellConfig::cell_channels` is empty by default. A campaign that wants pod-phase
  / recovery-state harvested (SGFuzz's reified state) wires the matcher
  `cell`-role `ChannelId`s in here, and the driving archive must present each such
  channel's **current** value in the per-moment slice (see the slice contract).
- `TEMPLATE_CHANNEL` is `ChannelId(1)` (coverage is `0` in the explorer defaults);
  override via `LogSensor::with_channel` / `CellConfig::template_channel` if a
  campaign renumbers channels.
