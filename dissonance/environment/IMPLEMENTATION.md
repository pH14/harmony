# environment — implementation notes

The two **control planes**, the versioned catalog, the seeded backings, and the
one `Moment`-keyed reproducer — the heart of dissonance's fault model
(tasks 24 + 45). Pure logic: no `/dev/kvm`, no guest, no real I/O, no wall-clock,
no host entropy, no sibling-crate dependencies. Builds and passes every gate on
macOS and Linux. No `unsafe`, so no Miri obligation.

> **Read the task-45 section first** (below) — it is the most recent amendment
> and explains the naming decision, the `Moment` re-keying, and the breaking
> public-API change to the merged reproducer.

## What was built

The public types as the spec's Public API lists them: the catalog
(`DecisionClass`, `DecisionPoint`+`class`/`admits`, `Answer`+`encode`/`decode`,
`Fault`, `CorruptSpec`, `BlockOp`, `Outcome`); the **host plane**
(`HostFault`+`encode`/`decode`, `Action`, `Ratio`, `BitMask`, `Moment`); the seam
(`Environment::decide`); the newtypes (`NodeId`, `ConnId`, `VTime`); the constants
(`CATALOG_VERSION`, `MAX_SUPPLY_LEN`); `FaultPolicy`
(`none`/`set_class`/`from_bytes`/`to_bytes`); `SeededEnv`; the reproducer
(`EnvSpec` with `BLOB_VERSION`/`encode`/`decode`/`materialize`/`record`/`perturb`/
`host_faults`/`overrides`/`seed`/`policy`, `RecordedEnv`+`set_moment`,
`StandingFault`, `EnvError`); and the proposal seam `EnvCodec`
(`seeded`/`mutate`/`compose`).

### Module layout

`catalog.rs` (the guest vocabulary + `Answer`/`DecisionPoint` methods) ·
`host.rs` (the host plane: `HostFault`, `Action`, `Moment`, `Ratio`, `BitMask`) ·
`prng.rs` (the local xorshift64\* generator) · `policy.rs` (`FaultPolicy` +
per-class sampling) · `seeded.rs` (`SeededEnv`, two independent streams) ·
`recorded.rs` (`EnvSpec`, `RecordedEnv`, `StandingFault`) · `envcodec.rs`
(`EnvCodec`, the proposal seam) · `codec.rs` (the shared little-endian writer + a
forward-only bounds-checked `Reader`, and the `Answer`/`Fault`/`HostFault`/`Action`
byte forms) · `error.rs` (`EnvError`).

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

- **CI wiring (two root-file edits, both following precedent).** `environment` is
  already in the `public-api` job's `-p` list (task 24). This task adds two shared
  edits, each matching the existing per-crate pattern: (1) `-p environment` in the
  `kani` job in `.github/workflows/quality.yml` (next to `-p vtime -p lapic`), so
  the `#[cfg(kani)]` proof is actually checked in CI — the round-4 P2 gate-hole
  fix; (2) `**/envcodec_proofs.rs` in `exclude_globs` in `.cargo/mutants.toml`
  (next to `clock_proofs.rs` / `device_proofs.rs`). No `miri` entry is needed (no
  `unsafe`). `Cargo.lock` is regenerated by the integrator — this branch does not
  commit it (matching the existing dissonance crates).

### Formal proofs (Kani)

`src/envcodec_proofs.rs` (`#[cfg(kani)]`, a child of `envcodec`) proves the bounded
integer invariant the host plane (and so `SetClockRate`) rests on, over fully
symbolic `u64` inputs (strictly stronger than the proptest sampling). It verifies
(`cargo kani -p environment` → 1 harness, 0 failures):

