# Task 73 — the harmony guest SDK + link-tier decode: implementation & box handoff

Phase H, the link tier. This task builds both halves of Tier 2 of the
cooperation gradient: the **guest SDK** (assertions / IJON state / buggify /
lifecycle — hooks + transport only) and the **host-side link-tier decode** that
populates `RunTrace.events`.

Branch `task/guest-sdk`, PR #59.

## Status at a glance — ALL GATES GREEN

| Gate | What | Status |
|------|------|--------|
| 1 | Standard suite green on every touched crate (macOS); SDK builds `x86_64-unknown-none` | **GREEN** (portable) |
| 2 | Portable decode + catalog proptests (≥256): decode never panics; catalog fold; a sometimes hit → `(Moment, Feature)` admitted by the spine Archive; a planted always-violation → `Bug` with a stable fingerprint; never-fired report round-trips | **GREEN** (portable) |
| 3 | Portable stream-separation golden: buggify leaves the supply stream byte-identical; per-point biasing golden | **GREEN** (portable) |
| 4 | **Box** determinism: SDK demo guest, same seed twice ⇒ byte-identical decoded stream + equal `state_hash` | **PASSED** (box) |
| 5 | **Box** the Bug path: a planted buggify-gated always-violation ⇒ `StopReason::Assertion` ⇒ `Bug`; `branch(genesis, bug.env)` replays N/N, N ≥ 8 | **PASSED** (box) |
| 6 | **Box** never-fired: two declared sometimes points, one wired ⇒ report flags the other; the fired one is a `(Moment, Feature)` | **PASSED** (box) |

**Box gate results (`live_sdk.rs`, patched KVM, CPU-pinned core 2, 2026-07-03):**

```
[gate A] 26 events, state_hash 3ec756befa032a7157ce5ee64714cba64d839b756c1613c261e99f19def016e4
         → byte-identical event stream + equal state_hash across same-seed runs
[gate B] assertion fired: point 20 (balance_nonneg); reproduced 8/8 from branch(genesis, bug.env)
[gate C] commit_seen fired, rollback_seen never — never-fired detection OK
test result: ok. 3 passed; 0 failed
```

KVM was loaded patched (1400832) for the run and **reverted to stock 1396736**
(REVERT OK, verified on a fresh connection) via the `box-window.sh` coordinator —
box-safety intact. The doorbell→services seam this landed is the shared-with-61
seam (61 reuses it).

## What landed (portable, verified)

- **`dissonance/environment`** (additive): `DecisionClass::Buggify = 7`,
  `Fault::BuggifyFire` (wire tag 16), `DecisionPoint::Buggify { point }`; a
  per-point buggify biasing section on `FaultPolicy` (default + per-point
  `num/den`), drawn from the domain-separated **fault** PRNG so buggify never
  disturbs the supply stream. Version bumps per the crate's rules:
  `CATALOG_VERSION` 3→4, `FaultPolicy` VERSION 2→3, `EnvSpec::BLOB_VERSION` 3→4;
  goldens + `public-api.txt` re-frozen. `tests/buggify.rs` pins gate 3.
