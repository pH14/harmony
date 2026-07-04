# Task 73 — guest SDK + link tier (PR A: the stable tiers)

**This PR is the stable-tier half of the task-73 split** (integrator ruling 2026-07-04, PR #59).
It carries the tiers that have been finding-free for 4+ review rounds; the vmm-core/control seams
(doorbell dispatch, SDK channel + snapshot/restore, stop surfacing/StopMask, the `SdkEvents` verb +
paging, the explorer `SocketMachine` override, the E820 doorbell reservation) move to the follow-up
**PR B** (`task/guest-sdk-vmm-seams`), which carries its own box gate. This PR builds+tests green
**standalone** — `link` decodes from `RunTrace` fixtures, no live wire.

## What landed here (the stable tiers)

- **`dissonance/environment`** (additive): `DecisionClass::Buggify = 7`, `Fault::BuggifyFire` (wire
  tag 16), `DecisionPoint::Buggify { point }`; per-point buggify biasing on `FaultPolicy`, drawn from
  the domain-separated **fault** PRNG so buggify never disturbs the supply stream; `is_buggify_only()`;
  `Prng::raw_state`/`from_raw_state` + `SeededEnv`/`RecordedEnv::stream_state`/`restore_stream_state`;
  `set_class(Buggify)` rejected (per-point only). Versions: CATALOG 3→4, FaultPolicy 2→3, EnvSpec::BLOB 3→4.
- **`consonance/hypercall-proto`** (additive): `ServiceId::Sdk = 6`, `Client::buggify_decide`, the
  `SdkBuggify` reference host.
- **`guest/sdk`** (new `no_std` crate `harmony-sdk`): the thin SDK verbs (assertions, IJON state
  registers, buggify, `entropy_fill`, lifecycle) + the canonical wire (`event_id = (namespace << 24) | local`),
  generic over `hypercall_proto::Transport`.
- **`dissonance/link`** (new crate): `decode_events` (total, panic-free), the `Catalog` fold + never-fired
  report (task-66 shape, redeclare drops the stale coordinate), the `LinkSensor` (channels 16/17;
  `state_max` mints only on a per-register increase), and the `AlwaysViolation` oracle. Tests decode from
  `RunTrace` fixtures.
- **`guest/payloads/sdk-demo`** (new): the SDK-instrumented demo guest — a buggify-gated planted
  `always` violation; builds standalone for `x86_64-unknown-none` as the compile proof.
- **Required ripple**: `dissonance/tactics-regime`'s `class_tag` gains the `Buggify` arm (environment's
  `DecisionClass::Buggify` makes its match non-exhaustive otherwise).

## Verification

- Main workspace `cargo test`: **172 binaries, 0 failed**; `link` + `guest/sdk` green; `sdk-demo` builds;
  clippy + fmt + `cargo deny` clean; public-api snapshots frozen (environment/link/hypercall-proto/explorer;
  the explorer, control-proto, and vmm-core snapshots are byte-identical to base — this PR does not touch them).

The full round-by-round history (rounds 1–8, including every vmm-core/control fix) lives on **PR B**.
