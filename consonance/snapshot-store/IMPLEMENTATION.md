# snapshot-store — implementation notes

Layered copy-on-write guest-memory store per `tasks/02-snapshot-store.md`. All standard
gates and task gates pass; see "Gates" below.

## Design in one paragraph

A `Store` holds two `BTreeMap`s (chosen over hash maps everywhere iteration order could
matter, per the determinism rules): `layers` (raw snapshot id → layer) and `pages`
(BLAKE3 hash → refcounted page content). A layer records only `gfn → PageRef` for pages
dirtied relative to its parent, plus the opaque vm_state blob. `PageRef` is either
`Data(hash)` or `Zero` — the all-zero page is special-cased and never stored, so sparse
images cost nothing and `stored_unique_pages` counts only real content. `read_page`
walks the chain (worst case O(chain length)); every layer visited on a miss memoizes
the answer in a per-layer `RefCell<HashMap>` index, making the common case O(1). The
memo is sound because sealed images are immutable and gc keeps every ancestor of a
resident layer resident; it is lookup-only, so the unordered map cannot leak
nondeterminism. `materialize` resolves the image into a freshly created sparse tempfile
(holes for zero pages) and maps it `MAP_PRIVATE` via `memmap2::map_copy` — portable on
macOS and Linux, no memfd — so mapping writes never reach the file or the store.

## Decisions the integrator should know

- **Released ⇒ unknown immediately.** When a snapshot's refcount hits 0 it behaves as
  unknown to every API entry point from that moment (read/derive/retain/stats/...);
  `gc` later reclaims whatever no live descendant chain still needs. Retain cannot
  resurrect a dead snapshot. This is the simplest semantics consistent with the gc
  gate; if the VMM wants "readable until gc", that is a small change but a different
  contract.
- **Redundant writes are dropped at seal.** A builder write whose content equals what
  the parent chain already resolves to for that gfn is discarded (the resolved bytes
  are identical either way, and ancestors are sealed so that can never change). This is
  what makes `owned_pages` "pages no ancestor provides identically" fall out naturally:
  the dedup gate's identical-rewrite children own 0 pages.
- **`begin_base` can be called more than once**, each call creating an independent root
  (the signature returns a builder, not a `Result`, so a second call cannot error). The
  expected usage is exactly one base per store.
- **`BuilderMisuse` is declared but never returned.** Single use is enforced at compile
  time: `seal` consumes the builder and builders hold `&mut Store`. The variant exists
  because the spec's error-enum sketch names it; it is documented as reserved.
- **`gc` accounting:** returns page payload bytes whose last reference went away plus
  the vm_state bytes of dropped layers; map/index bookkeeping is not counted.
  `bytes_resident` mirrors that definition (unique page data + vm_state blobs of all
  resident layers). `snapshots` / `logical_pages_total` count live snapshots only.
- **The mapping owns its tempfile.** "Internally maintained flat tempfile" is
  implemented as one fresh unlinked tempfile per `materialize` call, owned by the
  returned `Mapping` and reclaimed by the OS on drop. The store itself keeps nothing
  per-materialize resident, which is why `bytes_resident` stays ~10 pages in the
  sparse-1-GiB gate.
- **Collision stance** (documented at `intern_page`): BLAKE3 equality is treated as
  content equality, ~2⁻¹²⁸ collision odds — the git/content-addressed-store stance; no
  byte-wise confirmation.
- **`unsafe`:** exactly one block, in `src/mapping.rs`, around `memmap2`'s `map_copy`,
  with a `SAFETY:` comment (sole handle to an unlinked tempfile ⇒ no
  truncate/modify-behind-the-map hazard).

## Deviations considered and rejected

- *Eager full per-snapshot index (gfn → hash built at seal by cloning the parent's).*
  O(1) reads always, but seal cost becomes O(logical pages), violating the
  O(dirty pages) snapshot target. The lazy memo index keeps both targets.
- *Storing the zero page as ordinary interned content.* Simpler uniformity, but it
  pollutes `stored_unique_pages`/`bytes_resident` and costs a resident page for what
  the format already expresses as absence.
- *Store-side cache of materialized tempfiles (reuse across `materialize` calls).*
  Rejected: needs interior mutability through `&self`, complicates gc and
  `bytes_resident`, and repeated materialization of the same snapshot is not a current
  access pattern.
- *`AlreadyReleased` error variant.* Folded into `UnknownSnapshot`: released ids are
  deliberately indistinguishable from never-existing ones.

## Known limitations

- The per-layer memo index grows monotonically with distinct gfns read through a layer
  and is only freed when the layer is dropped; no eviction. Bounded by
  `mem_pages × resident layers` entries in the worst case.
- vm_state blobs of released-but-still-needed interior layers (live descendants exist)
  stay resident until the layer itself is unreachable, although no API can read them
  anymore. Freeing them early at release time would be a small optimization.
- `next_id` is a plain `u64` increment; 2⁶⁴ seals are out of scope.
- No persistence, compaction, compression, or concurrency — all explicit non-goals.

## Gates

On macOS (this machine, Apple Silicon, rustc 1.94.1) — all green:

```
cargo build  -p snapshot-store --all-features
cargo test   -p snapshot-store --all-features      # 18 tests + 2 ignored benches
cargo clippy -p snapshot-store --all-features --all-targets -- -D warnings
cargo fmt    -p snapshot-store -- --check
```

Also run green inside a Linux container (`rust:1`, aarch64) per `docs/BUILDING.md`.
Total `cargo test` runtime ≈ 1 s (oracle proptest at 256 cases included).

## Bench (gate 6, informational)

`cargo test -p snapshot-store --release --test bench -- --ignored --nocapture`,
Apple Silicon, release profile:

- **Seal of a 1000-dirty-page delta:** `seal()` ≈ 240 µs; derive + 1000 `write_page` +
  seal ≈ 5.7 ms end-to-end (dominated by BLAKE3 hashing of 4 MiB during the writes).
- **`read_page` at chain depth 64:** cold ≈ 5.7 µs/read (full chain walk), warm
  ≈ 225 ns/read ≈ **4.4 M reads/s** once the memo index is hot.

## quality-e — model-based (stateful) property test

`tests/stateful.rs` adds a `proptest-state-machine` test (`store_matches_model`,
256 cases, 1..40 ops) driving random `begin_base`/`derive`+write/`seal`/`read_page`/
`materialize`/`retain`/`release`/`gc` sequences against an in-test reference model.
The model keeps one seed byte per page per snapshot, plus refcount, `owned_pages`,
and `chain_len`. After every transition it asserts every gfn's `read_page`, the
per-snapshot `stats`, the store-wide `store_stats`, a `materialize` round-trip
(with a copy-on-write probe), and that released snapshots are uniformly unknown.
Tests + dev-dep (`proptest-state-machine`) only; no library or public-API change
(the frozen `public-api.txt` snapshot test still passes). `Cargo.lock` is left
untracked, matching `main`.