- **`consonance/hypercall-proto`** (additive): `ServiceId::Sdk = 6` (id 5
  reserved for task 61's Net); guest `Client::buggify_decide(point) -> bool`;
  reference host `SdkBuggify` service for loopback tests. no_std guest build
  unchanged; `public-api.txt` re-frozen.
- **`guest/sdk`** (new, standalone `no_std` workspace `harmony-sdk`): the SDK
  verbs + the canonical wire convention (`wire.rs`). 8 loopback tests green,
  builds `x86_64-unknown-none`, composes over `Client<VmcallTransport>`
  (compile-proven).
- **`dissonance/link`** (new): `decode_events`, `Catalog`/`CatalogReport`,
  `LinkSensor`, `AlwaysViolation`. 22 tests + proptests green.
- **`dissonance/explorer`**: filled the task-64 `GuestEvent`/`RunTrace.events`
  "empty until task 73" doc stub (doc-only).
- **`dissonance/tactics-regime`**: one exhaustiveness arm for the additive
  `DecisionClass::Buggify` (`class_tag`); `class_from_tag` still rejects tag 7 so
  buggify declines to the seeded `FaultPolicy` biasing — behavior preserved.
- **`guest/payloads/sdk-demo`** (new bare-metal payload): drives every SDK verb
  over the real doorbell transport; a buggify-gated planted always-violation.
  **Compile-verified** for `x86_64-unknown-none` (produces the ELF the box gate
  boots); box-only to *run*.

## The wire convention (the guest/host contract)

Owned by `guest/sdk/src/wire.rs` (canonical); mirrored privately in
`dissonance/link/src/wire.rs` and (below) in vmm-core. `event_id = (ns << 24) |
local`; all integers little-endian.

| ns | name | local | payload |
|----|------|-------|---------|
| 0 | control | 0 = catalog decl | `[SDKC u32][ver u8][count u32]{[kind u8][local u32][name_len u16][name]}×count` |
| 1 | assert | point | `[disposition u8][detail_len u16][detail]` (disp 0=hit, 1=violation) |
| 2 | state | reg | `[op u8][value u64]` (op 0=set, 1=max) |
| 3 | buggify | point | `[fired u8]` |
| 4 | lifecycle | 0 = setup_complete | (empty) |
| 8..=255 | plugins | — | reserved (task 74 OTel) |

## The three vmm-core seams (BUILT; read-only except these)

All three seams are implemented and mock-tested (`vmm.rs`'s two new
`doorbell_*` tests; 309 lib tests green) and exercised end-to-end by the box
gates above. Anchors:

1. **Doorbell → `Dispatcher`.** `consonance/vmm-core/src/vmm.rs:1802`
   (`dispatch_out`) default-denies `OUT 0x0CA1` today (fall-through at
   `vmm.rs:1831`). Add a `DOORBELL_PORT` branch: read the request frame from
   `guest_memory()[REQ_GPA..REQ_GPA+eax]`, drive a `hypercall_proto::Dispatcher`
   the `Vmm` now owns, write the response frame into guest RAM at `RESP_GPA`.
   **This is the "shared seam with task 61" the foreman sequences** — whichever
   of 61/73 lands the doorbell first; the other reuses it. The `Dispatcher`
   registers `SeededEntropy` (from the env seed), `ConsoleSink`, `MemBlockDevice`,
   a `Moment`-stamping `EventSink` (seam 2), and the `Sdk` service (seam 3).
2. **`Moment`-stamped capture + `GUEST_HAS_SDK`.** Flip `server_caps()`
   (`control.rs:151`) to set `CapFlags::GUEST_HAS_SDK` (update the pinning test at
   `control.rs:1075`). Stamp each Event emission with the current V-time/Moment
   (the `Vmm` has `vtime` wiring) as it is serviced, so the capture is
   `(Moment, event_id, bytes)`. Surface that capture to the conductor over the
   task-65 recorder channel — **see the conductor hop below**.
3. **Run-loop stop surfacing + buggify → `decide`.** When the doorbell services
   an assert-namespace Event with `disposition = violation` (or an
   `unreachable`), stop as `StopReason::Assertion { id = point, data = detail }`
   (`control.rs:map_terminal`, ~`:882`; the wire variant already exists).
   `setup_complete` (lifecycle ns) → `StopReason::SnapshotPoint`. `sometimes`
   hits do **not** stop (StopMask-gated; the replay plane consumes them). The
   `Sdk` service answers `buggify_decide(point)` via `Environment::decide(
   DecisionPoint::Buggify { point })` and records it at its `Moment` in the
   ControlServer's `recorded: EnvSpec` (`control.rs:202`) — like every
   guest-plane decision. (This is the guest-plane `decide`-seam that task 61 also
   needs; coordinate.)

### The conductor/control-proto hop (OUT of task 73's surface — name it)

`RunTrace.events` is assembled in `dissonance/conductor/src/record.rs:311`
(`events: vec![]` today). Populating it end-to-end needs (a) the ControlServer to
surface the `Moment`-stamped raw events over `control-proto` (the task-65
recorder channel — `records` already rides it), and (b) the conductor to call
`link::decode_events` and fill `RunTrace.events`. Both `conductor` and
`control-proto` are **outside task 73's surface list**, so this small wire hop is
a cross-task integration the foreman sequences (the spec's "name it in the PR"
clause). All the pure logic it needs is in `dissonance/link`.

### The box gate harness (BUILT — `consonance/vmm-core/tests/live_sdk.rs`)

`#![cfg(target_os = "linux")]`, `#[ignore]`d (mirrors `live_host_plane.rs`): boots
`sdk-demo` on the **patched** backend via `boot_selected(Patched, …)`, drives the
`ControlServer` (`GUEST_HAS_SDK` negotiated), and asserts gates A/B/C (see the
results table above). Runs with:

```
cd guest/payloads && cargo build -p sdk-demo --release
# then, inside the box-window (below), pinned to the leased core:
cargo test -p vmm-core --release --test live_sdk -- --ignored --nocapture
```

- **Gate A** asserts the raw `(Moment, event_id, bytes)` streams are byte-identical
  across same-seed runs (⇒ the decoded `(Moment, GuestEvent)` streams are too,
  `decode_events` being pure) plus equal `state_hash`. Raw-stream assertion avoids
  a consonance→dissonance dep; the `link` decode/catalog/sensor/oracle correctness
  is proven portably in `dissonance/link`.
- **Gate B** runs buggify hot (`set_buggify_point(50,1,1)`), gets
  `StopReason::Assertion` (id 20 = `balance_nonneg`), and replays
  `branch(genesis, recorded_env())` N/N (N=8). `bug.env` is `Seeded{seed,
  buggify-only policy}` — reproduced from the seeded fault stream, so the
  `restore` guest-override rejection is untouched (only the policy check relaxed).
- **Gate C** asserts `commit_seen` fired and `rollback_seen` never (the raw
  never-fired shape `link::Catalog::fold` reports).

## Box-safety (respected — used the `box-window.sh` coordinator)

Stock KVM = **1396736**; patched = 1400832. The gates ran via
`scripts/box-window.sh acquire task73` (loads patched, leases core 2) → gates →
`release task73` (last lease out **reverts to stock + verifies**). **Never
rmmod/modprobe KVM directly when the coordinator is in use** (foreman ruling
2026-07-02). The PPID gotcha: `box-window.sh acquire` must run as a *direct child*
of a long-lived script (not a transient `$(…)` subshell), else the lease goes
stale instantly — `gate.sh` redirects `acquire`'s stdout to a file rather than
capturing it. Verified stock 1396736 + zero leases on a fresh connection after
the run. Pin per `docs/BOX-PINNING.md`.

## Deviations considered and rejected

- **Recording buggify decisions as guest overrides.** Rejected: the spec says
  "recorded at its Moment like every guest-plane decision", but recording buggify
  as `Action::Guest` overrides would make a bug's `env` carry guest overrides
  that `restore` rejects (`has_guest`) — breaking the gate-5 `branch(genesis,
  bug.env)` replay. Instead buggify reproduces from the **seed + the buggify-only
  policy** (the `SeededEnv` fault stream), which is the reproducer the spec's
  intent wants; `restore` relaxes only the *policy* check (`is_buggify_only`), not
  the guest-override rejection. Buggify decisions are still observable (the SDK
  emits a buggify event per call) and captured (`Vmm::sdk_buggify`). Note:
  reproducing a buggify decision below a **non-genesis** snapshot would need the
  fault-stream position captured — a task-61 follow-on; the gates use genesis
  branches, which start the fault stream fresh and reproduce N/N.
- **Wiring `RunTrace.events` end-to-end through the conductor.** Deferred (out of
  surface): the box gates drive the `ControlServer` directly and read
  `Vmm::sdk_events` + assert on the raw stream, so they need no `conductor`/
  `control-proto` change. Populating the production `RunTrace.events` (the
  conductor calling `link::decode_events` over a `control-proto`-surfaced event
  stream) is the small cross-task hop named in "The conductor/control-proto hop"
  above — all its pure logic is in `dissonance/link`.
- **Not bumping `EnvSpec::BLOB_VERSION`.** Rejected: `FaultPolicy`'s inner byte
  vocabulary changed incompatibly (the trailing buggify section), so per the
  crate's documented rule the container version bumps in step (the task-50
  precedent). The ripple is either symbolic (recompiles) or in-surface (goldens
  re-frozen); no out-of-surface golden pins EnvSpec bytes (conductor encodes at
  runtime).
- **Depending on `matcher` for the report type.** Rejected (surface rule): the
  minimal shared `CatalogReport` mirrors task 66's shape and is noted for
  unification.

## Known limitations / follow-ons named (not built)

- **AFLGo-style directed search toward unhit assertions** — task-70 follow-on;
  the never-fired report is its input.
- **The in-guest OTel bridge** — task 74; reuses the SDK Event-service transport
  conventions (a reserved plugin namespace).
- **A message-carrying assertion detail** — the wire already reserves `detail`;
  the SDK verb signatures stay minimal today.
- **A C-ABI header shim for foreign workloads** — deferred non-goal.
