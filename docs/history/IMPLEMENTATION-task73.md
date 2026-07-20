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

## Round-1 review (4 blocking + nit — all addressed)

1. **SDK channel survives snapshot/restore (P1).** `SdkSnapshot` captures the
   channel's replay-relevant state (seeded stream position — buggify fault +
   entropy supply — and the event log); the `ControlServer` keys it by `SnapId`
   (captured on `snapshot`, restored on `branch`/`replay`, dropped on `drop`). A
   verbatim replay continues the streams + keeps the catalog; a branch reseeds
   but keeps the catalog. `environment` grew `Prng::raw_state/from_raw_state` +
   `SeededEnv`/`RecordedEnv::stream_state/restore_stream_state`. Verified
   **portably** (a bare payload has no synchronized mid-run point to seal at — the
   `setup_complete` SnapshotPoint is a skid-tainted doorbell OUT → `NotQuiescent`;
   the campaign seals at V-time boundaries, not there): the mock stream-resume
   tests + every mock control snapshot/branch/replay/drop now run with the SDK
   channel wired.
2. **`set_class(Buggify)` rejected** — buggify has no per-class slot (per-point
   only), so it never lands a `BuggifyFire` in the net slot / makes the policy
   self-unreadable. Round-trip regression added.
3. **`GUEST_HAS_SDK` honored from construction** — `ControlServer::new`
   `enable_sdk`s the live VM, so an SDK guest before its first branch is serviced.
4. **Entropy routed** — `dispatch_doorbell` services `ServiceId::Entropy` from the
   env supply stream (snapshotted with the channel), so `entropy_fill` works.
   (nit) SPDX headers on `sdk-demo`.

## Round-2 review (follow-on P1 + P2 + P3 — all fixed, portable, no box re-run)

1. **Replay wiped the buggify policy → all-Nominal (P1).** `reset_schedule_to_fresh_vm`
   resets `recorded` to `none()` on restore, and only branch re-set the policy, so a
   replay materialized the SDK env with `none()`. Fix: `SdkSnap` captures the active
   `FaultPolicy` with each snapshot; restore is `env_policy.or_else(snapshot policy)`
   (branch → branch policy; replay → snapshot policy) before `enable_sdk`. Test:
   `replay_restores_the_buggify_policy`.
2. **SDK-stop arm now poisons a staged crossed fault (P2).** An SDK stop is at a
   skid-tainted doorbell OUT (`effective_vns` a lower bound), so it applies task 59's
   terminal crossed-fault rule (poison loud) instead of returning past a crossed fault.
3. **Catalog redeclare drops the stale coordinate (P3).** `coord_of_name` map; `declare`
   removes the name's old `by_coord` entry on a move. Test:
   `redeclare_removes_the_stale_coordinate`. All three private (no public-API change).

## Round-3 review (2 new P1s + self-sweep — all fixed; box A/B/C re-run 3/3)

1. **The link tier is live over the real wire (P1a).** New control-proto `SdkEvents` verb +
   `Reply::SdkEvents` (APP_PROTOCOL_VERSION 2→3) carries the `(moment,event_id,bytes)` capture;
   `ControlServer` answers it from `Vmm::sdk_events`; `Machine::sdk_events` (defaulted, overridden
   by `SocketMachine`) does the wire round-trip. **Both** RunTrace sites fill `events`:
   `record.rs` in-process, `campaign.rs` remote → `link::decode_events`. conductor gains a `link`
   dep. Loopback gate proves a non-empty decoded `RunTrace.events` over the socket.
2. **Doorbell E820 reservation (P1b).** `build_boot_params` splits low RAM to reserve
   `[0xE000,0x10000)`, so a Linux SDK guest never allocates over REQ_GPA/RESP_GPA. Loader E820
   tests + a per-ram-size doorbell-reserved check; xAPIC-split tests shifted +2 entries.
3. **Self-sweep:** doorbell totality on empty/oversize/garbage/page-boundary requests
   (`doorbell_is_total_on_edge_requests`); reentrancy structurally impossible (one atomic OUT);
   catalog limits bounded (SDK rejects over-frame; link decode total).

## Round-4 review (1 P1 + 2 P2s + state-machine sweep — all fixed; box A/B/C 3/3)

1. **setup_complete deferral (P1).** The engine seals eagerly on `StopReason::SnapshotPoint`, and
   a doorbell OUT is unsealable (`NotQuiescent`). Fix: `SdkChannel.pending_snapshot` set at
   setup_complete; the control loop surfaces `SnapshotPoint` at the next V-time-synchronized
   boundary (sealable). `SdkStop::SnapshotPoint` removed. Loopback gate proves a usable seal
   through setup_complete. Box `state_hash` byte-identical (only host-side surfacing moved).
2. **Reject oversized `req_len` (P2).** `service_doorbell` returns `BadRequest` for `req_len >
   MAX_FRAME` (loopback-host ABI), not a silent clamp.
3. **RestoreFailed keeps SDK (P3).** The recoverable branch now `enable_sdk`s the kept fresh VM
   (was `sdk: None` under an advertised `GUEST_HAS_SDK`).
   **Sweep:** every VM-swap that keeps a VM wires the SDK channel (new/RestoreFailed/success);
   `SnapshotPoint` only surfaces sealably; no other advertised-vs-actual mismatch.

## Round-5 review (1 P1 + 3 P2s + stop/restore surface pass — all fixed; box A/B/C 3/3)

