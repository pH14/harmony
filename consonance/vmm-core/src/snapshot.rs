// SPDX-License-Identifier: AGPL-3.0-or-later

use snapshot_store::{Mapping, PAGE_SIZE, SnapStats, SnapshotId, Store, StoreConfig, StoreStats};
use vm_state::SnapshotRecords;

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("snapshot-store error")]
    Store(#[from] snapshot_store::StoreError),
    #[error("vm_state codec error")]
    Codec(#[from] vm_state::VmStateError),
    #[error("guest image is {got} bytes, expected {expected} ({pages} pages × {PAGE_SIZE})")]
    MemorySize {
        got: usize,
        expected: usize,
        pages: u64,
    },
    #[error("device blob malformed: {0}")]
    DeviceBlob(&'static str),
    #[error("engine state malformed: {0}")]
    EngineState(&'static str),
    #[error("dirty gfn {gfn} out of range: guest image is {pages} pages")]
    DirtyGfnOutOfRange { gfn: u64, pages: u64 },
    #[error("sparse snapshot pages are not sorted: {previous} then {current}")]
    SparsePagesNotSorted { previous: u64, current: u64 },
    #[error("sparse snapshot page {gfn} is duplicated")]
    SparsePageDuplicate { gfn: u64 },
    #[error("sparse snapshot page {gfn} out of range: guest image is {pages} pages")]
    SparsePageOutOfRange { gfn: u64, pages: u64 },
    #[error("device restore rejected: {0}")]
    DeviceRestore(&'static str),
    #[error("contract hash mismatch: snapshot taken under a different CPU/MSR contract")]
    ContractMismatch,
}

pub struct SnapshotEngine {
    store: Store,
    mem_pages: u64,
    max_chain_len: u32,
}

pub const DEFAULT_MAX_CHAIN_LEN: u32 = 32;

impl SnapshotEngine {
    pub fn new(mem_bytes: usize) -> SnapshotEngine {
        let mem_pages = (mem_bytes / PAGE_SIZE) as u64;
        SnapshotEngine {
            store: Store::new(StoreConfig { mem_pages }),
            mem_pages,
            max_chain_len: DEFAULT_MAX_CHAIN_LEN,
        }
    }

    pub fn mem_pages(&self) -> u64 {
        self.mem_pages
    }

    pub fn max_chain_len(&self) -> u32 {
        self.max_chain_len
    }

    pub fn set_max_chain_len(&mut self, max_chain_len: u32) {
        self.max_chain_len = max_chain_len;
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn store_stats(&self) -> StoreStats {
        self.store.store_stats()
    }

    pub fn stats(&self, snap: SnapshotId) -> Result<SnapStats, SnapshotError> {
        Ok(self.store.stats(snap)?)
    }

    pub fn snapshot_base(
        &mut self,
        memory: &[u8],
        vm_state: &[u8],
    ) -> Result<SnapshotId, SnapshotError> {
        self.check_image_len(memory)?;
        let mut builder = self.store.begin_base();
        for (gfn, frame) in memory.as_chunks::<PAGE_SIZE>().0.iter().enumerate() {
            builder.write_page(gfn as u64, frame)?;
        }
        Ok(builder.seal(vm_state.to_vec()))
    }

    pub fn snapshot_flatten(
        &mut self,
        parent: SnapshotId,
        memory: &[u8],
        dirty: &[u64],
        vm_state: &[u8],
    ) -> Result<SnapshotId, SnapshotError> {
        self.check_image_len(memory)?;
        Ok(self
            .store
            .flatten_base(parent, memory, dirty, vm_state.to_vec())?)
    }

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
                    let off = gfn as usize * PAGE_SIZE;
                    builder.write_page(gfn, &memory[off..off + PAGE_SIZE])?;
                }
            }
            None => {
                for (gfn, frame) in memory.as_chunks::<PAGE_SIZE>().0.iter().enumerate() {
                    builder.write_page(gfn as u64, frame)?;
                }
            }
        }
        Ok(builder.seal(vm_state.to_vec()))
    }

    pub fn snapshot_sparse_derive(
        &mut self,
        parent: SnapshotId,
        pages: &[(u64, [u8; PAGE_SIZE])],
        vm_state: &[u8],
    ) -> Result<SnapshotId, SnapshotError> {
        let mut previous = None;
        for &(gfn, _) in pages {
            if gfn >= self.mem_pages {
                return Err(SnapshotError::SparsePageOutOfRange {
                    gfn,
                    pages: self.mem_pages,
                });
            }
            if let Some(prev) = previous {
                match gfn.cmp(&prev) {
                    std::cmp::Ordering::Equal => {
                        return Err(SnapshotError::SparsePageDuplicate { gfn });
                    }
                    std::cmp::Ordering::Less => {
                        return Err(SnapshotError::SparsePagesNotSorted {
                            previous: prev,
                            current: gfn,
                        });
                    }
                    std::cmp::Ordering::Greater => {}
                }
            }
            previous = Some(gfn);
        }

        let mut builder = self.store.derive(parent)?;
        for &(gfn, page) in pages {
            builder.write_page(gfn, &page)?;
        }
        Ok(builder.seal(vm_state.to_vec()))
    }

    pub fn materialize(&self, snap: SnapshotId) -> Result<Mapping, SnapshotError> {
        Ok(self.store.materialize(snap)?)
    }

    pub fn diff_pages(
        &self,
        from: Option<SnapshotId>,
        to: SnapshotId,
    ) -> Result<Vec<(u64, [u8; PAGE_SIZE])>, SnapshotError> {
        Ok(self.store.diff_pages(from, to)?)
    }

    pub fn read_page(&self, snap: SnapshotId, gfn: u64) -> Result<[u8; PAGE_SIZE], SnapshotError> {
        let mut page = [0u8; PAGE_SIZE];
        self.store.read_page(snap, gfn, &mut page)?;
        Ok(page)
    }

    pub fn vm_state<S: SnapshotRecords>(&self, snap: SnapshotId) -> Result<S, SnapshotError> {
        Ok(S::decode(self.store.vm_state(snap)?)?)
    }

    pub fn vm_state_bytes(&self, snap: SnapshotId) -> Result<&[u8], SnapshotError> {
        Ok(self.store.vm_state(snap)?)
    }

    pub fn retain(&mut self, snap: SnapshotId) -> Result<(), SnapshotError> {
        Ok(self.store.retain(snap)?)
    }

    pub fn release(&mut self, snap: SnapshotId) -> Result<(), SnapshotError> {
        Ok(self.store.release(snap)?)
    }

    pub fn gc(&mut self) -> u64 {
        self.store.gc()
    }

    #[cfg(test)]
    pub(crate) fn corrupt_page_for_test(
        &mut self,
        snap: SnapshotId,
        gfn: u64,
        byte: usize,
        mask: u8,
    ) -> Result<(), SnapshotError> {
        Ok(self.store.corrupt_page_for_test(snap, gfn, byte, mask)?)
    }

    #[cfg(test)]
    pub(crate) fn corrupt_vm_state_for_test(
        &mut self,
        snap: SnapshotId,
        byte: usize,
        mask: u8,
    ) -> Result<(), SnapshotError> {
        Ok(self.store.corrupt_vm_state_for_test(snap, byte, mask)?)
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
    use snapshot_store::StoreError;
    use vm_state::Arm64VmState;
    use vm_state::VmState;

    use super::*;

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

        let mut child_mem = base_mem.clone();
        child_mem[PG..2 * PG].fill(0xFF);
        let child = eng
            .snapshot_derive(base, &child_mem, None, b"child-blob")
            .unwrap();
        assert_eq!(
            eng.stats(child).unwrap().owned_pages,
            1,
            "derive is dirty-set-proportional even without a drained dirty set"
        );
        assert_eq!(eng.store_stats().stored_unique_pages, 4);
    }

    #[test]
    fn diff_pages_exposes_target_resolved_branch_image() {
        let mut eng = SnapshotEngine::new(4 * PG);
        let base_mem = img(&[(0, 0x10)], 4);
        let base = eng.snapshot_base(&base_mem, b"base").unwrap();
        let mut child_mem = base_mem.clone();
        child_mem[PG..2 * PG].fill(0x20);
        let child = eng
            .snapshot_derive(base, &child_mem, Some(&[1]), b"child")
            .unwrap();

        assert_eq!(
            eng.diff_pages(Some(child), base).unwrap(),
            vec![(1, [0u8; PAGE_SIZE])]
        );
        assert_eq!(
            eng.diff_pages(None, child).unwrap(),
            vec![
                (0, [0x10u8; PAGE_SIZE]),
                (1, [0x20u8; PAGE_SIZE]),
                (2, [0u8; PAGE_SIZE]),
                (3, [0u8; PAGE_SIZE]),
            ]
        );
    }

    #[test]
    fn sparse_derive_is_sorted_atomic_and_parent_relative() {
        let mut eng = SnapshotEngine::new(4 * PG);
        let base = eng
            .snapshot_base(&img(&[(0, 0x10), (2, 0x20)], 4), b"base")
            .unwrap();
        let pages = vec![(1, [0x30; PAGE_SIZE]), (3, [0x40; PAGE_SIZE])];
        let child = eng.snapshot_sparse_derive(base, &pages, b"child").unwrap();
        assert_eq!(eng.stats(child).unwrap().owned_pages, 2);
        assert_eq!(
            eng.diff_pages(Some(base), child).unwrap(),
            pages,
            "sparse derive preserves target-resolved pages"
        );

        let before = eng.store_stats();
        assert!(matches!(
            eng.snapshot_sparse_derive(
                base,
                &[(2, [0xAA; PAGE_SIZE]), (1, [0xBB; PAGE_SIZE])],
                b"bad"
            ),
            Err(SnapshotError::SparsePagesNotSorted {
                previous: 2,
                current: 1
            })
        ));
        assert_eq!(
            eng.store_stats(),
            before,
            "invalid sparse input does not mutate the store"
        );
        assert!(matches!(
            eng.snapshot_sparse_derive(
                base,
                &[(1, [0xAA; PAGE_SIZE]), (1, [0xBB; PAGE_SIZE])],
                b"bad"
            ),
            Err(SnapshotError::SparsePageDuplicate { gfn: 1 })
        ));
        assert!(matches!(
            eng.snapshot_sparse_derive(base, &[(4, [0xAA; PAGE_SIZE])], b"bad"),
            Err(SnapshotError::SparsePageOutOfRange { gfn: 4, pages: 4 })
        ));
        assert_eq!(eng.store_stats(), before);
    }

    #[test]
    fn read_page_resolves_content_and_rejects_invalid_inputs() {
        let mut eng = SnapshotEngine::new(2 * PG);
        let base = eng.snapshot_base(&img(&[(1, 0xAB)], 2), b"base").unwrap();

        assert_eq!(eng.read_page(base, 1).unwrap(), [0xAB; PAGE_SIZE]);
        assert!(matches!(
            eng.read_page(base, 2),
            Err(SnapshotError::Store(StoreError::GfnOutOfRange {
                gfn: 2,
                mem_pages: 2
            }))
        ));

        eng.release(base).unwrap();
        assert!(matches!(
            eng.read_page(base, 0),
            Err(SnapshotError::Store(StoreError::UnknownSnapshot(id))) if id == base
        ));
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
        let child = eng
            .snapshot_derive(base, &mem, Some(&[3, 7]), b"c")
            .unwrap();
        assert_eq!(eng.stats(child).unwrap().owned_pages, 2);
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
        let mut eng = SnapshotEngine::new(64 * PG);
        let base_mem = img(&(0..40).map(|i| (i, (i as u8) + 1)).collect::<Vec<_>>(), 64);
        let base = eng.snapshot_base(&base_mem, b"boot").unwrap();
        let unique_after_base = eng.store_stats().stored_unique_pages;
        assert_eq!(unique_after_base, 40);

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
        let mut eng = SnapshotEngine::new(4 * PG);
        let s = VmState {
            contract_hash: [7u8; 32],
            ..Default::default()
        };
        let bytes = s.encode().unwrap();
        let snap = eng.snapshot_base(&vec![0u8; 4 * PG], &bytes).unwrap();
        assert_eq!(eng.vm_state::<VmState>(snap).unwrap(), s);
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
        assert_eq!(eng.mem_pages(), 8);

        let mut mem = vec![0u8; 8 * PG];
        mem[..PG].fill(0xAB);
        let base = eng.snapshot_base(&mem, b"blob").unwrap();
        assert_eq!(eng.store_stats().snapshots, 1);

        eng.retain(base).unwrap();
        eng.release(base).unwrap();
        assert_eq!(
            eng.store_stats().snapshots,
            1,
            "retain must have taken effect: one release of two refs leaves it live"
        );
        eng.release(base).unwrap();
        assert_eq!(eng.store_stats().snapshots, 0, "released after both refs");

        assert_eq!(eng.gc(), PAGE_SIZE as u64 + 4);
    }

    #[test]
    fn out_of_range_dirty_gfn_is_rejected() {
        let mut eng = SnapshotEngine::new(4 * PG);
        let mem = vec![0u8; 4 * PG];
        let base = eng.snapshot_base(&mem, b"").unwrap();
        assert!(matches!(
            eng.snapshot_derive(base, &mem, Some(&[4]), b""),
            Err(SnapshotError::DirtyGfnOutOfRange { gfn: 4, pages: 4 })
        ));
        assert!(eng.snapshot_derive(base, &mem, Some(&[3]), b"").is_ok());
    }

    #[test]
    fn ram_vcpu_and_gic_corruption_each_fail_before_restore() {
        let mut gic = gicv3::Gicv3::new(gicv3::GicConfig {
            impl_spis: 32,
            timer_hz: 0x0f1e_2d3c_4b5a_6978,
            timer_intid: 27,
        })
        .unwrap();
        gic.raise(40).unwrap();
        let mut state = Arm64VmState::default();
        state.regs.x[0] = 0x1122_3344_5566_7788;
        state.devices = crate::vendor::arm64::records::encode_device_blob(
            &crate::vendor::arm64::records::Arm64DeviceState {
                clock_offset: 0xdead_beef,
                report_stream: vec![1, 2, 3],
                uart_capture: b"integrity".to_vec(),
                uart_regs: [13, 1, 0x70, 0x301, 0x10],
                gic: Some(gic.snapshot()),
                doorbell: Vec::new(),
                pvclock: None,
            },
        );
        let blob = state.encode().unwrap();
        let vcpu_marker = 0x1122_3344_5566_7788_u64.to_le_bytes();
        let gic_marker = 0x0f1e_2d3c_4b5a_6978_u64.to_le_bytes();
        let vcpu_offset = blob
            .windows(vcpu_marker.len())
            .position(|window| window == vcpu_marker)
            .expect("vCPU marker in canonical state");
        let gic_offset = blob
            .windows(gic_marker.len())
            .position(|window| window == gic_marker)
            .expect("GIC marker in canonical device blob");

        let mut memory = vec![0_u8; 2 * PG];
        memory[PG..2 * PG].fill(0x5a);
        let mut ram_engine = SnapshotEngine::new(memory.len());
        let ram = ram_engine.snapshot_base(&memory, &blob).unwrap();
        ram_engine.corrupt_page_for_test(ram, 1, 17, 0x80).unwrap();
        assert!(matches!(
            ram_engine.materialize(ram),
            Err(SnapshotError::Store(StoreError::PageIntegrity { gfn: 1 }))
        ));

        let mut vcpu_engine = SnapshotEngine::new(memory.len());
        let vcpu = vcpu_engine.snapshot_base(&memory, &blob).unwrap();
        vcpu_engine
            .corrupt_vm_state_for_test(vcpu, vcpu_offset, 0x01)
            .unwrap();
        assert!(matches!(
            vcpu_engine.vm_state::<Arm64VmState>(vcpu),
            Err(SnapshotError::Store(StoreError::VmStateIntegrity))
        ));

        let mut gic_engine = SnapshotEngine::new(memory.len());
        let gic = gic_engine.snapshot_base(&memory, &blob).unwrap();
        gic_engine
            .corrupt_vm_state_for_test(gic, gic_offset, 0x01)
            .unwrap();
        assert!(matches!(
            gic_engine.vm_state::<Arm64VmState>(gic),
            Err(SnapshotError::Store(StoreError::VmStateIntegrity))
        ));
    }
}
