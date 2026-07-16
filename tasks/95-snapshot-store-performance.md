# Task 95 — snapshot-store performance: the production-shape bench + O(dirty) capture and remap restore (D5)

> **Two milestones, dispatched separately.** **M1** is a standard **delegable** task
> (one crate, `consonance/snapshot-store`, laptop-gated, portable) — dispatch it immediately.
> **M2** is a **FRONTIER** task (box-only gates, patched KVM, the live Postgres guest) —
> dispatch it only after M1 has merged, because M1's measured numbers are M2's baseline.
>
> This task implements the already-ruled design, not a new one: `docs/INTEGRATION.md` §5's
> Memory/snapshots row — *"KVM dirty-log harvest → `DeltaBuilder`; `materialize()` → memslot
> swap"* (the mechanism the task-08 spike chose) — and retires **ROADMAP D5** (*"snapshot
> performance (dirty-log harvest + memslot-remap restore) … today restore is a full guest-RAM
> memcpy"*). Task 60 made campaign throughput measurable, which was D5's stated trigger.
>
> Why now: every production seal is a **full 2 GiB scan + 524,288 BLAKE3 hashes** (~seconds),
> and every branch/restore is a **full-chain resolve into a tempfile with 2 syscalls per
> non-zero page, followed by a full 2 GiB memcpy** into guest RAM. Task 68's lazy
> materialization minimizes *replay work* (which suffix to re-run), not these per-operation
> store costs — its spec lists them as an explicit non-goal, deferred to D5. Campaign
> throughput (tasks 69/70/86) pays these costs once per materialized exemplar.

Read first: `tasks/00-CONVENTIONS.md`; `consonance/snapshot-store/src/lib.rs` +
`consonance/snapshot-store/IMPLEMENTATION.md` (the store and its recorded trade-offs);
`consonance/vmm-core/src/snapshot.rs` (`SnapshotEngine` — the only production caller);
`consonance/vmm-core/src/control.rs` (the seal RPC → `snapshot_base`, the branch RPC →
`materialize` + `restore_snapshot`); `consonance/vmm-core/src/vmm.rs` (`restore_guest_memory`
— the memcpy M2.2 removes); `docs/INTEGRATION.md` §5; `docs/ROADMAP.md` (D5);
`tasks/08-snapshot-restore-spike.md` (the spike that chose the mechanism). For M2 also:
`consonance/vmm-backend/src/kvm_sys.rs` (`map_memory` — where the dirty-log flag goes),
`consonance/vmm-core/tests/seal_rate_sweep.rs` (the live harness whose gates must stay green),
`docs/BOX-PINNING.md`.

## Environment

- **M1 (delegable, portable):** touch only `consonance/snapshot-store/`. All gates run
  laptop-side on macOS and Linux. The benches are `#[ignore]` informational tests (not CI);
  they want ~5 GiB of free RAM at the default production shape and scale down via an env knob
  (see M1.1). No box, no KVM, no new dependencies.
- **M2 (frontier):** surface list (waiver of hard rule 1): `consonance/vmm-backend`
  (dirty-log flag + harvest ioctl), `consonance/vmm-core` (seal/restore wiring, RAM-backing
  ownership, live gates). `consonance/snapshot-store` is **read-only** in M2 — its public API
  already has everything M2 needs (`snapshot_derive(dirty: Some(..))`, `materialize()`); a
  needed store change is a finding to escalate, not a patch. Box gates run on the
  determinism box per `docs/BOX-PINNING.md` (patched KVM for live runs; always revert to
  stock `1396736` and verify).

## Context — where the cycles go today (verified against code, 2026-07-08)

1. **Seal is O(full image).** `KvmBackend::map_memory` registers guest RAM with `flags: 0`
   (`kvm_sys.rs`, the `KVM_SET_USER_MEMORY_REGION` call) — dirty logging is never enabled, no
   dirty bitmap is ever read. So the seal RPC (`control.rs`, `snapshot` verb) calls
   `SnapshotEngine::snapshot_base(vmm.guest_memory(), &blob)`, which iterates **all**
   `mem_pages` frames and BLAKE3-hashes each (`snapshot.rs`). At the production 2 GiB guest
   (`seal_rate_sweep.rs::GUEST_RAM_LEN = 2 << 30`) that is 524,288 hashes per seal; the sweep
   itself calls a snapshot "expensive (~seconds each)". The dirty-set-proportional path
   `snapshot_derive(parent, memory, Some(gfns), vm_state)` **already exists and is tested**
   — nothing feeds it a harvested dirty set. Production never derives: every seal is an
   independent base (deliberate at the time — `seal_rate_sweep.rs` "Independent
   `snapshot_base`s (not a derive chain) keep materialize O(1)-layer"; M1 measures what a
   chain actually costs, M2 supersedes the trade with a bounded chain).
