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
`LogSensor` therefore holds its codebook as **campaign state** behind a `RefCell`
(`Sensor::observe(&self, …)` is immutable but the fold must persist; `Box<dyn
Sensor>` carries no `Send`/`Sync` bound, so this is sound — the campaign drives
one sensor sequentially).

**Read/write split (round 4):** `observe` is the **mutating** fold — it advances
the campaign codebook (ids stable across the run sequence). `adapt` is a
**read-only view**: it folds a *clone* of the current codebook, so adapting a
trace twice yields identical `TemplateRecord`s and never mutates the campaign
(the round-4 fix — the old shared mutating fold double-folded a re-adapted trace,
inflating species and drifting `param.N` between calls). Already-absorbed lines
keep their `observe` ids on the clone (the round-3 wildcard-covers-any scoring
guarantees the re-match); unseen lines get would-be assignments that are not
persisted. `adapt_is_a_read_only_view` pins "observe(t) then adapt(t) twice ⇒
identical records + unchanged codebook bytes"; re-folding is idempotent, so the
spine's "same trace, same stream" purity still holds for `observe`.

**Order-invariant params (round 5):** `adapt` folds the **whole trace first**,
then extracts each record's params against the *final* (post-fold) templates (via
`Codebook::params_for`). Otherwise `param.N` would depend on arrival order — an
early literal line assigned *before* its position generalized to `<*>` would show
no param pre-`observe` but a param post-`observe`, so a matcher-DSL signal over
`param.N` would depend on the sensor-list call order. Folding the whole trace
lands the view in the same state (`base ∪ t`) whether or not `t` was observed
(the round-3 wildcard scoring makes re-ingest id-stable), so `adapt(t)` is
order-invariant; `adapt_is_invariant_to_observe_order` pins `adapt(t) ==
observe(t)-then-adapt(t)` on a generalizing example.

Persistence ("serialize → reload → continue is indistinguishable") is exposed as
**opaque bytes** on the sensor — `LogSensor::codebook_bytes()` (snapshot) and
`LogSensor::with_codebook_bytes()` (resume) — never a codebook-shaped type;
`snapshot_and_resume_continue_the_fold` and the two-trace
`ids_are_stable_across_the_run_sequence` pin it.

**Deviation considered and rejected (round 2):** the original submission folded a
fresh codebook per `observe`, so ids were stable only *within* one run — a spec
violation the reviewer flagged. The fix threads campaign state as above.

### Codebook internality is enforced at the API boundary (round 4)

The internality ruling — "nothing codebook-shaped appears in any public signature
that the spine or another crate could couple to" — is now enforced by
visibility, not just convention: `Codebook`, `ClusterConfig`, `Assignment`,
`CODEBOOK_VERSION`, and the template-token vocabulary (`Token`, `WILDCARD`) are
`pub(crate)` and not re-exported. A campaign persists the fold only through the
opaque bytes above. The public surface is the sensor, the `Matchable` adapter,
CellFn v1 + its (mandated, public) `CellConfig` knobs, and the errors — the
frozen `tests/public-api.txt` snapshot is the guard. Consequently the
clustering-side gate-4 proptests (totality, masked-parameter merging, codebook
round-trip / reload transparency, `from_json` fuzz) live as **unit** tests in
`cluster.rs` (they touch the `pub(crate)` codebook); the CellFn encoding proptests
stay in `tests/`. `nextest` runs both, so gate 4 is covered end to end.

### Similarity scoring: constant positions only (foreman spec amendment, round 6)

Similarity scores the template's **constant (non-`<*>`) positions only**:
`matches / constants`, cross-multiplied **strictly above** τ; wildcard positions
are excluded from *both* numerator and denominator, and a **zero-constant
template (all `<*>`) matches nothing**. Ranking among candidates is by matched
constants (most-specific wins), ties to the lowest id. This is the unique local
rule that satisfies *both* spec requirements at once, resolving the round-3 vs
round-5 tension the review chain surfaced (the amendment ships in
`tasks/67-logtmpl-sensor.md`, with a note crediting both repros):

