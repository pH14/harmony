# `dissonance/flow` — implementation notes

The pure-logic core of the one central L4 flow-fault proxy task 50's `net_decide`
seam feeds. `FlowEvent`s + per-flow decisions in → a deterministic, V-time-scheduled
stream of `FlowAction`s out. Two engines behind one `FlowEngine` trait:
`ToxiproxyEngine` (shipped, toxic semantics) and `PassthroughEngine` (faults-off
baseline + the proof the trait abstracts over more than one mechanism).

## Crate facts for the integrator

- **Package name `flow`** (dir = name, matching the other dissonance crates). All
  gates run `-p flow`.
- **No `unsafe`** → no Miri gate; not added to the `quality.yml` `miri` list.
- **No floats, no wall-clock, no `rand`, no `HashMap`/`HashSet`.** The only ordered
  container is the `BTreeMap` in `sched.rs` (keyed `(VTime, seq)`) and the per-conn
  `BTreeMap` in `toxiproxy.rs`; neither lets iteration order reach an action.
- **No `save_state`/`restore_state`** — by design (the win over the retired
  host-side `pv-net`). The engine's whole state is plain guest RAM; consonance
  snapshots/branches it for free, and replay determinism is proven by re-running
  the event sequence (gate 1), not by serializing state.
- **Local PRNG** (`src/prng.rs`) is the `hypercall-proto` xorshift64\* algorithm
  re-implemented locally (conventions rule 2), identical to `environment`'s copy.

## Design decisions (and the semantics they pin)

- **One shared scheduler (`sched.rs`).** Both engines drain through the same
  `(VTime, seq)` `BTreeMap`: `VTime` is the due time, `seq` a per-engine monotonic
  counter. Ties at one V-time break by insertion order, never by map/hash order
  (gate 6). Putting ordering in one place means every `FlowEngine` inherits the
  same deterministic drain.
- **`Throttle{bps}` = bytes per V-time unit.** There is no real "second" in this
  model (V-time is the only clock), so the V-time tick plays the role of the
  second; the frontier maps V-time→real-time when it programs `tbf`. A chunk of
  `n` bytes costs `ceil(n / bps)` V-time units, charged against a **per-direction**
  transmit cursor (`max(arrival, cursor) + cost`), so the two halves of a
  connection don't throttle each other. `bps == 0` yields a `u64::MAX` cost (fully
  stalled) rather than dividing by zero.
- **`Loss` rolls a per-conn PRNG seeded from the decision.** Exactly one draw per
  chunk (kept aligned to the chunk count); drop iff `roll % den < num`. `num >= den`
  drops everything (`1/1` = full drop); `den == 0` delivers — matching
  `environment`'s documented "den==0 = no-op at enforcement" so the two layers
  agree.
- **`Reset` policy vs. `Close` teardown.** The `FlowAction` vocabulary has no
  "close" action, so a connection teardown *is* a `Reset`. A `Reset` **policy**
  fires one `Reset` at the first event that carries a V-time (the first chunk, or
  the close) and drops everything after it. A normal **`Close`** on any other
  policy schedules a `Reset` at `max(close_at, last_deliver)` so the teardown never
  precedes still-pending (delayed/throttled) delivered data for that flow.
- **Decider consulted exactly once per flow, on `Open`** (gate 5). A duplicate
  `Open` for a known flow is ignored (no re-consult); a `Chunk`/`Close` never
  consults. Draw order is therefore the `Open` order — a `HashMap`-backed conn set
  would have failed this.
- **Saturating V-time everywhere.** `at + d` and the throttle cursor use
  `saturating_add`; a hostile `Latency(u64::MAX)` or an `at` near `u64::MAX` clamps
  to `u64::MAX`, never wraps into the past (gate 2).
- **Stray events are ignored, never panics — in *both* engines.** The `FlowEngine`
  trait contract (spec §85–86, §92–93) makes totality on guest input a requirement
  of *every* impl, not just toxiproxy. A `Chunk`/`Close` for an unknown or
  already-torn-down flow schedules nothing. So `PassthroughEngine` also tracks flow
  lifecycle (registered on `Open`, torn on `Close`) and delivers verbatim only for a
  *live* flow — it just does so **without consulting the decider** (gate 4). A chunk
  for an unopened flow, a chunk after `Close`, and a close for an unknown flow are
  all ignored. Guest-controlled `Chunk.bytes` are arbitrary and never inspected for
  control flow.

## Deviations considered and made

