// SPDX-License-Identifier: AGPL-3.0-or-later
//! Live VM snapshot / branch: the wiring that joins `snapshot-store` (the layered
//! copy-on-write guest-memory page store) and `vm-state` (the versioned codec for
//! the non-memory machine blob) to the live VM in [`crate::vmm`].
//!
//! This module is "the elsewhere" both sibling crates defer their KVM side to
//! (their docs say the KVM integration "lives elsewhere"). It holds:
//!
//! - **[`SnapshotEngine`]** — a thin owner of a [`snapshot_store::Store`] that turns
//!   a full guest-memory image + a sealed `vm_state` blob into a content-addressed
//!   snapshot (`begin_base` → `write_page` per frame → `seal`), derives later
//!   snapshots from the pages dirtied since a parent (`derive`), and materializes a
//!   snapshot back into a private CoW [`Mapping`]. Capture is **dirty-set-
//!   proportional**: the store discards a written page whose content already
//!   resolves through the parent chain, so a derived snapshot's `owned_pages` counts
//!   only genuinely-changed frames, and identical page contents are stored **once
//!   store-wide** — so N VMs forked from one boot share a single resident base.
//!
//! The **record set** a snapshot carries is per-vendor, so it lives with the
//! vendor ([`crate::vendor::x86::records`]): the conversions between the live
//! machine's register file and `vm-state`'s plain-data records, plus the
//! vmm-core-owned device blob (the `vm_state::DeviceBlob` payload). The engine
//! here owns the memory half and the opaque blob container, and never interprets
//! a record.
//!
//! The KVM-specific mechanics this builds on — the dirty-log harvest that yields the
//! per-snapshot dirty set, and the memslot remap that makes restore O(dirty) rather
//! than O(image) — live **below the `Backend` trait** in `vmm-backend` (task 08's
//! measured mechanism); see `IMPLEMENTATION.md`. The engine here is portable and
//! Mac/Miri-testable against plain memory, exactly as `snapshot-store` is.

use snapshot_store::{Mapping, PAGE_SIZE, SnapStats, SnapshotId, Store, StoreConfig, StoreStats};
use vm_state::VmState;

