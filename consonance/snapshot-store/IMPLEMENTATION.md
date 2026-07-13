# snapshot-store — implementation notes

Layered copy-on-write guest-memory store per `tasks/02-snapshot-store.md`. All standard
gates and task gates pass; see "Gates" below.

## Design in one paragraph

A `Store` holds `layers` (raw snapshot id → layer, a `BTreeMap`: `gc` and `store_stats`
iterate it, so its order must be deterministic) and `pages` (BLAKE3 hash → refcounted page
content, a `HashMap` under a XOR-folding hasher since task 95 M1.2c — it is lookup-only
and never iterated; see the field doc). A layer records only `gfn → PageRef` for pages
dirtied relative to its parent, plus the opaque vm_state blob. `PageRef` is either
`Data(hash)` or `Zero` — the all-zero page is special-cased and never stored, so sparse
images cost nothing and `stored_unique_pages` counts only real content. `read_page`
walks the chain (worst case O(chain length)); every layer visited on a miss memoizes
the answer in a per-layer `RefCell<HashMap>` index, making the common case O(1). The
memo is sound because sealed images are immutable and gc keeps every ancestor of a
resident layer resident; it is lookup-only, so the unordered map cannot leak
nondeterminism. `materialize` resolves the image into a freshly created sparse tempfile
(holes for zero pages), populating it through a single write mapping — one memcpy per
resolved page, not a `seek`+`write_all` pair of syscalls (task 95 M1.2b) — and maps it
`MAP_PRIVATE` via `memmap2::map_copy` — portable on macOS and Linux, no memfd — so mapping
writes never reach the file or the store. `write_page` recognises the all-zero page by
comparing bytes rather than hashing (task 95 M1.2a), so the ~3/4 of a booted image that is
zero never reaches BLAKE3.

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
- **`unsafe`:** exactly two blocks, both in `src/mapping.rs` — around `memmap2`'s
  `map_copy` (`Mapping::new`) and `map_mut` (`Mapping::populate`, task 95 M1.2b) — each
  with a `SAFETY:` comment resting on the same argument (sole handle to an unlinked
  tempfile ⇒ no truncate/modify-behind-the-map hazard). `lib.rs` stays unsafe-free.

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

Task 95 M1:

- *`data.iter().all(|&b| b == 0)` as the zero test (as the spec writes it).* Rejected on
  measurement: 11× slower than the equivalent `bcmp` against a static zero page, and the
  difference is ~0.5 s of every 2 GiB seal. See "Findings for the integrator".
- *Page-data arena (M1.2d).* Skipped, per the spec's own trigger; see "Why M1.2d was
  skipped" for the decomposition that rules it out (≤8% of `base_seal` available).
- *Timing the mmap-memcpy floor without `unsafe` in the bench.* Rejected: every
  `memmap2` mapping constructor is `unsafe`, and `Mapping::populate` is `pub(crate)` and
  so unreachable from `tests/`. The bench's one `unsafe` block (`map_mut` on its own
  freshly created tempfile) carries the same `SAFETY:` argument as `mapping.rs`. The
  crate rule "only `mapping.rs` may use `unsafe`" is preserved where it matters — the
  library — and `tests/bench_production_shape.rs` is an `#[ignore]`-d informational target.
- *Re-mapping a `try_clone`d `File` to unit-test copy-on-write in `mapping.rs`.* Rejected:
  a second handle to the tempfile contradicts the sole-handle precondition both `SAFETY:`
  comments rest on. `tests/gates.rs::mapping_writes_never_reach_the_store` already covers
  CoW end-to-end through `Store::materialize`.
- *`gc_reap`'s "release all but the tip".* Read both ways, because on a **chain** the tip
  retains every ancestor and `gc` reaps nothing (freed = 0; that is the retention design,
  not a bug). The bench reports both: `sibling_reap_ms` (64 siblings off one base, 63
  layers actually reaped) and `chain_walk_ms` (64-deep chain, the pure reachability walk).

## Known limitations

- The per-layer memo index grows monotonically with distinct gfns read through a layer
  and is only freed when the layer is dropped; no eviction. Bounded by
  `mem_pages × resident layers` entries in the worst case.
