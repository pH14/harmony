# `harmony-sdk` (task 73) — implementation notes

The `no_std`, `alloc`-free guest SDK: hooks + transport only (the thin-SDK
ruling). Generic over `hypercall_proto::Transport`, so `Sdk<Client<VmcallTransport>>`
is a complete guest client with zero new transport code.

## What it is

- **Verbs** (`src/lib.rs`): `init(transport, catalog)` (one catalog-declaration
  Emit), `assert_always`/`assert_sometimes`/`assert_reachable`/`assert_unreachable`,
  `state_set`/`state_max`, `setup_complete`, `buggify(point) -> bool`, and
  `entropy_fill` (a re-export citing the seeded Entropy hypercall — **not** a new
  random primitive).
- **The wire convention** (`src/wire.rs`) is the **canonical source of truth**
  for the SDK event byte format: an `event_id` is `(namespace << 24) | local`,
  and each namespace has a fixed payload shape. The host-side `dissonance/link`
  decoder and the vmm-core stop-surfacing seam **mirror** these constants
  (conventions rule 2, the guest/host protocol pattern); a golden test on each
  side pins agreement. Task 74's OTel bridge takes a reserved plugin namespace
  (8..=255) — the SDK owns the allocation so channels never collide.

## Emission semantics (the thin-SDK ruling)

- `assert_always` emits **only on violation**; `assert_sometimes` emits on
  **every** hit (features are a timestamped stream, task 64). The guest never
  times anything — the host stamps each emission at the `Moment` it surfaces.
- `state_max`/`state_set` report the **raw** `(reg, op, value)`; the host
  interprets max-novelty. No max tracking in the guest.
- `buggify(point)` round-trips the host's fire decision over `ServiceId::Sdk`
  (op 1) **and** records the result on the Event stream, so the link tier
  observes reached-and-fired vs reached-and-nominal and the catalog can flag a
  never-reached buggify point.

## Deviations considered and rejected

- **Building `random()` in the SDK** — rejected per the spec: the Entropy
  hypercall is already the guest-random primitive, so `entropy_fill` forwards to
  it rather than adding a stream.
- **A max-tracking `state_max`** — rejected: it would put interpretation in the
  guest, against the thin-SDK ruling. The guest reports the raw value + op byte.
- **Carrying assertion detail bytes** — the assert payload reserves a
  `detail_len` field (always 0 today); the point id is the assertion identity.
  A message-carrying variant is a trivial additive follow-on and needs no wire
  change (the decoder already reads `detail`).
- **Putting the wire constants in `hypercall-proto`** (which all three consumers
  depend on) — rejected to keep the SDK crate the documented owner of "the
  payload convention" (spec §1) and to avoid a host-feature dependency in the
  link tier; mirrored-with-golden instead, the established pattern.

## Known limitations

- **One-frame catalog.** `init` marshals the whole declared set into one Event
  frame (≤ `MAX_PAYLOAD - 4` bytes). A catalog that overflows is rejected with
  `SdkError::CatalogTooLarge`, never truncated. Chunked declaration is a
  follow-on; the demo catalog is far under the cap.
- **24-bit local ids.** A point/register id must fit `wire::LOCAL_MAX` (the low
  24 bits); a larger id is `SdkError::PointIdTooLarge`. 16M ids per namespace is
  ample.

## Gates

- `cargo build` / `cargo test` (8 loopback tests, host) — green.
- **no_std proof:** `cargo build --lib --target x86_64-unknown-none` — green
  (the vmcall-transport gate).
- `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` — clean.
- A compile-time proof (`tests/loopback.rs`) that the SDK composes over the real
  `Client<VmcallTransport>` with zero new transport code (the box gate runs it
  for real). No `unsafe` in the crate ⇒ no Miri obligation.

## For the integrator

Standalone workspace (like `guest/payloads`), own lockfile, path deps into
`consonance/`. The box-side SDK demo payload, image wiring, and the vmm-core
doorbell/stop-surfacing seams are described in the repo-root
`IMPLEMENTATION-task73.md`.