- `ratio_new_rejects_exactly_zero_denominator` — `Ratio::new` is total and returns
  `Some` iff `den != 0`, and every constructed `Ratio` round-trips its fields with
  `den() != 0` (the no-divide-by-zero invariant behind `SetClockRate` and the
  codec's `den == 0` rejection).

The round-4 fail-close reduced `compose` to the genesis splice (`at == 0`), so the
`m + at` re-key and its overflow guard are gone — and with them the round-1 `rekey`
helper and its two Kani proofs.

**The proof is checked by exactly one CI gate, and it is now wired.** The
`#[cfg(kani)]` harness is not compiled by the non-kani test suite cargo-mutants
uses as its oracle, so it must be excluded from mutation
(`**/envcodec_proofs.rs` in `.cargo/mutants.toml`, beside `clock_proofs.rs` /
`device_proofs.rs`) **and** run by the kani job (`cargo kani -p environment` added
to `quality.yml`). Excluding from mutation without wiring kani would leave the
proof checked by nothing — the round-4 P2; both edits land together.

## Task 45 — the host control plane (`HostFault` + `perturb` + `Moment` stamping)

This is the **amendment** the top-of-spec note scopes: it widens the merged
reproducer from guest-only to *both* control planes on one `Moment` axis. It is a
breaking public-API change (the `public-api.txt` snapshot, the codec/replay/
mutation tests, and the blob magic all change in this PR).

### Naming — the spec's `struct Environment` is realized as `EnvSpec`

The dissonance ruling overloads `Environment` for **two** things: the
`decide`-seam *trait* (`env: &mut dyn Environment`) and the reproducer *struct*
(`struct Environment { seed, overrides }`). One crate cannot name both
`Environment`. Task 24 resolved the clash by keeping the **trait** as
`Environment` (the cross-crate seam the design's `decide` example and the other
dissonance crates depend on) and naming the **reproducer** `EnvSpec`. This task
keeps that resolution: the spec's `struct Environment { seed, overrides:
BTreeMap<Moment, Action> }` is the existing `EnvSpec`, now with its overrides
re-keyed by `Moment` and widened to `Action`. Renaming the seam trait was
considered and **rejected** — it is the merged public contract other in-flight
crates (control-proto, explorer, vmm-core) implement/consume, and the note scopes
the break to "widening the recorded value type," not to the seam.

`EnvSpec` is kept as an enum (`Seeded` | `Recorded`) — a superset of the ruling's
single `{ seed, overrides }` struct — because task 24's `policy` (a seed alone
cannot reproduce a policy-dependent answer sequence) and `standing` (correlated
V-time-windowed faults) are load-bearing and dropping them would be a regression,
not an amendment. A `Seeded` spec is just the empty-overrides case.

### The one `Moment` axis

`Moment = u64` (a bare alias, per the ruling — an absolute retired-instruction
count, so codec/`compose` re-keying is plain integer arithmetic). `Action =
Host(HostFault) | Guest(Answer)` is the merged vocabulary; `EnvSpec::Recorded.
overrides: BTreeMap<Moment, Action>` puts both planes on one ordered timeline.
The old per-decision `DecisionId` key is **removed** — superseded by `Moment`
(this is the value-type/key widening the note authorizes).

- **Stamping** is uniform across both planes: `EnvSpec::record(at, action)` is the
  primitive (a guest decision at the count it surfaced, a host fault at the chosen
  count); `EnvSpec::perturb(fault, at)` is the host convenience and the recording
  half of control-proto's `perturb` verb (the transport adds the
  `ControlError`/wire semantics — that verb lives in control-proto, task 25, not
  this crate). Both promote a `Seeded` spec to `Recorded` on first use.
- **Host faults never flow through `decide`.** `materialize` routes only
  `Action::Guest` answers into `RecordedEnv`; `Action::Host` perturbations are
  read off the spec via `host_faults()` and applied imperatively by the frontier
  at their `Moment` — exactly the `StandingFault` division of labour. A host
  action sharing a `Moment` with a guest decision is therefore *not* surfaced as
  an answer (the seeded base answers); this is tested
  (`host_overrides_never_leak_into_guest_answers`).
- **`Moment` matching.** `RecordedEnv` is keyed by `Moment`; the frontier (which
  knows the retired-instruction count) calls `set_moment(at)` before each surfaced
  decision, and an admissible guest override at that `Moment` fires consuming no
  PRNG (the base advances only on fallback — the bit-identical-replay property,
  preserved from task 24). `decide(point) -> Outcome` is **unchanged** (the seam
  contract is not touched); the `Moment` arrives via the separate `set_moment`
  call, so a generic `&mut dyn Environment` user is unaffected.

### `EnvCodec` — the proposal seam (task 93's one-axis `compose`)

`EnvCodec::{seeded, mutate, compose}` is the vocabulary-aware seam the Theme calls
(it "cannot invent a legal `HostFault`/`Answer`, so it asks the codec").

`compose(base, tail, at) -> Result<EnvSpec, EnvError>` returns `Ok` **only for the
one composition it can prove bit-identical** and otherwise **fails closed**
(`EnvError::UnsupportedComposition`), rather than emit a wrong reproducer. This is
the integrator ruling: the rich compose model belongs to **task 93** (the
`EnvCodec::compose` vs genesis-only revisit, deferred). Rounds 1–3 tried to patch
the hard cases (carry/filter/shift standing faults; re-key at `at > 0`); the PR #16
cross-model pass showed each is *structurally* wrong, not an edge case.

**What `compose` supports (the provable Ok-set):** a **genesis splice — `at == 0` —
of an override-only `tail` with the same seed and policy as `base`.** The `[0, 0)`
base prefix is empty, so the result is exactly the `tail`'s overrides under the
shared seed/policy, with no standing faults — a genesis-complete reproducer that
replays bit-identically to the `tail` (proven by property over arbitrary
schedules, including seed-serviced decisions).

**Why everything else fails closed:**

- **A non-genesis splice (`at != 0`)** — the round-4 P1. The composed reproducer
  has one `SeededEnv`; replaying its `[0, at)` prefix advances the shared PRNG
  before the tail starts, but a branch-local `tail` materializes its seeded streams
  *fresh* (word 0). Any unoverridden (seed-serviced) decision in `[0, at)` desyncs
  every later seed-serviced answer → not bit-identical. The fix needs the PRNG
  **state** captured at the splice (task 93); `compose` cannot capture it, and
  cannot statically know whether the prefix draws the seed, so it rejects **all**
  `at != 0`. (This also subsumes the round-1 overflow case — overflow needed
  `at > 0`, now uniformly rejected — so the `Overflow` variant is removed.)
- **Either input carries a `StandingFault`** — its window is `VTime` (retired
  *branches*), a different clock than the `Moment` offset; correct re-keying needs
  a runtime `Moment → VTime` map `compose` lacks.
- **`tail`'s seed or policy differs from `base`'s** — one `EnvSpec` carries one
  seed/policy, so it cannot hold a piecewise stream.

Until task 93, the integrator should **branch from a genesis-complete env** rather
than compose across a non-genesis snapshot; `UnsupportedComposition` is the loud
signal to do so, never a soft failure to paper over.

`mutate(env, salt)` is deterministic and **host-only**: it inserts, moves, or
removes an `Action::Host` override (always legal — a `HostFault` needs no
`DecisionPoint`), and **every `Action::Guest` override is preserved verbatim** —
never removed, relocated, or overwritten (the destination of an insert/move skips
any guest-occupied `Moment` via `free_non_guest_slot`). This is *the P2 mutate
fix*: previously the move/remove victim was drawn from the whole merged map, so a
guest answer could be relocated out of the `DecisionPoint` context the codec
lacks. Guest-plane mutation stays the explorer's job (it has the live decision
context). `seeded` is the pure DST constructor.

`mutate`'s private logic (the per-arm `host_fault_from`, the guest-skipping
`free_non_guest_slot`, the per-op branches) is pinned with **exact-value**
assertions in a `#[cfg(test)] mod tests` *inside* `envcodec.rs` — the PR #16
round-2 `cargo mutants` survivors. These need the private `Prng` / `MUTATE_DOMAIN`
/ helpers that `tests/` cannot reach: each `host_fault_from` arm maps a controlled
PRNG stream to its exact `HostFault` (low byte chosen ≠ 0x00/0xFF so `& 0xFF` is
distinguishable from `|`/`^`); `free_non_guest_slot` returns the drawn word (never
`Default`) and skips a guest-occupied slot by exactly one; and each `mutate` op is
selected by a computed salt and asserted to its distinct effect (remove → len 0,
move → len 1 with the action preserved, insert → len 2). The round-4 fail-close
removed the `compose` prefix filter and the `checked_add` overflow path (no longer
reached), so the round-2/3 tests for those are gone; the `compose` Ok/reject set is
now covered by `tests/envcodec.rs`. `cargo mutants --in-diff` over the PR diff
reports **0 missed**.

