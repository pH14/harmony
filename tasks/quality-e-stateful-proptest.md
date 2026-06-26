# Task quality-e — model-based (stateful) property tests

Read `tasks/00-CONVENTIONS.md` first. **Rule 1 waived** only to add dev-dependencies and
tests to two crates. You must NOT change library logic or public APIs — tests + dev-deps only.

## Dependency
**Requires `quality-a` merged** (it adds `proptest-state-machine` + `arbitrary` to the
dev-dependency whitelist). Branch from updated `main`.

## Environment
Runs on: macOS and Linux. Requires: Rust only. No `/dev/kvm`.

## Context
The existing proptests are mostly value-level. The two real state machines —
`snapshot-store::Store` and `hypercall-proto::Dispatcher` — deserve model-based testing:
drive a random operation sequence against both the real type and a simple reference model,
asserting they agree at every step. This is the oracle pattern `unison` already embodies.
See `docs/CODE-QUALITY.md`.

## Deliverables
1. **`snapshot-store`**: add a `proptest-state-machine` test driving random sequences of
   `begin_base`/write/`seal`/`derive`/`release`/`gc` against an in-test reference model
   (ordered collections are fine in TEST code — the determinism rules constrain library code,
   not test oracles). Assert `read_page` for arbitrary gfns and `stats` agree with the model
   after every operation.
2. **`hypercall-proto`**: add a stateful test over `Dispatcher` — random sequences of service
   registration, `dispatch`, `save_state`/`restore_state` — versus a reference model; assert
   response frames and save/restore round-trips agree.
3. ≥256 cases each; keep total `cargo test` (nextest) runtime under ~3 minutes.

## Acceptance gates
1. Both stateful proptests present and green on macOS and Linux.
2. `git diff` changes only `[dev-dependencies]` in the two `Cargo.toml`s and files under the
   two crates' `tests/` — no library/public-API change.
3. Standard gates still pass for both crates.

## Non-goals
Changing crate logic; testing the other two crates (out of scope this PR).