1. **Deferred snapshot drains first (P1).** The deferred `SnapshotPoint` surfaces at the top of
   the run loop **after the drain** (not the `Continued` arm), so a fault at the boundary is
   applied + cleared before the seal — no `SnapshotWhileArmed`. Test at the fault-arrival seam.
2. **entropy_fill = VMM SeededEntropy (P2).** Routed through the same stream RDRAND uses (via
   `VtimeWiring::draw_entropy`); `sdk_supply` removed. Mixed-use test proves one stream (no dup
   words). Fail-closed if V-time unwired.
3. **state_max increase-only (P3).** `op`-aware per-register running max; only a strict increase
   mints novelty. `attr_str` helper. Test with a 5→10→3→10→12 sequence.
4. **SdkEvents paged (P4).** `Request::SdkEvents { offset }`, each reply bounded to `MAX_FRAME_LEN`,
   `SocketMachine` pages until empty. Test: a >1-frame capture splits + reassembles.
   Final surface pass: no other advertised-vs-actual mismatch (pending_snapshot can't be captured
   mid-flight — NotQuiescent there; entropy resumes via the VM snapshot; paging always progresses).

## Round-6 review (2 P2s — fixed; box A/B/C 3/3)

1. **Deferred seal gates on an empty schedule (P2/1).** Round-5 drained then surfaced, but a FUTURE
   fault (m > vns) stayed staged and `snapshot()` rejects any non-empty schedule (`SnapshotWhileArmed`).
   Now the deferred `SnapshotPoint` surfaces only when `self.schedule.is_empty()` at a synchronized
   boundary — else keep deferring as the run drains each fault at its Moment. Test: a future fault
   surfaces the point at the fault's Moment (not the earlier RDTSC); `snapshot()` then `Ok`.
2. **`Vmm::run()` stops on `SdkStop` (P2/2).** It looped to the terminal, swallowing an assertion.
   `TerminalReason::SdkStop` + `RunResult.sdk_stop`; `run` breaks on either. Defensive `unreachable!`
   in map_terminal + terminal serialization (SDK stop never routes/latches there). Test through `run()`.
   Another control/stop surface pass found nothing further.

## Round-7 review (2 items — fixed; box A/B/C 3/3, state_hash moved by design)

1. **SDK channel folded into the state_hash.** `state_blob` gains an `SDK\0` chunk (seeded stream
   positions + pending stop) present ONLY when `enable_sdk`'d — SDK-less goldens byte-identical, but
   a diverged SDK buggify stream now hashes differently. `encode_sdk_channel` helper. The demo's box
   `state_hash` moved (`3ec756…` → `df3e79…`) — expected (it's an SDK guest); gate A asserts equality,
   not a pinned value. Test: `state_hash_folds_the_sdk_stream_and_is_absent_when_unwired`.
2. **SDK stops honor the StopMask.** control-proto class bits `SNAPSHOT_POINT=8`, `ASSERTION=9`
   (standalone, bit 7 reserved for Buggify); the run loop gates both surfaces on `until.on.armed(bit)`.
   `StopMask::NONE` runs straight through; `u32::MAX`/`ALL` (box gate/engine) unchanged.
   `APP_PROTOCOL_VERSION 3→4`. Docs amended. Test: `stop_mask_gates_the_sdk_snapshot_point_and_assertion`.
   (Reviewer's round-6 split trigger fired — 3rd round on this surface; escalated to the integrator.)

## THE SPLIT (integrator ruling 2026-07-04) — this is **PR B** (the vmm-core/control seams)

Round 8's fresh pass found 1 P1 (again on the SDK×hash surface), so the split executed. The stable
tiers (environment, harmony-linux/sdk, link, sdk-demo, hypercall-proto) land as **PR A** (`task/guest-sdk`,
reduced); this branch (`task/guest-sdk-vmm-seams`) carries the vmm-core/control seams — doorbell
dispatch, SDK channel + snapshot/restore, stop surfacing/StopMask, the `SdkEvents` verb + paging,
the explorer `SocketMachine` override, the E820 doorbell reservation — with its own box gate, on top
of PR A.

### Round-8 P1 (fixed here): the SDK hash chunk now folds the active FaultPolicy

The round-7 `SDK\0` chunk captured the stream position + pending stop but NOT the active `FaultPolicy`
— so two same-seed forks at the same stream position but with **different** buggify policies hashed
equal (the policy determines the fire/nominal sequence, not the position). Fix: the `SdkChannel`
captures the policy bytes at `enable_sdk` (the caller has `EnvSpec::policy()` — `RecordedEnv` exposes
no accessor, so it is passed in, keeping PR A untouched); `encode_sdk_channel` length-prefixes them
into the chunk. Test `state_hash_folds_the_active_buggify_policy`: two policies differing only in the
buggify biasing at the same (position-0) stream hash differently; the same policy hashes equal.

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
- **`harmony-linux/sdk`** (new, standalone `no_std` workspace `harmony-sdk`): the SDK
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
- **`consonance/acceptance-suite/payloads/sdk-demo`** (new bare-metal payload): drives every SDK verb
  over the real doorbell transport; a buggify-gated planted always-violation.
  **Compile-verified** for `x86_64-unknown-none` (produces the ELF the box gate
  boots); box-only to *run*.

## The wire convention (the guest/host contract)

Owned by `harmony-linux/sdk/src/wire.rs` (canonical); mirrored privately in
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
cd consonance/acceptance-suite/payloads && cargo build -p sdk-demo --release
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