- vm_state blobs of released-but-still-needed interior layers (live descendants exist)
  stay resident until the layer itself is unreachable, although no API can read them
  anymore. Freeing them early at release time would be a small optimization.
- `next_id` is a plain `u64` increment; 2⁶⁴ seals are out of scope.
- No persistence, compaction, compression, or concurrency — all explicit non-goals.
- `materialize` still resolves and writes the **whole** image eagerly; task 95 M1.2b made
  that write path optimal (it is at the mmap-memcpy floor) but did not make it lazy.
  Demand paging (userfaultfd) is an explicit non-goal of task 95.
- `Store::pages` grew a hasher whose quality argument depends on its keys being BLAKE3
  digests. Enforced only by documentation plus one pinning test, not by the type system.

## Gates

On macOS (this machine, Apple Silicon, rustc 1.94.1) — all green:

```
cargo build  -p snapshot-store --all-features
cargo nextest run -p snapshot-store --all-features   # 27 tests + 8 ignored benches
cargo clippy -p snapshot-store --all-features --all-targets -- -D warnings
cargo fmt    -p snapshot-store -- --check
cargo deny check                                     # advisories/bans/licenses/sources ok
cargo test -p snapshot-store --test public_api -- --ignored   # frozen surface unchanged
```

Also run green inside a Linux container (`rust:1`, aarch64) per `docs/BUILDING.md`:
build / `cargo test` (14 lib + 13 integration) / clippy `-D warnings` / fmt, plus the
production-shape bench at `HARMONY_BENCH_PAGES=65536`.

Miri: `MIRIFLAGS="-Zmiri-permissive-provenance -Zmiri-disable-isolation" cargo
+nightly-2026-06-16 miri test -p snapshot-store --lib` → 10 passed. See "Findings for the
integrator" for why the crate is not (and cannot yet be) in the nightly Miri job whole.

Total `cargo test` runtime ≈ 1 s (oracle proptest at 256 cases included); the benches are
`#[ignore]`-d and not in CI.

## Bench (gate 6, informational)

`cargo test -p snapshot-store --release --test bench -- --ignored --nocapture`,
Apple Silicon, release profile:

- **Seal of a 1000-dirty-page delta:** `seal()` ≈ 240 µs; derive + 1000 `write_page` +
  seal ≈ 5.7 ms end-to-end (dominated by BLAKE3 hashing of 4 MiB during the writes).
- **`read_page` at chain depth 64:** cold ≈ 5.7 µs/read (full chain walk), warm
  ≈ 225 ns/read ≈ **4.4 M reads/s** once the memo index is hot.

## Production-shape bench (task 95)

`tests/bench_production_shape.rs` measures the store at the shape production runs — the
2 GiB guest of `consonance/vmm-core/tests/seal_rate_sweep.rs` (`GUEST_RAM_LEN = 2 << 30`,
524,288 frames) — on a synthetic booted-guest image: 1 in 4 pages non-zero, every 8th
non-zero page a duplicate of an earlier one (so 131,072 non-zero / 114,688 unique).
Informational `#[ignore]` tests, not CI. `tests/bench.rs` (32 MiB toy shape) stays.

```
cargo test -p snapshot-store --release --test bench_production_shape -- --ignored --nocapture
HARMONY_BENCH_PAGES=65536 cargo test ...   # scale down (power of two, >= 4096)
```

**Machine:** Apple M1 Max, 64 GiB, macOS 26.4.1, rustc 1.94.1, release profile,
`mem_pages=524288` (2 GiB). Peak RSS ~4 GiB, so the five benches take a process-wide
lock and run one at a time. Also run in a `rust:1` Linux container (aarch64) at
`HARMONY_BENCH_PAGES=65536` to exercise the knob and the skip paths.

### M1.2 before/after

Wall-clock on this machine drifts ~10% between cold and warm runs, so every number below
is the **best of interleaved A/B rounds** (baseline / after-M1.2b / final, alternating),
never sequential runs. `restore_floor_full_image_vec_copy` contains no store code and is
the control: it moved <3% across all rounds.

Because (a) touches only `write_page`/`seal` and (b) touches only `materialize`, the
baseline→after-b column isolates (a) for the seal benches and (b) for `materialize`.