### The D4 invariant (no Theme/explorer change to consume `HostFault`)

Verified by inspection (the explorer/Theme, task 12, is not in this worktree).
The Theme is agnostic across three opaque seams — navigation (the opaque `EnvSpec`
blob), scoring (coverage/oracle), and **proposal** (`EnvCodec` + the catalog).
`HostFault` enters *only* through those seams: it is an `Action` variant inside the
opaque `BTreeMap<Moment, Action>` the Theme never destructures, and it is produced
by `EnvCodec::{mutate, compose}` / recorded by `perturb`. The Theme orders and
manipulates overrides as `(Moment, opaque Action)` — the single `Moment` axis is
exactly what lets it do so without learning a plane. Nothing in this crate's
public surface forces a `match` on `Action::Host` vs `Action::Guest` in search
policy. So adding the host plane grew the catalog + codec and touched the Theme
contract not at all — the invariant holds.

### Deviations considered and rejected (task 45)

- **Renaming the `Environment` trait** to free the name for the reproducer struct
  — rejected (breaks the cross-crate seam; see Naming above).
- **Dropping `policy`/`standing` to match the ruling's bare `{seed, overrides}`**
  — rejected (regression; they are load-bearing for reproduction/correlated
  faults).
- **Adding a `Moment` parameter to `decide`** — rejected; it changes the merged
  seam signature (not authorized by the note) and burdens every `&mut dyn
  Environment` caller. `set_moment` keeps the seam intact and matches the
  architecture (the frontier supplies the count out-of-band).