2. **Restore is doubly O(full image).** The branch RPC calls `engine.materialize(id)` →
   `Store::materialize`, which resolves the full image and writes every non-zero page into a
   sparse tempfile via a `seek` + `write_all` **pair of syscalls per page** (`lib.rs`,
   `materialize`), mmaps it `MAP_PRIVATE` — and then `restore_snapshot` does
   `ram.copy_from_slice(image)` (`vmm.rs`, `restore_guest_memory`): a full 2 GiB memcpy into
   the fresh VM's separately-allocated RAM. The CoW mapping's laziness is discarded.
3. **Nothing is measured at production shape.** The existing bench
   (`snapshot-store/tests/bench.rs`) runs at 32 MiB with 1000-page deltas
   (recorded: `seal()` ≈ 240 µs, warm `read_page` ≈ 225 ns). There is no measurement of a
   2 GiB base seal, of `materialize` at realistic resident counts, or of the restore memcpy.

## M1 — the production-shape bench, then the portable optimizations (delegable)

Do M1.1 first and record the *before* numbers; land M1.2 items only with a measured
before/after delta for each. Public API is unchanged throughout M1 — every existing test must
pass unmodified.

### M1.1 The bench: `consonance/snapshot-store/tests/bench_production_shape.rs`

New file, same pattern as the existing `tests/bench.rs` (which stays): `#[ignore =
"informational bench; run with --release --ignored --nocapture"]` tests, `std::time::Instant`
behind the same file-level `#![allow(clippy::disallowed_methods)]` + "not order-observable"
justification comment. Not pass/fail in CI; the numbers go into `IMPLEMENTATION.md`.

Shared shape constants (top of file):

```rust
/// Production shape: 2 GiB guest (seal_rate_sweep's GUEST_RAM_LEN), overridable for
/// constrained machines via HARMONY_BENCH_PAGES (power of two, >= 4096).
const PROD_MEM_PAGES: u64 = 524_288;
/// Non-zero fraction of the synthetic booted image: 1 in 4 pages (a booted guest is
/// mostly zeros); every 8th non-zero page repeats an earlier content (dedup realism).
```

Build the synthetic image deterministically (seeded content like `bench.rs::page(seed)`;
no `rand`): one `Vec<u8>` of `mem_pages * PAGE_SIZE` bytes, gfn `g` non-zero iff `g % 4 == 0`,
and among non-zero pages every 8th reuses content from an earlier one. Read
`HARMONY_BENCH_PAGES` from the env at test start (default `PROD_MEM_PAGES`); print the
effective shape in every `[BENCH]` line so recorded numbers are self-describing.

Benches (each prints one `[BENCH] name key=value ...` line; median of ≥ 3 iterations where an
iteration is cheap, single run where it is seconds-scale):

1. **`base_seal`** — `begin_base` + `write_page` for **all** frames + `seal` (exactly what
   `snapshot_base` does). Report `total_s`, `us_per_page`, and `owned_pages`. Also time a
   **hash-only** loop (just `blake3::hash` over the same frames, no store) as the same-shape
   baseline, so hash cost vs intern/alloc cost is attributable.
2. **`dirty_delta_seal`** — from the sealed base, `derive` + write **only** N dirty frames +
   `seal`, for N in `{512, 4_096, 32_768, 262_144}` (fresh deterministic contents). This is
   the M2.1 payoff curve. Report `us_per_page` and `total`.
3. **`full_rescan_delta_seal`** — `derive` + write **all** frames of an image where only
   4,096 actually changed + `seal`. This is what a derive-without-dirty-set costs (the
   `dirty: None` fallback path); pairs with bench 2 to show scan-domination.
4. **`materialize_sweep`** — `materialize()` wall time over resident-page counts
   `{4_096, 32_768, 131_072}` × chain depth `{1, 8, 32}` (build depth-k chains by deriving
   k−1 times with small disjoint dirty sets on top of a base holding the resident set).
   Also report two floors alongside: (a) writing the same resolved page set into an
   equally-sized tempfile through a single `memmap2::MmapMut` memcpy loop (the ideal write
   path — this is the M1.2b target), and (b) one full-image `Vec` → `Vec` copy (the
   restore-memcpy floor that M2.2 removes; `copy_from_slice` on `mem_pages * PAGE_SIZE`).