| bench | metric | before | after (a) | after (b) | after (c) | overall |
|---|---|--:|--:|--:|--:|--:|
| `base_seal` | total_s | 1.413 | 0.515 | — | **0.487** | **2.90× faster** |
| `full_rescan_delta_seal` | total_s | 1.441 | 0.537 | — | **0.498** | **2.89×** |
| `dirty_delta_seal` n=512 | µs | 1657 | 1583 | — | **1434** | 1.16× |
| `dirty_delta_seal` n=4,096 | µs | 12610 | 13178 | — | **11223** | 1.12× |
| `dirty_delta_seal` n=32,768 | µs | 106007 | 104144 | — | **92237** | 1.15× |
| `dirty_delta_seal` n=262,144 | µs | 986508 | 985034 | — | **852836** | 1.16× |
| `materialize_sweep` r=4,096 d=1 | ms | 33.81 | — | 23.80 | **25.16** | 1.34× |
| `materialize_sweep` r=32,768 d=1 | ms | 288.34 | — | 211.61 | **208.73** | 1.38× |
| `materialize_sweep` r=131,072 d=1 | ms | 1161.85 | — | 833.13 | **814.83** | 1.43× |
| `gc_reap` sibling reap | ms | 18.70 | 18.84 | — | **7.86** | 2.38× |
| `restore_floor_full_image_vec_copy` *(control)* | ms | 45.78 | — | 46.93 | 46.08 | — |

Directional bar (gate 3), all met:

- **(a) improves `base_seal`** on the quarter-resident image: 1.413 s → 0.515 s.
- **(b) improves `materialize_sweep` at every resident count** (1.42×, 1.36×, 1.39× at
  depth 1), and lands **on** the mmap-memcpy floor. Remaining gap to that floor, final
  tree: r=4,096 → 25.16 vs 22.49 ms; r=32,768 → 208.73 vs 208.85 ms; r=131,072 → 814.83
  vs 795.69 ms. That residual is the chain resolve (a `BTreeMap` of `resident` entries)
  plus the final `map_copy`; the per-page syscalls are gone.
- **(c) regresses no bench.** Seal-side it is a clear win (`dirty_delta_seal` −12…−14%,
  `gc_reap` 2.4×, `base_seal` a further −5%). On `materialize` it is neutral: over 5
  interleaved rounds the per-case ratios span 0.93–1.06, and the **floor** — which
  executes no store code — swings by the same 0.93–1.03, so that band is APFS/page-cache
  variance, not the store.

### Chain depth and the two floors (for task 95 M2)

- `materialize` is **flat in chain depth** at these depths: r=131,072 costs 913/853/874 ms
  at depth 1/8/32 (differences are inside the I/O noise band). The resolve walk is
  `O(chain × layer size)`, and the interior layers are small — the cost is dominated by
  writing `resident` pages. M2.1's `max_chain_len = 32` is not near a cliff here.
- The two floors are ~16× apart: writing the resolved image into a **tempfile** through
  one mapping costs 836 ms at r=131,072 (512 MiB through the page cache onto APFS), while
  the full-image **anonymous-memory** memcpy that `vmm.rs::restore_guest_memory` performs
  costs 51 ms for the whole 2 GiB (≈39 GiB/s). On this machine the memcpy M2.2 removes is
  *not* the expensive part of a restore — `materialize`'s tempfile write is. M2 should
  measure both on the box (Linux, different filesystem) before attributing its win; the
  remap saves the memcpy **and** lets the CoW mapping fault lazily, but if the box's
  tempfile write is as dominant as it is here, the headline number will come from not
  materializing eagerly at all, which is task 68's territory, not this one.
- `seal_s` is negligible: at the 2 GiB base seal, `write_page` × 524,288 costs 0.476 s and
  `seal()` itself 0.011 s. The redundancy pass is not a cost centre.

### Why M1.2d (page-data arena) was skipped

Bench 1 now reports its own decomposition. Final tree, 2 GiB shape:
`writes_s=0.476`, `seal_s=0.011`, `hash_only_s=1.283` (BLAKE3 over **all** frames),
`hash_nonzero_s=0.371` (BLAKE3 over just the 131,072 frames the store still hashes).