- **Public `Ratio` fields** — rejected in favour of a checked `Ratio::new`
  (den ≥ 1) + accessors, so every constructed `Ratio` is valid, the codec
  round-trips, and no `SetClockRate` can smuggle a divide-by-zero into the
  frontier (rule 4). The decoder also rejects a zero denominator from mutated
  bytes.
- **`compose` saturating the offset / dropping tail standing faults** (the
  original PR #16 findings) — rejected. Saturating `m + at` collapses distinct
  overrides onto `u64::MAX`, and dropping the tail's standing faults loses
  reproducer state below a snapshot; both silently violate the genesis-complete,
  collision-free replay contract. Now `compose` carries the tail standing
  (shifted by `+at`) and returns `Err(EnvError::Overflow)` on any out-of-range
  re-keying — fallible by necessity, not silently lossy.
- **`mutate` selecting move/remove victims from the whole map** (PR #16) —
  rejected: it can relocate an `Action::Guest` away from the `DecisionPoint`
  context the codec lacks, forcing a wrong/ignored guest answer on replay. Victims
  are now host-only and guest overrides are preserved verbatim.

### Integrator notes (task 45)

- **Blob magic bumped `DEV1` → `DEV2`** (and `BLOB_VERSION` 1 → 2,
  `CATALOG_VERSION` 1 → 2). A task-24 blob no longer decodes — by design, the
  recorded value type changed; the differing magic makes it a loud rejection, not
  a silent misparse (tested: `dev1_magic_is_rejected`).
- **The frontier (vmm-core) owns enforcement** (out of scope here): apply each
  `host_faults()` entry at its `Moment` during a run, and call
  `RecordedEnv::set_moment` before surfacing each guest decision. `HostFault`'s
  determinism contract (`SkewTime`/`SetClockRate` integer/fixed-point;
  `CorruptMemory` = pure `word ^ mask` at `(Moment, gpa)`) is what makes that
  enforcement bit-identical on replay.
- **`compose` is fallible and fails closed; the hard model is task 93's.** It
  returns `Ok` only for a **genesis splice** (`at == 0`) of an override-only,
  same-seed/same-policy `tail` (result ≡ the tail, provably bit-identical), and
  **rejects** (`Err(EnvError::UnsupportedComposition)`) every `at != 0` (a `[0,at)`
  prefix can desync the tail's fresh seed stream), any `StandingFault` (V-time ≠
  Moment clock), and any `tail` seed/policy ≠ `base`'s (one `EnvSpec` cannot carry a
  piecewise-seeded stream). All are deferred to **task 93** (the compose-model
  revisit). The integrator should treat `UnsupportedComposition` as "branch from a
  genesis-complete env instead of composing" until task 93 lands — never as a soft
  failure to paper over.

## Gates

`cargo build/nextest/clippy(-D warnings)/fmt -p environment --all-features` and
`cargo deny check` all pass: 79 tests, including the task-45 acceptance property
`mixed_host_guest_replays_bit_identically` (256-case record→replay round-trip with
host overrides present) and the `compose` Ok/reject set:
`compose_genesis_splice_is_bit_identical_to_tail` (256-case — the accepted `at == 0`
case replays bit-identical, seed-serviced decisions included),
`compose_rejects_every_nonzero_splice` and `compose_rejects_any_standing_fault`
(256-case fail-close), plus targeted `compose_rejects_non_genesis_splice` /
`compose_fails_closed_on_standing_seed_or_policy_mismatch`. The PR #16 host-only
`mutate` hardening stays (`mutate_preserves_every_guest_override` 256-case + the
in-source exact-value mutant kills). The carried-forward guest gates remain
(override semantics cross-checked against an independent rule; per-class golden
answer sequence; host-plane wire golden; codec round-trip +
never-panic-on-arbitrary-bytes + off-version `BadVersion`; `FaultPolicy`
byte-determinism; no-order-leakage permutation tests) alongside the ignored,
nightly-only public-api guard and the `cargo kani -p environment` proof. Suite
runtime ≈ 0.5 s. As with the other proptest-using crates, the clippy run surfaces
the pre-existing workspace `clippy.toml` meta-diagnostics about the unresolvable
`rand::*` disallowed-method paths (proptest pulls `rand` into the dev graph); they
cite no crate code and do not fail `-D warnings`.

## Task 35 — mutation hardening

`tests/mutation_kills.rs` adds exact-value tests that kill the mutants the first
full-tree `cargo mutants` run left surviving (or only timeout-caught) in this
crate. No production logic changed. *(Task 45 note: the `RecordedEnv` helper in
this file now stamps a guest `Action` at a `Moment` and calls `set_moment` for the
re-keyed backing; the pinned boundaries are unchanged. Line numbers below are from
the original task-35 run and have since shifted — the function names are stable.)*

- **`catalog.rs` `DecisionPoint::admits`** scheduler bound `selection < ready`
  — `scheduler_selection_bound_is_strict` pins it strict: `ready-1` admissible,
  `ready` and `ready+1` not (the `<`→`<=` mutant would admit an out-of-range
  index equal to `ready`). Checked both directly and through a `RecordedEnv`
  override (a `ready`-valued override is ignored; an in-range one wins).
- **`codec.rs` `read_answer`** length bound `b.len() > MAX_SUPPLY_LEN` —
  `supply_length_bound_is_exclusive_at_max` decodes a supply of *exactly*
  `MAX_SUPPLY_LEN` bytes (must succeed) and one byte over (must be rejected),
  killing both `>`→`==` and `>`→`>=`.
- **`seeded.rs` `supply_bytes`** accumulator `take = (n - out.len()).min(8)` —
  `entropy_supply_is_exactly_the_requested_length` requests non-multiples of 8
  greater than 8 (12, 20, 31, 100, 255, 257); the `-`→`+` mutant overshoots to
  the next multiple of 8, so the exact-length assertion fails.

**Timeout mutants (deterministically bounded, not 372 s).** `supply_bytes`'s loop
bound `while out.len() < n` has two surviving mutants, `<`→`<=` and `<`→`==`.
Both make `supply_bytes` **non-terminating** (the final `take == 0` iteration
makes no progress, and `<`→`==` additionally hangs on `n == 0`). A non-terminating
loop has no terminating tell, so — exactly as the `unison` crate documents for
its loop-condition mutants — they are caught by **timeout**, not by assertion: any
test that calls `supply_bytes` (mine included) hangs, and the existing
`arb_point` proptests already sample `Entropy { bytes: 0 }`, so the suite hangs
under the mutation regardless of new tests. The exact-length tests above still
*pin* the contract, so any **terminating** off-by-one regression is caught fast
by assertion; and because the re-run is scoped (`-p environment`), cargo-mutants'
auto-timeout is its ~20 s minimum rather than the full-tree ~372 s, so the
detection is deterministic and bounded. (Empirically confirmed that `--fail-fast`
does **not** reclassify these to "caught": nextest cannot promptly kill the
CPU-bound infinite loop in the parallel proptest binary.)

**Verification.** `cargo mutants -p environment --file {catalog,codec,seeded}.rs`
→ **90 caught, 0 missed, 2 timeouts, 8 unviable** (from the original task-35 run).
The 2 timeouts are exactly the `supply_bytes` loop-bound mutants above.