5. **`gc_reap`** — build 64 layers, release all but the tip, time `gc()`.

Record every number (with machine, OS, and effective shape) in a new
"Production-shape bench (task 95)" section of `consonance/snapshot-store/IMPLEMENTATION.md`,
before-vs-after for each M1.2 item.

### M1.2 The optimizations (each gated on its measured delta; all behavior-preserving)

**(a) Zero-page short-circuit in `write_page`.** `BuilderCore::write_page` (`lib.rs`)
currently BLAKE3-hashes every page and compares against the precomputed `zero_hash`. Add,
before hashing: if the page is all zeros (`data.iter().all(|&b| b == 0)` — the compiler
vectorizes this; do not add a dependency), take the `PageRef::Zero` arm directly.
Semantically identical (`blake3(data) == zero_hash` iff `data` is the zero page, to the
collision bound the crate already documents); skips the hash for the majority-zero frames a
booted image is made of. Expected to dominate bench 1's improvement.

**(b) Materialize write path: one mapping instead of 2 syscalls per page.**
In `Store::materialize` (`lib.rs`), replace the per-page `file.seek(..)` + `file.write_all(..)`
loop with a single write-mapping of the already-sized tempfile: map it once, memcpy each
resolved non-zero page to `gfn * PAGE_SIZE`, flush, unmap, then hand the file to
`Mapping::new` exactly as today. Keep the file sparse: never touch offsets of zero/absent
pages. The crate rule "only `mapping.rs` may use `unsafe`" holds: implement the write mapping
as a `pub(crate)` helper in `mapping.rs` (e.g. `Mapping::populate(file: &File, len: u64,
pages: impl Iterator<Item = (u64, &[u8])>) -> io::Result<()>` using `memmap2::MmapMut::map_mut`
with a `// SAFETY:` comment mirroring the existing one — same freshly-created, exclusively-owned
tempfile argument), call `.flush()` before returning, and keep `lib.rs` unsafe-free. Follow the
crate's existing Miri seam treatment for mmap-touching tests (mmap does not run under Miri;
structure as the current `mapping.rs` tests are structured). The resolve side (the
first-writer-wins chain walk into `resolved`) is unchanged.

**(c) The page-content table: hash-keyed, not tree-keyed.** `Store::pages` is
`BTreeMap<PageHash, PageEntry>` under uniformly-random 32-byte BLAKE3 keys — every lookup is a
cache-hostile tree descent, and seal/intern/release are lookup-heavy. Replace with
`std::collections::HashMap<PageHash, PageEntry, BuildPageHashHasher>` where the custom hasher
(no new dependency) XOR-folds the written bytes in 8-byte little-endian chunks — the key is
already a cryptographic hash, so folding is uniform, and XOR-folding is robust to the standard
library's length-prefix `write` pattern for byte arrays. Add the mandatory
`#[allow(clippy::disallowed_types)]` + `// not order-observable:` justification, and this
field-doc guard: **this map is never iterated** (`store_stats` uses `.len()` only; `gc`
iterates `layers`, not `pages`) — any future iteration must collect-and-sort first or it is a
determinism bug. Unit-test the hasher (distinct known keys → working map behavior; insert/
lookup/remove round-trip).

**(d) Optional — page-data arena.** Only if, after (a)–(c), bench 1's gap to the hash-only
baseline shows per-page `Box<[u8]>` allocation still matters: a page-aligned slab with a free
list behind the same `PageEntry` semantics. Do **not** build this speculatively; skipping it
with a one-line justification in `IMPLEMENTATION.md` is the expected outcome.

### M1 acceptance gates

1. Standard suite green on `consonance/snapshot-store` (build / nextest / clippy `-D warnings`
   / fmt / deny), macOS + Linux, plus Miri per the crate's existing configuration. Every
   pre-existing test passes **unmodified**.
2. `bench_production_shape.rs` runs at the default shape on a dev machine
   (`cargo test -p snapshot-store --release --test bench_production_shape -- --ignored
   --nocapture`) and its numbers are recorded in `IMPLEMENTATION.md` (machine noted).
3. Per-optimization before/after table in `IMPLEMENTATION.md`. Directional bar (informational
   numbers, but these directions are pass/fail): (a) improves `base_seal` on the
   quarter-resident image; (b) improves `materialize_sweep` at every resident count, and its
   remaining gap to the mmap-memcpy floor is reported; (c) does not regress any bench.
