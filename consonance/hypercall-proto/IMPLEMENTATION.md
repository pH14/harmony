# hypercall-proto implementation notes

- Implemented the protocol as an alloc-free `no_std` core with optional `guest` client and `host` dispatcher/reference services.
- Rejected using unordered service registries; `Dispatcher` uses `BTreeMap` so state snapshots are byte-deterministic.
- Reference entropy is exactly the specified xorshift64* stream and serializes its `u64` state as little-endian bytes.
- Known limitation: `Client::block_read` requires an output length that is a whole number of 512-byte sectors, returning a protocol error otherwise.
- `Client::event_emit` sends exactly one Emit frame per call; `data` longer than `MAX_PAYLOAD - 4` returns `ClientError::InvalidLength` rather than being fragmented (one emit = one logical event; only `entropy_fill`/`block_read` chunk internally, per spec).
- The frame magic constant is the u32 value `0x31504348`, i.e. the bytes `"HCP1"` on the little-endian wire (the task spec's value notation was corrected to match; the VMCALL RAX magic in `docs/INTEGRATION.md` is the same value).
- `Dispatcher::restore_state` is atomic: a failed restore rolls services back to their entry state, so an `Err` never leaves the dispatcher half-restored.
- `SeededEntropy::restore_state` rejects the all-zero state (unreachable from `save_state`; it would pin the xorshift64* stream at zero). The guest client validates the transport-returned response length before use, so a hostile host length cannot panic the guest.
- Gates run locally: standard cargo build/test/clippy/fmt with all features, plus the no-default guest build for `x86_64-unknown-none`.

## quality-e — model-based (stateful) property test

`tests/stateful.rs` adds a `proptest-state-machine` test (`dispatcher_matches_model`,
256 cases, 1..40 ops) over `Dispatcher`: random service registration, `dispatch`
(well-formed plus deliberately malformed frames — bad magic, wrong kind), `save_state`,
and `restore_state`. The reference independently re-implements the dispatcher's
routing/framing and each service's logic. Invariants: every `dispatch` response frame
must equal the model-predicted frame byte-for-byte, and after every transition
`save_state` must equal the model's blob (restores rewind the model in lockstep, so a
divergent restore is caught by the next dispatch or save comparison). Service ids map
to fixed canonical service types to keep the save/restore registration-shape contract
clean. Tests + dev-dep only; no library or public-API change. `Cargo.lock` untracked.