/// Errors from the snapshot/branch path: a store failure, a `vm_state` codec
/// failure, a malformed vmm-core device blob, a guest-image size mismatch, a
/// LAPIC restore rejection, or a snapshot taken under a different CPU/MSR contract.
#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    /// An underlying [`snapshot_store::Store`] operation failed.
    #[error("snapshot-store error")]
    Store(#[from] snapshot_store::StoreError),
    /// The `vm_state` blob failed to encode or decode (strict, total codec).
    #[error("vm_state codec error")]
    Codec(#[from] vm_state::VmStateError),
    /// A guest-memory image's length is not the configured image size.
    #[error("guest image is {got} bytes, expected {expected} ({pages} pages × {PAGE_SIZE})")]
    MemorySize {
        /// The offending image length in bytes.
        got: usize,
        /// The configured image length in bytes (`pages * PAGE_SIZE`).
        expected: usize,
        /// The configured image size in pages.
        pages: u64,
    },
    /// The vmm-core-owned device blob (inside `vm_state::DeviceBlob`) was malformed
    /// — truncated, a bad magic/version, or an out-of-range field. Total, never a
    /// panic (Convention rule #4).
    #[error("device blob malformed: {0}")]
    DeviceBlob(&'static str),
    /// A harvested dirty-page gfn lies outside the configured guest image.
    #[error("dirty gfn {gfn} out of range: guest image is {pages} pages")]
    DirtyGfnOutOfRange {
        /// The offending guest frame number.
        gfn: u64,
        /// The configured guest image size in pages.
        pages: u64,
    },
    /// The userspace xAPIC rejected a restored [`LapicState`].
    #[error("lapic restore rejected: {0}")]
    Lapic(&'static str),
    /// The snapshot was taken under a different ratified CPU/MSR contract than the
    /// one this VMM enforces, so its CPUID/MSR behavior would silently diverge on
    /// restore. Refused loudly (INTEGRATION.md §4 `contract_hash`).
    #[error("contract hash mismatch: snapshot taken under a different CPU/MSR contract")]
    ContractMismatch,
}

/// The live-VM snapshot / branch engine: a [`snapshot_store::Store`] sized to the
/// guest image, plus the page count.
///
/// One engine backs a whole exploration tree: a single base layer holds the booted
/// image, every later snapshot records only its dirtied pages, and identical page
/// contents are interned once store-wide so N branches from one boot do not cost N
/// copies. `vm_state` blobs are sealed verbatim (the canonical `vm_state::VmState`
/// encoding), opaque to the store.
pub struct SnapshotEngine {
    store: Store,
    mem_pages: u64,
    max_chain_len: u32,
}

/// Default [`SnapshotEngine::max_chain_len`]: `materialize` is O(chain), so a
/// dirty-log derive chain (task 95 M2.1) is bounded — at this depth a seal
/// flattens via `snapshot_base` instead (one full scan; content-dedup keeps the
/// storage cost near zero). 32 sits well below the flat region of the M1
/// depth-sweep (materialize was depth-flat at 1/8/32 on the bench machine).
pub const DEFAULT_MAX_CHAIN_LEN: u32 = 32;

impl SnapshotEngine {
    /// Create an engine for guest images of `mem_bytes` bytes. `mem_bytes` must be a
    /// non-zero multiple of [`PAGE_SIZE`]; otherwise the engine still works but the
    /// final partial page is simply never addressable.
    pub fn new(mem_bytes: usize) -> SnapshotEngine {
        let mem_pages = (mem_bytes / PAGE_SIZE) as u64;
        SnapshotEngine {
            store: Store::new(StoreConfig { mem_pages }),
            mem_pages,
            max_chain_len: DEFAULT_MAX_CHAIN_LEN,
        }
    }

    /// The configured guest image size in pages.
    pub fn mem_pages(&self) -> u64 {
        self.mem_pages
    }

    /// The configured derive-chain bound (task 95 M2.1): a capture whose parent
    /// already has `chain_len >= max_chain_len` must seal as a fresh base (one
    /// flattening full scan) instead of deriving deeper — keeping `materialize`
    /// O(bounded chain). Default [`DEFAULT_MAX_CHAIN_LEN`].
    pub fn max_chain_len(&self) -> u32 {
        self.max_chain_len
    }

    /// Override the derive-chain bound (a config knob, not a magic number).
    /// `0` disables deriving entirely (every seal is a base).
    pub fn set_max_chain_len(&mut self, max_chain_len: u32) {
        self.max_chain_len = max_chain_len;
    }

    /// Read-only access to the underlying store (for `store_stats` / `stats`).
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Store-wide statistics — the **N-VMs-share-one-base** evidence: a base plus N
    /// derived snapshots that touched nothing keep `stored_unique_pages` at the base's
    /// distinct-content count, never `N ×` it (gate 3).
    pub fn store_stats(&self) -> StoreStats {
        self.store.store_stats()
    }

    /// Per-snapshot statistics (`owned_pages` = pages this layer provides that no
    /// ancestor provides identically — the dirty set actually retained).
    pub fn stats(&self, snap: SnapshotId) -> Result<SnapStats, SnapshotError> {
        Ok(self.store.stats(snap)?)
    }

    /// Build the **base** layer from a full guest-memory image and a sealed blob:
    /// `begin_base` → `write_page` per guest frame → `seal(vm_state)`. Pages whose
    /// content is the all-zero page cost nothing (sparse images are free).
    ///
    /// `vm_state` is the canonical [`vm_state::VmState::encode`] bytes (opaque to the
    /// store). `memory` must be exactly `mem_pages * PAGE_SIZE` bytes.
    pub fn snapshot_base(
        &mut self,
        memory: &[u8],
        vm_state: &[u8],
    ) -> Result<SnapshotId, SnapshotError> {
        self.check_image_len(memory)?;
        let mut builder = self.store.begin_base();
        for (gfn, frame) in memory.chunks_exact(PAGE_SIZE).enumerate() {
            builder.write_page(gfn as u64, frame)?;
        }
        Ok(builder.seal(vm_state.to_vec()))
    }

    /// Derive a child snapshot of `parent` from the current full image.
    ///
    /// When `dirty` is `Some(gfns)`, only those frames are written — the **dirty-set-
    /// proportional** path the KVM dirty-log harvest feeds (each later snapshot pays
    /// only for what changed). When `dirty` is `None`, every frame is written and the
    /// store's seal-time dedup keeps the result equally cheap (a frame whose content
    /// already resolves through the parent chain is discarded), so capture is correct
    /// even without a harvested dirty set — only the capture *cost* differs.
    pub fn snapshot_derive(
        &mut self,
        parent: SnapshotId,
        memory: &[u8],
        dirty: Option<&[u64]>,
        vm_state: &[u8],
    ) -> Result<SnapshotId, SnapshotError> {
        self.check_image_len(memory)?;
        let mut builder = self.store.derive(parent)?;
        match dirty {
            Some(gfns) => {
                for &gfn in gfns {
                    if gfn >= self.mem_pages {
                        return Err(SnapshotError::DirtyGfnOutOfRange {
                            gfn,
                            pages: self.mem_pages,
                        });
                    }
                    // gfn < mem_pages and the image length was checked == mem_pages *
                    // PAGE_SIZE, so this frame is always fully in range (no panic).
                    let off = gfn as usize * PAGE_SIZE;
                    builder.write_page(gfn, &memory[off..off + PAGE_SIZE])?;
                }
            }
            None => {
                for (gfn, frame) in memory.chunks_exact(PAGE_SIZE).enumerate() {
                    builder.write_page(gfn as u64, frame)?;
                }
            }
        }
        Ok(builder.seal(vm_state.to_vec()))
    }

    /// Materialize `snap`'s full logical image as a private copy-on-write
    /// [`Mapping`] — the host backing the restore points the KVM memslot at (the
    /// remap mechanism task 08 chose; below the trait). Resolving the chain is
    /// O(chain) per gfn, memoized; only non-zero pages touch the sparse tempfile.
    pub fn materialize(&self, snap: SnapshotId) -> Result<Mapping, SnapshotError> {
        Ok(self.store.materialize(snap)?)
    }

    /// Decode the sealed `vm_state` blob of `snap` back into a [`VmState`].
    pub fn vm_state(&self, snap: SnapshotId) -> Result<VmState, SnapshotError> {
        Ok(VmState::decode(self.store.vm_state(snap)?)?)
    }

    /// Increment `snap`'s refcount (an explorer holding a fork alive). See
    /// [`snapshot_store::Store::retain`].
    pub fn retain(&mut self, snap: SnapshotId) -> Result<(), SnapshotError> {
        Ok(self.store.retain(snap)?)
    }

    /// Decrement `snap`'s refcount. See [`snapshot_store::Store::release`].
    pub fn release(&mut self, snap: SnapshotId) -> Result<(), SnapshotError> {
        Ok(self.store.release(snap)?)
    }

    /// Reap layers unreachable from any live snapshot; returns bytes freed.
    pub fn gc(&mut self) -> u64 {
        self.store.gc()
    }

    fn check_image_len(&self, memory: &[u8]) -> Result<(), SnapshotError> {
        let expected = (self.mem_pages as usize).saturating_mul(PAGE_SIZE);
        if memory.len() != expected {
            return Err(SnapshotError::MemorySize {
                got: memory.len(),
                expected,
                pages: self.mem_pages,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- engine: base / derive / sharing ------------------------------------

    const PG: usize = PAGE_SIZE;

    fn img(pages: &[(usize, u8)], total_pages: usize) -> Vec<u8> {
        let mut m = vec![0u8; total_pages * PG];
        for &(gfn, byte) in pages {
            m[gfn * PG..(gfn + 1) * PG].fill(byte);
        }
        m
    }

    #[test]
    fn base_then_derive_stores_only_dirtied_pages() {
        let mut eng = SnapshotEngine::new(8 * PG);
        let base_mem = img(&[(0, 0xA), (1, 0xB), (5, 0xC)], 8);
        let base = eng.snapshot_base(&base_mem, b"base-blob").unwrap();
        assert_eq!(eng.stats(base).unwrap().owned_pages, 3);
        assert_eq!(eng.store_stats().stored_unique_pages, 3);

        // Dirty only page 1; the derive (full image, no dirty hint) must store ONE
        // owned page (the store's seal-time dedup drops the unchanged frames).
        let mut child_mem = base_mem.clone();
        child_mem[PG..2 * PG].fill(0xFF);
        let child = eng
            .snapshot_derive(base, &child_mem, None, b"child-blob")
            .unwrap();
        assert_eq!(
            eng.stats(child).unwrap().owned_pages,
            1,
            "derive is dirty-set-proportional even without a harvested dirty set"
        );
        // Store-wide: the 3 base contents + the 1 new content = 4 (page 1's old
        // 0xB is still referenced by the base).
        assert_eq!(eng.store_stats().stored_unique_pages, 4);
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "materialize uses mmap, which Miri cannot execute; the parse/convert logic is covered by the non-mmap tests"
    )]
    fn derive_with_dirty_hint_matches_full_capture() {
        let mut eng = SnapshotEngine::new(8 * PG);
        let base_mem = img(&[(0, 0xA), (3, 0xB)], 8);
        let base = eng.snapshot_base(&base_mem, b"b").unwrap();
        let mut mem = base_mem.clone();
        mem[3 * PG..4 * PG].fill(0x99);
        mem[7 * PG..8 * PG].fill(0x77);
        // Harvested dirty set {3, 7}: capture only those frames.
        let child = eng
            .snapshot_derive(base, &mem, Some(&[3, 7]), b"c")
            .unwrap();
        assert_eq!(eng.stats(child).unwrap().owned_pages, 2);
        // Materialize and confirm the dirtied frames read back the new content and
        // an untouched frame reads the base.
        let map = eng.materialize(child).unwrap();
        assert_eq!(map.as_slice()[3 * PG], 0x99);
        assert_eq!(map.as_slice()[7 * PG], 0x77);
        assert_eq!(map.as_slice()[0], 0xA);
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "materialize uses mmap, which Miri cannot execute; the parse/convert logic is covered by the non-mmap tests"
    )]
    fn n_views_share_one_read_only_base() {
        // Gate 3: materialize N independent CoW views from one base; the base's
        // distinct contents are stored ONCE store-wide, not N×.
        let mut eng = SnapshotEngine::new(64 * PG);
        // 40 pages with DISTINCT non-zero content (byte i+1), so each is a distinct
        // store-wide content address (no incidental dedup masking the sharing claim).
        let base_mem = img(&(0..40).map(|i| (i, (i as u8) + 1)).collect::<Vec<_>>(), 64);
        let base = eng.snapshot_base(&base_mem, b"boot").unwrap();
        let unique_after_base = eng.store_stats().stored_unique_pages;
        assert_eq!(unique_after_base, 40);

        // Eight branches that each touch nothing: pure shared base.
        let mut views = Vec::new();
        for _ in 0..8 {
            let v = eng
                .snapshot_derive(base, &base_mem, Some(&[]), b"branch")
                .unwrap();
            views.push(eng.materialize(v).unwrap());
        }
        assert_eq!(
            eng.store_stats().stored_unique_pages,
            unique_after_base,
            "N branches that touched nothing add NO unique pages — the base is shared"
        );
        // Every view sees the same base image.
        for v in &views {
            assert_eq!(v.as_slice()[0], base_mem[0]);
            assert_eq!(v.as_slice()[39 * PG], base_mem[39 * PG]);
        }
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "materialize uses mmap, which Miri cannot execute; the parse/convert logic is covered by the non-mmap tests"
    )]
    fn materialize_reproduces_the_full_image() {
        let mut eng = SnapshotEngine::new(16 * PG);
        let mem = img(&[(0, 0x11), (8, 0x22), (15, 0x33)], 16);
        let base = eng.snapshot_base(&mem, b"x").unwrap();
        let map = eng.materialize(base).unwrap();
        assert_eq!(map.as_slice(), &mem[..]);
    }

    #[test]
    fn vm_state_blob_seals_and_decodes() {
        // The engine seals the canonical vm_state bytes and hands them back to decode.
        let mut eng = SnapshotEngine::new(4 * PG);
        let mut s = VmState {
            contract_hash: [7u8; 32],
            ..Default::default()
        };
        s.vtime.ratio_den = 1; // encodable
        let bytes = s.encode().unwrap();
        let snap = eng.snapshot_base(&vec![0u8; 4 * PG], &bytes).unwrap();
        assert_eq!(eng.vm_state(snap).unwrap(), s);
    }

    #[test]
    fn wrong_image_length_is_rejected() {
        let mut eng = SnapshotEngine::new(4 * PG);
        assert!(matches!(
            eng.snapshot_base(&vec![0u8; 3 * PG], b""),
            Err(SnapshotError::MemorySize { .. })
        ));
    }

    #[test]
    fn engine_mem_pages_retain_release_gc() {
        let mut eng = SnapshotEngine::new(8 * PG);
        assert_eq!(eng.mem_pages(), 8); // exact: kills mem_pages -> 0 / 1

        // One non-zero page + a non-empty blob, so gc has bytes to free.
        let mut mem = vec![0u8; 8 * PG];
        mem[..PG].fill(0xAB);
        let base = eng.snapshot_base(&mem, b"blob").unwrap(); // refcount 1
        assert_eq!(eng.store_stats().snapshots, 1);

        // retain → refcount 2; one release → still live (kills retain -> Ok(())).
        eng.retain(base).unwrap();
        eng.release(base).unwrap();
        assert_eq!(
            eng.store_stats().snapshots,
            1,
            "retain must have taken effect: one release of two refs leaves it live"
        );
        // Second release → refcount 0 (kills release -> Ok(())).
        eng.release(base).unwrap();
        assert_eq!(eng.store_stats().snapshots, 0, "released after both refs");

        // gc reaps the dead layer, freeing the one stored page + the 4-byte blob.
        // The exact value kills gc -> 0 and gc -> 1.
        assert_eq!(eng.gc(), PAGE_SIZE as u64 + 4);
    }

    #[test]
    fn out_of_range_dirty_gfn_is_rejected() {
        let mut eng = SnapshotEngine::new(4 * PG);
        let mem = vec![0u8; 4 * PG];
        let base = eng.snapshot_base(&mem, b"").unwrap();
        // gfn 4 is one past the 4-page (gfns 0..=3) image.
        assert!(matches!(
            eng.snapshot_derive(base, &mem, Some(&[4]), b""),
            Err(SnapshotError::DirtyGfnOutOfRange { gfn: 4, pages: 4 })
        ));
        // The in-range boundary gfn 3 is accepted.
        assert!(eng.snapshot_derive(base, &mem, Some(&[3]), b"").is_ok());
    }
}