4. New targeted tests: zero-shortcut equivalence (an explicitly-written zero page still yields
   `owned_pages`/`stored_unique_pages` semantics identical to today's — extend the existing
   `zero_writes_are_never_stored` / `zero_write_over_data_is_owned_but_unstored` pair), and
   the hasher unit test of (c).

## M2 — frontier: O(dirty) capture + remap restore (dispatch after M1 merges)

Implements `docs/INTEGRATION.md` §5's Memory/snapshots disposition. Two independent halves;
land 2.1 before 2.2 (2.1's A/B gate is cheaper to debug without a new restore path in play).

### M2.1 KVM dirty-log harvest → `snapshot_derive`

- **Enable tracking:** in `KvmBackend::map_memory` (`kvm_sys.rs`), set
  `KVM_MEM_LOG_DIRTY_PAGES` in `flags` for the guest-RAM memslots — **both** parts of the
  LAPIC-hole split (`region.rs::split_parts`; an 8 GiB guest is two slots). Plain
  `KVM_GET_DIRTY_LOG` only (it retrieves-and-resets); `KVM_CAP_DIRTY_LOG_RING` and
  `KVM_CLEAR_DIRTY_LOG` manual-protect modes are out of scope.
- **Harvest verb:** a new `Backend` method (e.g. `harvest_dirty_gfns(&mut self) ->
  Result<Vec<u64>, _>`) that issues `KVM_GET_DIRTY_LOG` per RAM slot, decodes the bitmaps,
  translates slot-relative page indices back to absolute gfns via the recorded `MemRegions`
  table, and returns **sorted ascending, deduplicated** gfns (determinism discipline: the
  bitmap order is already deterministic; sorting is the stated contract). The portable
  `MemRegions` side gets the pure bitmap→gfn decode logic (unit-testable on macOS); only the
  ioctl lives behind the KVM backend.
- **Wire into the seal RPC** (`control.rs`): the session tracks
  `derived_from: Option<SnapshotId>` for the open VM — set after a successful seal (the new
  snapshot becomes the parent of the next one) and after a successful branch-restore (the
  branch source is the parent); `None` for a fresh-booted VM. At the moment `derived_from`
  becomes `Some`, harvest-and-discard once to reset the log so the next harvest covers exactly
  the span since that state. Seal then becomes: `derived_from` is `Some(parent)` and `parent`
  is still live in the store → `snapshot_derive(parent, memory, Some(&harvested), vm_state)`;
  otherwise → `snapshot_base` exactly as today.
- **The safety rule (this is the invariant a reviewer checks first):** the dirty set is a
  **cost hint, never a correctness input**. On *any* doubt — harvest ioctl error, tracking not
  armed continuously since the parent state, parent released, any error on the derive path —
  fall back to `dirty: None` (full write; the engine doc already guarantees `dirty: None`
  derive is correct-by-dedup) or to `snapshot_base`. Never fail the seal RPC because the
  optimization was unavailable, and never pass a dirty set you cannot prove complete. A
  KVM over-report (superset) is harmless — seal-time dedup discards no-op writes; an
  under-report is silent snapshot corruption — that asymmetry is why the fallback is always
  the full scan.
- **Bounded chains (supersedes the independent-bases note):** derive chains make
  `materialize` O(chain) where production was O(1-layer) — bounded by M1 bench 4's measured
  depth curve. Add `max_chain_len` to the engine config (default **32**; a config knob, not a
  magic number): if the parent's `chain_len` (from `stats`) is at the limit, seal via
  `snapshot_base` instead (one full-scan flatten, content-dedup makes it cheap in storage).
  Update the `seal_rate_sweep.rs` "Independent `snapshot_base`s" comment to cite this task.

### M2.2 Remap restore: the mapping becomes the memslot backing

- Today (`control.rs` branch verb → `vmm.rs`): materialize → construct a fresh `Vmm` (which
  allocates and populates its own RAM) → `restore_snapshot` memcpys the mapping into that RAM.
  Replace the memcpy: construct the fresh `Vmm` **around the `Mapping`** — the mapping's
  buffer *is* the guest RAM the memslots register (`map_memory` gets
  `mapping.as_mut_slice()`'s pointer, both split parts), and the `Vmm` owns the `Mapping` for
  its lifetime (e.g. `enum RamBacking { Owned(<today's allocation>), Snapshot(Mapping) }` —
  whatever `vmm.rs` currently owns, made a two-variant enum). The restore path must **skip**
  normal boot-time image loading into RAM (the mapping already holds the image); `vm_state`
  restore is unchanged. `MAP_PRIVATE` semantics do the rest: guest writes stay private to this
  VM, the store's pages and the tempfile are never written back, and untouched pages are
  faulted lazily instead of copied eagerly.
- Keep the memcpy path compilable behind a server config flag (`restore_mode: Remap | Memcpy`,
  default `Remap`) — it is the A/B arm of the determinism gate and the fallback if a box gate
  fails.
- **Escalation rule:** if `Vmm` construction order cannot accept an injected backing without
  restructuring beyond this surface (e.g. device setup writes into RAM before the backing
  could be swapped), stop and escalate with the specific constructor sequence — do not force
  it with `unsafe` pointer surgery or post-hoc region rebinding.

### M2 acceptance gates (box, per `docs/BOX-PINNING.md`; portable-logic suite also green)

- **(a0) Tracking is inert:** same seed, dirty logging enabled vs a `flags: 0` build, no seal
  taken → bit-identical `state_hash` at the same stop. (Write-protect faults are host-side
  and must not perturb the guest-observable execution or the Moment count.)
- **(a) Capture A/B:** run to a Moment, seal once via harvested `snapshot_derive` and once via
  `snapshot_base` from the same state (two runs, same seed) → the two snapshots'
  `materialize()`d images are byte-identical and `vm_state` blobs equal.
- **(b) Restore A/B:** branch the same (snapshot, suffix) under `Remap` and `Memcpy` modes →
  identical stop + `state_hash`.
- **(c) Nothing regresses:** `seal_rate_sweep.rs` and the task-68 live materialization gate
  (`live_materialization.rs`) pass unchanged on the new paths.
- **(d) The numbers:** report seal wall time (full-scan vs dirty-set at the campaign's
  typical suffix), restore wall time (memcpy vs remap), and chain-depth distribution under
  `max_chain_len = 32`, in an `docs/history/IMPLEMENTATION-task95.md` at repo root (the task-56/61/69
  pattern), quoting M1's laptop numbers as the store-side baseline.

## Box-safety (CRITICAL — M2 only)

Stock KVM = **1396736**; always leave the box on stock + verified after every run. Kill live
harnesses first (`pkill -9 -f 'live_|seal_rate'` — beware the argv self-match landmine: write
scripts and launch in separate ssh calls, `</dev/null`), wait for `lsmod | grep '^kvm_intel'`
users=0, then `rmmod kvm_intel kvm; modprobe kvm; modprobe kvm_intel`, verify size on a fresh
connection. Pin to the leased core (`taskset`, `scripts/box-window.sh` discipline — acquire
inside the long-lived driver process, never a transient ssh call). Run gates in the foreground
and read results before reporting.

## Prior art

- **Task 08 spike** (in-repo) — measured KVM restore-by-remap vs memcpy on real `/dev/kvm`;
  chose the memslot/CoW-mapping mechanism `INTEGRATION.md` §5 records. M2 implements it.
- **Firecracker diff snapshots** [eng] — dirty-log-harvested delta snapshots in production;
  validates the harvest→delta shape and the superset-tolerance argument.
- **Nyx** (USENIX Sec 2021) [eng] — fast-reload discipline: restore cost proportional to
  dirtied state, not image size; the bar M2.2 aims at.

## Non-goals

- **userfaultfd / demand-paged restore and any remote/central page-store architecture** — the
  logged follow-on direction (a big-memory store node serving pages to thin runs); M1's bench
  and M2's remap seam are deliberately the substrate for it, but nothing network- or
  uffd-shaped lands here. Portability rule 6 stands for M1 (no Linux-only syscalls).
- `KVM_CAP_DIRTY_LOG_RING`, manual-protect / `KVM_CLEAR_DIRTY_LOG` modes, PML tuning.
- Parallel/rayon hashing, page compression, a durable on-disk store format, cross-VM shared
  base files — all post-D5 economics, none needed for the D5 win.
- Task-68 retention-pool or Selector policy changes; the pool decides *which* snapshots to
  keep — this task changes only what keeping and reviving them costs.
- `control-proto` wire changes: the RPC surface and encoded bytes are unchanged; everything
  here is behind the server.
- ARM: all M2 KVM specifics stay behind the `Backend` trait per `docs/ARCH-BOUNDARY.md`; the
  portable decode/bookkeeping halves are ISA-neutral.