- **stable ids (round-3 repro, still green):** an absorbed line still matches
  every surviving constant of its generalized template, scoring
  `constants/constants = 1 > τ`, so re-folding never remints it — the drift
  `[0,0,0,0,1] → [0,2,2,0,1]` cannot recur.
- **no over-merge (round-5 example, now fixed):** after `a b c d e` / `a b x d e`
  generalize to `a b <*> d e`, the distinct line `a b y q r` shares only the
  `a b` prefix — `2/4`, not above τ — so it mints a new species instead of
  merging (`distinct_line_sharing_only_the_prefix_mints_a_new_species`). The
  earlier wildcard-covers-any rule scored it `3/5` and wrongly merged.

**Zero-constant lines (round 9).** A line with no constant token (all-`<*>` — an
all-numeric or blank line) can't match *anything* via this rule, not even its own
identical twin, so it falls to the mint path. To keep it a **stable, single**
species (and honour shape-uniqueness there too), the mint path first checks the
leaf for a live template with the *exact same shape* and reuses it rather than
minting a duplicate (`Codebook::leaf_shape_twin`; a constant-bearing exact-shape
line never reaches here — it scores `1 > τ` and matches above). Pinned by
`zero_constant_shapes_reuse_instead_of_minting` and the sensor-level
`zero_constant_lines_get_stable_ids` (blank + `123 456` observed twice → stable
ids, no duplicate mints). The k3s cardinality is unaffected (78 species → 78
distinct cell keys; each fixture form has a distinct parse-tree leaf, so scoring
never merges *across* forms, and the fixtures hold no zero-constant lines).

### Parse-tree leaf keys are token-kind-tagged (round 6)

The leaf key keeps the first-`D` tokens **whole** (`Vec<Token>`), not their
`as_str()` display strings — a literal token that is textually `<*>` must not
route into the same leaf as a masked `Token::Wildcard` (their display strings
collide). `literal_wildcard_text_does_not_collide_with_a_masked_wildcard` pins it.

### Redacted `LogSensor` `Debug` (round 6)

`#[derive(Debug)]` would recurse into the private codebook and print its
templates, tree keys, and thresholds through any external `{:?}` — leaking the
internals the internality ruling keeps off the boundary. `LogSensor` has a manual
`Debug` showing only `{ channel, templates: <count>, codebook: <redacted> }`.

### Shape-uniqueness invariant + id aliasing (integrator ruling, round 8)

Constant-only similarity fixed round-3/round-5, but the review chain surfaced a
*third* sibling of the same root cause — **stateless re-scoring against an
evolving template set can't guarantee assignment stability**. Convergent
generalization can drive two templates to the *same shape* (`a b c d e` → 0,
`a b x y z` → 1, both eventually `a b <*> <*> <*>`); a re-arriving `a b x y z`
then scores `2/2` on both and the lowest-id tie-break silently reassigns it,
conflating species. The integrator ruled **Option A** (structural, not another
point fix):

- **Invariant:** no two *live* templates share a shape. A merge-generalization
  whose result equals a live twin's shape instead **merges into the survivor**
  (lowest id); the twin is retired (removed from its leaf) and
  `retired_id → survivor_id` recorded in a serialized **alias table** (`BTreeMap`,
  deterministic). A template's leading `depth` tokens never generalize (they are
  the leaf key), so equal shapes always share a leaf — the collision check is
  leaf-local, and at most one twin can exist (all other live shapes were already
  distinct).
- **Canonicalization:** every id the crate returns — `observe`'s `Feature`,
  `adapt`'s `TemplateRecord`, and therefore CellFn's folded ids — passes through
  `Codebook::canonical` (follow the alias chain). Survivors are always lower, so
  it strictly descends and cannot loop; `from_json` rejects a non-descending or
  out-of-range alias (`Error::CorruptAlias`) so untrusted bytes can't induce a
  loop or OOB. Template slots are **never removed** (id → index stays stable); a
  retired slot survives, reachable only through the alias.