- **`PassthroughEngine` carries private fields** (`conns` + `Scheduler`) instead of
  being the literal unit struct (`pub struct PassthroughEngine;`) the spec sketches.
  Two reasons. (1) A buffering `FlowEngine` is inherently stateful — `on_event`
  records actions and a *later* `due(now)` drains them — so the events have to live
  somewhere between the two calls. (2) The trait contract requires stray-ignore
  (above), which needs a per-flow lifecycle map. Adding private fields is the "add
  private items" rule 3 explicitly allows; the struct's name, visibility, meaning,
  and `FlowEngine` impl are unchanged. Because the spec gave it no constructor, both
  `PassthroughEngine::new()` and a derived `Default` are provided so construction
  stays a one-liner. (`ToxiproxyEngine` matched its spec'd `pub fn new()` directly.)
- **Added `pub fn FlowAction::at(&self) -> VTime`** (an *addition*, conventions
  rule 3 — nothing specified was removed/renamed). It is the action's due time —
  the key `due` drains on — and the frontier needs it to enact actions; the gate
  tests use it too.
- **Added the standard derives** (`Clone`/`Debug`/`PartialEq`/`Eq`, plus
  `Copy`/`Ord`/`Hash` on the newtypes) to the public types — required by the
  property tests and harmless to the contract. Every other public item matches the
  spec's Public API exactly.

## Known limitations / notes

- **Throttle is ceil-per-chunk**, so a stream of tiny chunks delivers slightly
  *below* the nominal `bps` (each pays a whole V-time unit minimum). This is
  deterministic and adequate for a fault model; an exact fractional-remainder pacer
  was rejected as unnecessary complexity for a pure-logic fault injector.
- **Closed flows are retained, not evicted (`ToxiproxyEngine` *and*
  `PassthroughEngine`).** A closed flow stays in `conns` (left `torn: true`), so a
  later `Open` reusing the same `ConnId` is skipped and the map grows with the number
  of distinct flows in a run. This is **spec-conformant**: `ConnId` is an opaque
  `u64`, the contract is "exactly once per flow", and a closed conn is still *known*
  (so a late chunk on it is correctly ignored rather than treated as a fresh flow).
  It rests on a **frontier invariant**: the `net_decide`/proxy shell must hand out a
  *fresh* `ConnId` per flow (e.g. a monotonic accept counter), **not** a raw,
  reusable 5-tuple hash. If the frontier ever needs 5-tuple reuse, the clean fix is
  to **evict on `Close`** (remove the entry) — which keeps memory bounded *and* still
  satisfies stray-ignore (an evicted conn is "unknown", so a late chunk/duplicate
  close is ignored) while letting a reused `ConnId` re-decide as a new flow. Not done
  now (no consumer; would re-shape the gate-5 "distinct opens" semantics); flagged so
  the choice is deliberate. Per-run flow counts are bounded, and consonance snapshots
  the whole map for free, so unbounded-in-principle growth is bounded-per-run in
  practice. See the PR reply for the full reasoning.
- **Per-message / L7 faults** (reorder/dup/corrupt a *specific* message) are out of
  scope here (the SDK/L7 tier, a later task) — they need message boundaries the L4
  flow layer cannot see.

## Tests → acceptance gates

- **Gate 1 (trait-generic determinism):** `tests/determinism.rs`
  `toxiproxy_is_deterministic` / `passthrough_is_deterministic` — the same property
  run against *both* impls, 256 cases each; plus `due_respects_now`.
- **Gate 2 (no panic + saturation):** `tests/total.rs` (512-case no-panic property
  over arbitrary events incl. edge V-times; stray/after-close unit tests) and the
  `cargo-fuzz` target `fuzz/fuzz_targets/on_event.rs`. Saturation goldens live in
  `tests/golden.rs`.
- **Gate 3 (per-policy golden):** `tests/golden.rs` — one golden per `FlowPolicy`,
  including the `Loss` kept/dropped set for `seed=0xC0FFEE` derived from the
  xorshift64\* stream.
- **Gate 4 (`PassthroughEngine` nominal):** `tests/golden.rs`
  `passthrough_is_nominal_and_never_decides` (asserts the recording decider is
  never consulted), plus the passthrough stray-ignore exact-behavior tests
  (`passthrough_ignores_chunk_for_unknown_conn`, `…_chunk_after_close`,
  `…_close_for_unknown_conn`, `…_duplicate_close`) — each also asserts the decider
  is never consulted.
- **Gate 5 (decider-driven):** `tests/decider.rs` — once-per-flow, in `Open` order,
  plus a property tying the consult count to the number of distinct opens.
- **Gate 6 (no order leakage):** `tests/determinism.rs`
  `same_vtime_ties_break_by_insertion_order` (behavioral) and
  `source_uses_no_hash_containers` (structural).

## Fuzzing

`fuzz/` is a self-contained cargo-fuzz workspace (empty `[workspace]` table, like
`dissonance/control-proto/fuzz`), so the standard `-p flow` gates and the root
`cargo deny`/workspace glob ignore it. It depends on `libfuzzer-sys` and
`arbitrary` (the conventions-approved fuzz/dev dependency). The target was
typechecked locally with the pinned nightly
(`cargo +nightly-2026-06-16 check --manifest-path dissonance/flow/fuzz/Cargo.toml`);
running it needs `cargo-fuzz`, which is a box/CI-nightly tool, not installed on the
Mac. `cargo deny --manifest-path dissonance/flow/fuzz/Cargo.toml check licenses`
passes locally.

## Public-API snapshot

The frozen public surface is committed at `tests/public-api.txt` and guarded by
`tests/public_api.rs` (mirrors `dissonance/control-proto` exactly: same
`-sss --all-features` invocation, same pinned nightly `nightly-2026-06-16`, same
skip-loudly-when-tooling-absent behavior, so a stable-only box stays green). The
gate is `#[ignore]`d in the normal suite and runs via
`cargo test -p flow --test public_api -- --ignored` in the CI `public-api` job.
Refresh after an intentional, reviewed API change with
`UPDATE_PUBLIC_API=1 cargo test -p flow --test public_api -- --ignored`.

**CI wiring owned by the integrator (conventions rule 1 — this branch does not
touch root / CI files):** add `-p flow` to the `public-api` job's crate list in
`.github/workflows/quality.yml`, and add a
`cargo deny --manifest-path dissonance/flow/fuzz/Cargo.toml check --config deny.toml licenses`
line to the out-of-workspace deny step (mirroring the existing `control-proto`
fuzz line). Both pass locally.

## What the frontier binds against this crate

The real `accept`/`splice` TCP proxy, the transparent redirect (iptables REDIRECT /
a CNI hook) that routes inter-node traffic through the one central proxy, the
`FlowDecider` impl that issues the `net_decide` hypercall and maps
`environment::Answer` (the `NetFlow` `Net*` faults) → `FlowPolicy`, and the enacting
of `FlowAction`s on real sockets. All out of scope here, all built later against
this crate.