`base_seal` (0.487 s) is now **2.6× below** the hash-only baseline, so the spec's trigger
for (d) — "bench 1's gap to the hash-only baseline shows per-page `Box<[u8]>` allocation
still matters" — is not met. Decomposing the 0.105 s that `writes_s` spends above
`hash_nonzero_s`: ~0.048 s is the zero-page byte scan over 393,216 zero frames (measured
0.123 µs/page), leaving ~0.057 s for 114,688 interns *plus* 524,288 `BuilderCore::pages`
`BTreeMap` inserts. Of that, the 448 MiB of unavoidable page-content memcpy is ~0.015 s at
this machine's memory bandwidth. So an arena targets at most ~0.04 s of a 0.487 s seal
(≤8%), in exchange for a slab + free list in a crate that deliberately confines `unsafe`
to `mapping.rs`. Not built.

### Findings for the integrator

- **The spec's suggested zero test does not vectorize.** `data.iter().all(|&b| b == 0)`
  runs at **1.35 µs/page (3.0 GB/s)** on aarch64 — LLVM keeps the early-exit branch and
  does not widen it. Comparing against a static zero page (`data == &ZERO_PAGE[..]`)
  lowers to one `bcmp` at **0.12 µs/page (33 GB/s)**, is the same predicate, and adds no
  dependency. Over 393,216 zero frames that is ~0.5 s per seal — i.e. the entire
  difference between M1.2a as specified and M1.2a as landed. Implemented as the `bcmp`
  form, documented at the call site.
- **`snapshot-store` is absent from the nightly Miri job** (`.github/workflows/nightly.yml`)
  despite containing `unsafe`, and `cargo miri test -p snapshot-store` cannot pass as
  written: `tempfile()`'s `open` needs `-Zmiri-disable-isolation`, and every
  `materialize` test then needs **file-backed** `mmap`, which Miri does not implement.
  This predates task 95 and is outside M1's surface (`consonance/snapshot-store/` only),
  so it is reported rather than patched. What *does* pass, and was run:
  `MIRIFLAGS="-Zmiri-permissive-provenance -Zmiri-disable-isolation" cargo
  +nightly-2026-06-16 miri test -p snapshot-store --lib` → **10 passed**. The four new
  `mapping.rs` unit tests are `#[cfg(all(test, not(miri)))]` for exactly this reason, so
  the lib half stays interpretable. Adding `--lib` for this crate to the Miri job is a
  one-line change an integrator can make on a branch that may touch `.github/`.
- **`Store::pages` must never be iterated.** M1.2c made it a `HashMap`; `store_stats`
  reads only `.len()` and `gc` iterates `layers` (a `BTreeMap`). The invariant is now a
  field doc. Any future iteration must collect-and-sort first or it is a determinism bug.
- **The XOR-fold hasher is not a mixer.** Keys whose four 8-byte words cancel fold
  identically (`[0xFF; 32]` and `[0; 32]` both fold to the length prefix). Sound here only
  because the keys are BLAKE3 digests of page content, never attacker-shaped input — the
  same premise `intern_page`'s collision stance already rests on. Pinned by
  `page_hash_hasher_backs_a_working_map`, so keying this map on anything else fails a test.
- **`materialize` is unchanged in behaviour**, including sparseness: `Mapping::populate`
  touches only the offsets of resolved non-zero pages, verified directly by
  `populate_leaves_untouched_pages_as_holes` (`st_blocks` on a 256 MiB image with one
  written page) on both macOS/APFS and Linux/overlayfs, and indirectly by the pre-existing
  `sparse_one_gib_materialize_stays_sparse` gate.
- **Public API is byte-for-byte unchanged** (`tests/public-api.txt` still passes), and
  every pre-existing test passes unmodified. `Mapping::populate` is `pub(crate)`.

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

## Task 35 — mutation hardening