- **Load-time integrity (rounds 9+):** `from_json` validates the whole snapshot's
  self-consistency — first the alias table (well-formed: strictly descending, in
  range → `Error::CorruptAlias`), then per leaf: ids exist
  (`Error::DanglingTemplate`); no **retired id is still live** in a leaf, since an
  honest merge removes it and a retired-but-live candidate could be
  matched/mutated/emitted as its survivor (`Error::RetiredTemplateLive`); the list
  is **strictly ascending**, since `ingest`'s lowest-id tie-break relies on it
  (`Error::NonAscendingLeaf`); and **no two live candidates share a shape**, the
  shape-uniqueness invariant an honest fold maintains by merging/reusing
  duplicates (`Error::DuplicateLiveShape`). Pinned by the four
  `from_json_rejects_*` tests (non-ascending leaf, retired-but-live via `leaf
  [0,1]`+`aliases {1:0}`, and duplicate live shape via a forged equal-shape
  template).
- **Version bump:** the serialized state gained the alias field, so
  `CODEBOOK_VERSION` is now **2** (an old v1 blob is rejected on load).
- Pinned by `convergent_shapes_merge_into_the_survivor_with_a_serialized_alias`
  (codex's exact scenario — collision, survivor id, alias `1→0` serialized +
  survives reload), `merged_species_are_canonical_across_observe_and_adapt`
  (both views agree, retired id never surfaces), and
  `from_json_rejects_a_non_descending_alias`. Re-ingest stability and
  adapt/observe agreement (rounds 3/5) stay green. k3s cardinality is unaffected
  (78 → 78; fixture forms occupy distinct leaves, so no shapes ever converge).

### Re-derivation contract: canonical modulo drift (integrator ruling D1)

Shape-uniqueness closes the *convergent-merge* family, but an exhaustive
double-fold search (round-9 analysis) found a **distinct** id-instability Option A
does not cover — a **cross-observe erosion-steal**. Witness
(`cross_observe_erosion_steal_is_accepted_drift`): `a b d c / a b e d / a b d d /
a b c c` folded twice gives raw ids `[0,1,0,0]` then `[0,1,1,0]` — `a b d d`
drifts from species 0 to species 1 with the alias table **empty**. Mechanism (not
a merge): the lowest-id tie-break assigns `a b d d` to id0 (keeping id0's pos-2
constant), a *later* line erodes that constant (id0 → `[a b <*> <*>]`), and on the
re-fold id0 scores below id1, which takes the line — the two templates stay
distinct shapes, so nothing merges.

The integrator **ruled D1** (`INTEGRATION.md` 6c): (a) Option-A aliasing stays for
shape convergence; (b) **exact re-derivation of a recorded trace is defined as
replay against the recording-time codebook snapshot** — the task-65 runtrace store
persists that snapshot, so a recorded trace re-derives bit-for-bit by
construction; (c) cross-observe erosion-steals on an *evolving* codebook are
**accepted as documented clustering drift** ("canonical modulo drift"), because
exactness where it matters is already delivered by the snapshot. The tie-break is
left as-is (favour the lowest live id); reshaping it or aliasing-on-every-steal
(D2/D3) were rejected. So `observe`'s "ids stable across the run sequence" means
*canonical modulo this accepted drift*; exact replay is a snapshot concern, not a
live-fold one.

### `adapt`'s stability contract: diagnostic view + snapshot replay (ruling D1)

`adapt` is the **diagnostic clone-view** fold — exact re-derivation of a recorded
trace goes through the stored recording-time snapshot (D1), not `adapt`. `adapt`
folds a **clone** of the campaign codebook, so any merge/alias it forms lives only
in that clone and is discarded — and that leaks nothing, because `adapt` returns
records whose `template` id is already canonicalized through the clone's alias
table (pass 2): a consumer only ever receives **survivor** ids, never a retired
one it would need the unseen alias to reconcile. The fold being deterministic,
`adapt(t)` on a base `B` yields the *same* canonical id stream `observe(t)` would
on `B` (the clone's `B ∪ t` equals the campaign's); only the persisted state
differs. Audited against this PR's two consumers:

