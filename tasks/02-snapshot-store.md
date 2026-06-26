# Task 02 — `consonance/snapshot-store`: layered copy-on-write guest memory store

Read `tasks/00-CONVENTIONS.md` first. Touch only `consonance/snapshot-store/`.

## Environment

Runs on: macOS and Linux. Requires: Rust only. Does not require: `/dev/kvm`, Intel CPU,
QEMU, root.

## Context

The hypervisor explores execution by snapshotting a running VM thousands of times per run and
branching from interesting states. A snapshot is cheap because it stores only **pages dirtied
since the parent snapshot** plus a small opaque blob of vCPU/device state; full guest memory
is reconstructed by resolving down a layer chain, and a large read-only base image (the booted
guest, ~GBs) is shared by every VM on the machine. This crate is that storage engine, built
and tested standalone against plain memory — the KVM integration (dirty-page harvesting,
EPT/memslot remapping) comes later and is not part of this task.

Design targets: snapshot cost O(dirty pages); page read cost O(chain length) worst case with
an index making the common case O(1); identical page contents stored once store-wide.

## Public API

std crate. `unsafe` is permitted **only** inside the mmap-backed `Mapping` implementation
(via `memmap2`), with `// SAFETY:` comments.

```rust
pub const PAGE_SIZE: usize = 4096;

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)] pub struct SnapshotId(u64); // opaque, monotonic
pub struct StoreConfig { pub mem_pages: u64 /* guest memory size in pages */ }

pub struct Store { /* ... */ }
impl Store {
    pub fn new(cfg: StoreConfig) -> Store;

    /// Build the base layer. Pages not written before seal() are implicitly zero.
    pub fn begin_base(&mut self) -> BaseBuilder<'_>;

    /// Begin a child snapshot of `parent`. Errors if parent unknown or unsealed.
    pub fn derive(&mut self, parent: SnapshotId) -> Result<DeltaBuilder<'_>, StoreError>;

    /// Read one page of `snap`'s logical memory image into `out` (len PAGE_SIZE),
    /// resolving through the layer chain; zero page if never written.
    pub fn read_page(&self, snap: SnapshotId, gfn: u64, out: &mut [u8]) -> Result<(), StoreError>;

    /// The opaque vCPU/device blob recorded at seal time.
    pub fn vm_state(&self, snap: SnapshotId) -> Result<&[u8], StoreError>;

    /// Materialize the full logical image as a private copy-on-write mapping
    /// (a copy-on-write map over an internally maintained flat tempfile, or an
    /// equivalent PORTABLE mechanism — must work on macOS and Linux, so no memfd;
    /// see docs/BUILDING.md).
    /// The mapping is mutable; writes never affect the store.
    pub fn materialize(&self, snap: SnapshotId) -> Result<Mapping, StoreError>;

    /// Reference counting: snapshots start with refcount 1 (held by creator).
    pub fn retain(&mut self, snap: SnapshotId) -> Result<(), StoreError>;
    pub fn release(&mut self, snap: SnapshotId) -> Result<(), StoreError>;
    /// Drop layers unreachable from any live (refcount > 0) snapshot or its ancestors.
    /// Returns bytes freed.
    pub fn gc(&mut self) -> u64;

    pub fn stats(&self, snap: SnapshotId) -> Result<SnapStats, StoreError>;
    pub fn store_stats(&self) -> StoreStats;
}

pub struct BaseBuilder<'a> { /* write_page(gfn, &[u8]) -> Result<...>; seal(vm_state: Vec<u8>) -> SnapshotId */ }
pub struct DeltaBuilder<'a> { /* same surface as BaseBuilder */ }

pub struct Mapping { /* as_slice(&self) -> &[u8]; as_mut_slice(&mut self) -> &mut [u8]; len() */ }

pub struct SnapStats  { pub logical_pages: u64, pub owned_pages: u64, pub chain_len: u32 }
pub struct StoreStats { pub snapshots: u64, pub stored_unique_pages: u64,
                        pub logical_pages_total: u64, pub bytes_resident: u64 }

pub enum StoreError { /* unknown snapshot, gfn out of range, bad page length,
                         builder misuse, io errors via thiserror */ }
```

Semantics that must hold:

- **Immutability**: once sealed, a snapshot's logical image never changes, regardless of any
  later snapshots, gc, dedup, or materialized-mapping writes.
- **Chain resolution**: `read_page` returns the page from the nearest ancestor (including
  self) that wrote it, else zeros.
- **Dedup**: pages with identical content are stored once store-wide (content-addressed by
  `blake3` hash; document the theoretical collision stance in a comment). `owned_pages`
  counts pages a layer references that no ancestor layer provides identically;
  `stored_unique_pages` counts distinct page contents resident.
- **gc correctness**: gc never frees data reachable from a live snapshot's chain; releasing
  all snapshots then gc'ing frees everything except internal fixed overhead.
- Builders enforce single use: writing after `seal`, or sealing twice, is a compile-time or
  runtime error (consuming `self` in `seal` is the preferred design).

## Acceptance gates

Beyond the standard gates:

1. **Oracle property test** (the core gate): maintain a naive model — `Vec<u8>` full image
   per snapshot — alongside the store. Drive both with arbitrary operation sequences
   (proptest): create base with random sparse pages, derive chains and trees (parents chosen
   randomly among sealed snapshots), random page writes in builders, interleaved
   retain/release/gc, random `read_page` and full `materialize` comparisons. Assert
   byte-equality between store and model at every read. Include deep chains (≥ 64) and wide
   fan-out (≥ 32 children of one parent).
2. **Dedup test**: a base of N distinct pages, plus 10 children each rewriting the same pages
   with identical content ⇒ `stored_unique_pages` stays N and children's `owned_pages` == 0.
3. **Zero-page test**: never-written pages read as zeros at every chain depth and in
   `materialize`; sparse 1 GiB-logical base with 10 written pages materializes without
   allocating ~1 GiB of resident memory (assert via `bytes_resident`).
4. **Mapping CoW test**: write to a materialized mapping, then re-read the same pages via
   `read_page` and a fresh `materialize` ⇒ original content intact.
5. **gc test**: build a chain A→B→C, release B (A and C still live) ⇒ gc frees nothing C
   needs (verify C still reads correctly); release C, gc ⇒ stats shrink accordingly.
6. **Bench (informational, not a pass/fail gate)**: `cargo bench` or an ignored test printing:
   seal time for a 1 000-dirty-page delta, and `read_page` throughput at chain depth 64.
   Record numbers in `IMPLEMENTATION.md`.

## Non-goals

KVM/memslot/EPT anything; dirty-page *tracking* (the caller hands you dirty pages); chain
compaction/flattening; persistence across process restarts (in-memory + tempfile is fine);
compression; concurrency (single-threaded API; `Store` need not be `Sync`).
