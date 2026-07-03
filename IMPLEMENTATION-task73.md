# Task 73 — the harmony guest SDK + link-tier decode: implementation & box handoff

Phase H, the link tier. This task builds both halves of Tier 2 of the
cooperation gradient: the **guest SDK** (assertions / IJON state / buggify /
lifecycle — hooks + transport only) and the **host-side link-tier decode** that
populates `RunTrace.events`.

Branch `task/guest-sdk`. **Do not push** (per the worker charter — the foreman
picks this up).

## Status at a glance

| Gate | What | Status |
|------|------|--------|
| 1 | Standard suite green on every touched crate (macOS); SDK builds `x86_64-unknown-none` | **GREEN** (portable) |
| 2 | Portable decode + catalog proptests (≥256): decode never panics; catalog fold; a sometimes hit → `(Moment, Feature)` admitted by the spine Archive; a planted always-violation → `Bug` with a stable fingerprint; never-fired report round-trips | **GREEN** (portable) |
| 3 | Portable stream-separation golden: buggify leaves the supply stream byte-identical; per-point biasing golden | **GREEN** (portable) |
| 4 | **Box** determinism: SDK demo guest, same seed twice ⇒ byte-identical decoded stream + equal `state_hash` | **HANDED TO FOREMAN** (needs `/dev/kvm`) |
| 5 | **Box** the Bug path: a planted buggify-gated always-violation ⇒ `StopReason::Assertion` ⇒ `Bug`; `branch(genesis, bug.env)` replays N/N, N ≥ 8 | **HANDED TO FOREMAN** |
| 6 | **Box** never-fired: two declared sometimes points, one wired ⇒ report flags the other; the fired one is a `(Moment, Feature)` | **HANDED TO FOREMAN** |

The portable gates (1–3) are the reusable core and are fully verified. The box
gates (4–6) need the patched-KVM box and are handed off per this project's
established frontier pattern (tasks 59/60/63/68).

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

## Box handoff — the three vmm-core seams (read-only except these)

vmm-core builds on macOS (`mock` feature; no KVM deps), so the wiring below is
compile- and mock-testable laptop-side, but the three box gates need the box.
Precise anchors (from a read-only survey):

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

### The box gate harness

Mirror `consonance/vmm-core/tests/live_host_plane.rs` (task 59): a
`#![cfg(target_os = "linux")]`, `#[ignore]` `tests/live_sdk.rs` that boots
`guest/payloads/target/x86_64-unknown-none/release/sdk-demo` on the real backend
with `GUEST_HAS_SDK` negotiated, then:

- **Gate 4:** run the same seed twice; assert the raw `(Moment, event_id, bytes)`
  streams are byte-identical (⇒ the decoded `(Moment, GuestEvent)` streams are
  too, `decode_events` being a pure function) and `state_hash` is equal. The
  harness can assert on the **raw** stream to avoid a consonance→dissonance dep;
  decode purity gives the decoded-stream equality.
- **Gate 5:** run with buggify enabled (high per-point bias on `slow_disk`) so
  the always-violation trips; assert `StopReason::Assertion`, mint a `Bug` via
  `link::AlwaysViolation`, and `branch(genesis, bug.env)` + re-run reproduces the
  violation N/N (N ≥ 8).
- **Gate 6:** capture the raw stream + declaration, `link::Catalog::fold`; assert
  `rollback_seen` ∈ `never_fired` and `commit_seen` ∈ `fired`, and that
  `commit_seen`'s hit is a `(Moment, Feature)` via `link::LinkSensor`.

## Box-safety (CRITICAL — from the spec)

Stock KVM = **1396736**; the patched module is larger. ALWAYS leave the box on
stock + verified after every run: `pkill -9 -f sdk_` (and any `live_*`) FIRST →
wait `lsmod | grep '^kvm_intel'` users=0 → `rmmod kvm_intel kvm; modprobe kvm;
modprobe kvm_intel` → verify size 1396736 on a FRESH ssh connection. SSH drops
(exit 255) on pkill/rmmod are normal — reconnect + verify. Pin builds/tests to
`taskset -c 2` (`docs/BOX-PINNING.md`). Run gates in the foreground and READ
results before reporting.

## Deviations considered and rejected

- **Building the full doorbell/`decide`-seam integration unilaterally.**
  Rejected: the spec makes the doorbell→`Dispatcher` wiring an explicit shared
  seam with task 61 that "the foreman sequences", and the end-to-end events hop
  crosses out-of-surface `conductor`/`control-proto`. A large, box-only,
  cross-boundary rewrite of the core run loop that I cannot run here — and that
  task 61 might land instead — would be reckless. The portable logic is complete
  and the box wiring is spec'd above with exact anchors.
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