`tests/mutation_kills.rs` adds `seal_assigns_a_fresh_id_each_time`, which seals
several snapshots and asserts their ids are **distinct** (and a derived child's id
differs from its parent's). It performs **no** chain walk, so a frozen id counter
is observed by a fast assertion rather than only by a hang.

This targets `lib.rs:521`'s `self.store.next_id += 1`. The `+=`→`-=` sibling is
caught immediately (debug-build `0u64 - 1` underflow panic). The named survivor
`+=`→`*=` freezes the counter at 0, so every `seal` reuses id 0; a derived child
then reuses its parent's id, leaving a **self-parented layer** whose
`resolve`/`materialize` chain walk never terminates (`resolve` has no cycle
break) — which is why it surfaced only as a ~372 s **timeout**, never as a
survivor. Because the existing `stateful.rs` proptest drives derive→read
sequences, the suite hangs under this mutation, so — like the `seeded.rs` loop
mutants and `unison`'s loop-condition mutants — it stays **caught by timeout**, a
non-terminating loop having no other tell. The new test still pins id
distinctness so any *terminating* counter regression fails fast, and a scoped
re-run bounds the timeout to cargo-mutants' auto-minimum (~30 s) rather than the
full-tree 372 s.

**Verification.** `cargo mutants -p snapshot-store --re 'in BuilderCore'` (the
`seal`/`BuilderCore` mutants, incl. line 521) → **5 caught, 0 missed, 1 timeout**;
the timeout is exactly `+=`→`*=`. Library and public API unchanged
(`public-api.txt` still passes); `Cargo.lock` left untracked, matching `main`.

### Task 35 re-hardening — re-verified on the post-task-50 tree

Task 50 (the net-fault boundary, which retired `dissonance/pv-net`) touched only
`dissonance/`; `consonance/snapshot-store` is byte-identical, so `lib.rs:521`'s
`self.store.next_id += 1` and the `seal_assigns_a_fresh_id_each_time` test are
unchanged. Re-verified on the current tree across the **whole** file:
`cargo mutants -p snapshot-store --file lib.rs` → **51 caught, 0 missed, 1 timeout,
11 unviable** (63 mutants). The single non-caught is exactly `lib.rs:521`'s
`+=`→`*=` — the inherent self-parented-layer infinite loop, bounded to
cargo-mutants' ≈20 s scoped minimum (never the full-tree 372 s), as documented
above. No production logic changed.

## Task 98 — `Mapping::anonymous`, the Miri-executable backing seam (PR #99)

`Store::materialize` always produces the production backing: a copy-on-write
`mmap` over a sparse tempfile — real syscalls Miri cannot execute. vmm-core's
remap-restore path (`bringup::compose_restore_target`, task 95 M2.2) performs its
own `unsafe` `Backend::map_memory` over a `Mapping`'s buffer, and that seam must
stay Miri-exercisable (the vmm-core nightly Miri gate, bead `hm-4yj`).
`Mapping::anonymous(len)` provides the same `Mapping` interface over an anonymous
heap buffer:

- **4-KiB-aligned base** (PR #99 round 2): the backing is a `Vec<AlignedPage>`
  (`#[repr(C, align(4096))]` 4-KiB pages), because `map_memory`'s contract (c)
  requires a page-aligned host address — the seam must satisfy the same contract
  the `mmap` backing does, and a plain `Vec<u8>` gives no such guarantee.
  `as_slice`/`as_mut_slice` view the page vec as bytes (this module's granted
  `unsafe`; sound because `AlignedPage` is a `repr(C)` byte array with
  `size == align == 4096`, so the Vec is one contiguous padding-free run of
  initialized bytes).
- The exposed `len` is exact; the allocation rounds up to whole pages.
- Zero-filled — byte-observably identical to a freshly materialized all-zero
  image. Fill via `as_mut_slice`.
- Production restores always go through `Store::materialize`; `anonymous` is for
  tests / the UB safety net. Its consumer is vmm-core's
  `bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping`,
  which reads the mapped buffer back through a retained raw pointer after the
  `Mapping` moves into the `Vmm`.

Tests (`mapping::anon_tests`, all Miri-run): zero-fill + write visibility,
zero-length, **base alignment** (pins the contract the round-2 review caught),
exact-length. Public API: `Mapping::anonymous` added to `tests/public-api.txt`
(intentional, regenerated with `UPDATE_PUBLIC_API=1 cargo test -p snapshot-store
--test public_api`).