- **`Matchable` (`TemplateRecord`)** — `attr("template")` is the view-canonical
  id, a within-record value the matcher DSL (task 66) compares against a config
  literal; it is never compared to a raw id from a different derivation.
- **CellFn v1** — folds the **template-channel Features emitted by `observe`**
  (already campaign-canonical), not `adapt`'s records; the matcher `cell`-role
  ids it also folds are content hashes (task 66), independent of our template
  ids. So CellFn never sees an `adapt` view-local id.

Neither consumer compares `adapt` output to historical raw ids, so the view-local
alias needs no exposure. (Cross-base drift is the accepted D1 behavior above, not
an `adapt`-specific defect — it affects `observe` and `adapt` equally.)

### Codebook internality (the EXPLORATION ruling)

Only stable `FeatureId`s cross the boundary (via `Feature` and the
`TemplateRecord::template` attribute). The codebook and its config are
`pub(crate)` (see the API-boundary note above), so the spine and explorer cannot
couple to template text, tree structure, or thresholds; the crate-internal
`template_text()` helper (used only by tests) returns a `String`, never a signal.

### Parameter extraction reads the *raw* tokens

Clustering compares the **masked** stream, but `param.N` is pulled from the
**raw** tokens at the (final) template's wildcard positions — so a masked value
survives verbatim (`host=10.0.0.1`, `5432` in the gate-6 line extract intact even
though each was masked for clustering because it carries a digit).

### Line decoding drops exactly one terminator

A scrape record's `line` bytes are decoded UTF-8-lossy (task 65 stores bytes
verbatim; lossy keeps decoding total over arbitrary bytes) and **exactly one**
line terminator — a trailing `\r\n` or `\n` — is dropped, never all trailing
`\r`/`\n` (round 5). A payload that genuinely ends in `\r` (a progress bar, a
protocol echo) therefore keeps its bytes and clusters to the same template; a
bare trailing `\r` with no `\n` is treated as payload, not a terminator.
`strips_exactly_one_line_terminator` pins the cases.

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

1. Standard suite — build / nextest (71 tests) / clippy `-D warnings` / fmt / deny,
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

## Consuming task 65's `Record`

Task 65 landed the concrete scrape-tier `Record` as **raw bytes** —
`{ stream: StreamId, line: Vec<u8> }`, one verbatim newline-delimited line — and
its doc explicitly delegates structural meaning ("log vs. span, parsed fields")
to this crate's codebook. `LogSensor` therefore decodes each record's `line`
UTF-8-**lossy** (bytes are stored verbatim; lossy decoding keeps clustering total
over arbitrary bytes) and drops the trailing terminator, then clusters it. Every
scrape record is a console line, so **all** records cluster regardless of
`stream` (a per-stream channel split is a future knob, not v1). The fixture
loader mirrors the recorder: one `Record { stream: StreamId(0), line }` per line,
partitioned with `split_inclusive('\n')` so the bytes are **verbatim** — an
unterminated final line gains no spurious `\n`, and a CRLF or payload `\r`
survives intact (round 7; `lines()` + `push('\n')` would have rewritten them).

## For the integrator

- `Cargo.lock`: this branch merges `main` (which had merged tasks 65/66), so the
  committed lock delta vs `main` is purely the additive `logtmpl` entry, and
  `cargo build --locked -p logtmpl` succeeds (round-3 fix).
- `CellConfig::cell_channels` is empty by default. A campaign that wants pod-phase
  / recovery-state harvested (SGFuzz's reified state) wires the matcher
  `cell`-role `ChannelId`s in here, and the driving archive must present each such
  channel's **current** value in the per-moment slice (see the slice contract).
- `CellConfig` is `#[serde(default)]`, so a partial tuning file (`{}`,
  `{ "fold_k": 128 }`) deserializes with the documented defaults for any omitted
  knob — task 69 tunes one dial without restating the rest (round 8, pre-approved).
- `TEMPLATE_CHANNEL` is `ChannelId(1)` (coverage is `0` in the explorer defaults);
  override via `LogSensor::with_channel` / `CellConfig::template_channel` if a
  campaign renumbers channels.
