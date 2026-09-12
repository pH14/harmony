// SPDX-License-Identifier: AGPL-3.0-or-later

mod mapping;

pub use mapping::Mapping;

use std::cell::RefCell;
#[allow(clippy::disallowed_types)]
use std::collections::HashMap;
use std::collections::{BTreeMap, BTreeSet};

pub const PAGE_SIZE: usize = 4096;

static ZERO_PAGE: [u8; PAGE_SIZE] = [0u8; PAGE_SIZE];

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct SnapshotId(u64);

#[derive(Copy, Clone, Debug)]
pub struct StoreConfig {
    pub mem_pages: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("unknown (or fully released) snapshot {0:?}")]
    UnknownSnapshot(SnapshotId),
    #[error("gfn {gfn} out of range: guest memory is {mem_pages} pages")]
    GfnOutOfRange { gfn: u64, mem_pages: u64 },
    #[error("page buffer is {len} bytes, expected {PAGE_SIZE}")]
    BadPageLength { len: usize },
    #[error("memory image is {got} bytes, expected {expected}")]
    BadMemoryLength { got: usize, expected: usize },
    #[error("snapshot page integrity check failed at gfn {gfn}")]
    PageIntegrity { gfn: u64 },
    #[error("snapshot vCPU/device state integrity check failed")]
    VmStateIntegrity,
    #[error("builder misuse: {0}")]
    BuilderMisuse(&'static str),
    #[error("i/o error")]
    Io(#[from] std::io::Error),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SnapStats {
    pub logical_pages: u64,
    pub owned_pages: u64,
    pub chain_len: u32,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct StoreStats {
    pub snapshots: u64,
    pub stored_unique_pages: u64,
    pub logical_pages_total: u64,
    pub bytes_resident: u64,
}

type PageHash = [u8; 32];

#[derive(Default)]
struct PageHashHasher(u64);

impl std::hash::Hasher for PageHashHasher {
    fn write(&mut self, bytes: &[u8]) {
        let (chunks, rest) = bytes.as_chunks::<8>();
        for chunk in chunks {
            self.0 ^= u64::from_le_bytes(*chunk);
        }
        let mut word = [0u8; 8];
        word[..rest.len()].copy_from_slice(rest);
        self.0 ^= u64::from_le_bytes(word);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

type BuildPageHashHasher = std::hash::BuildHasherDefault<PageHashHasher>;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum PageRef {
    Zero,
    Data(PageHash),
}

struct PageEntry {
    data: Box<[u8]>,
    refs: u64,
}

struct Layer {
    parent: Option<u64>,
    pages: BTreeMap<u64, PageRef>,
    vm_state: Vec<u8>,
    vm_state_hash: [u8; 32],
    refcount: u64,
    chain_len: u32,
    #[allow(clippy::disallowed_types)]
    resolve_cache: RefCell<HashMap<u64, PageRef>>,
}

pub struct Store {
    cfg: StoreConfig,
    next_id: u64,
    layers: BTreeMap<u64, Layer>,
    #[allow(clippy::disallowed_types)]
    pages: HashMap<PageHash, PageEntry, BuildPageHashHasher>,
    zero_hash: PageHash,
}

impl Store {
    pub fn new(cfg: StoreConfig) -> Store {
        Store {
            cfg,
            next_id: 0,
            layers: BTreeMap::new(),
            #[allow(clippy::disallowed_types)]
            pages: HashMap::default(),
            zero_hash: *blake3::hash(&[0u8; PAGE_SIZE]).as_bytes(),
        }
    }

    pub fn begin_base(&mut self) -> BaseBuilder<'_> {
        BaseBuilder {
            core: BuilderCore {
                store: self,
                parent: None,
                pages: BTreeMap::new(),
            },
        }
    }

    pub fn derive(&mut self, parent: SnapshotId) -> Result<DeltaBuilder<'_>, StoreError> {
        self.live_layer(parent)?;
        Ok(DeltaBuilder {
            core: BuilderCore {
                store: self,
                parent: Some(parent.0),
                pages: BTreeMap::new(),
            },
        })
    }

    pub fn flatten_base(
        &mut self,
        parent: SnapshotId,
        memory: &[u8],
        dirty: &[u64],
        vm_state: Vec<u8>,
    ) -> Result<SnapshotId, StoreError> {
        let expected = self
            .cfg
            .mem_pages
            .checked_mul(PAGE_SIZE as u64)
            .and_then(|len| usize::try_from(len).ok())
            .ok_or(StoreError::BadMemoryLength {
                got: memory.len(),
                expected: usize::MAX,
            })?;
        if memory.len() != expected {
            return Err(StoreError::BadMemoryLength {
                got: memory.len(),
                expected,
            });
        }

        self.live_layer(parent)?;
        let mut inherited: BTreeMap<u64, PageRef> = BTreeMap::new();
        let mut cur = Some(parent.0);
        while let Some(id) = cur {
            let Some(layer) = self.layers.get(&id) else {
                debug_assert!(false, "dangling parent link");
                break;
            };
            for (&gfn, &pref) in &layer.pages {
                inherited.entry(gfn).or_insert(pref);
            }
            cur = layer.parent;
        }
        let mut candidates: BTreeSet<u64> = inherited.keys().copied().collect();
        let mut dirty_set = BTreeSet::new();
        for &gfn in dirty {
            if gfn >= self.cfg.mem_pages {
                return Err(StoreError::GfnOutOfRange {
                    gfn,
                    mem_pages: self.cfg.mem_pages,
                });
            }
            candidates.insert(gfn);
            dirty_set.insert(gfn);
        }

        let mem_pages = self.cfg.mem_pages;
        let mut builder = self.begin_base();
        for gfn in candidates {
            let inherited = inherited.get(&gfn).copied().unwrap_or(PageRef::Zero);
            if !dirty_set.contains(&gfn) {
                builder.core.insert_page_ref(gfn, inherited)?;
                continue;
            }
            let offset = usize::try_from(gfn)
                .ok()
                .and_then(|gfn| gfn.checked_mul(PAGE_SIZE))
                .ok_or(StoreError::GfnOutOfRange { gfn, mem_pages })?;
            let end = offset
                .checked_add(PAGE_SIZE)
                .ok_or(StoreError::GfnOutOfRange { gfn, mem_pages })?;
            builder
                .core
                .write_page_against(gfn, &memory[offset..end], inherited)?;
        }
        Ok(builder.seal(vm_state))
    }

    pub fn read_page(&self, snap: SnapshotId, gfn: u64, out: &mut [u8]) -> Result<(), StoreError> {
        self.live_layer(snap)?;
        if out.len() != PAGE_SIZE {
            return Err(StoreError::BadPageLength { len: out.len() });
        }
        if gfn >= self.cfg.mem_pages {
            return Err(StoreError::GfnOutOfRange {
                gfn,
                mem_pages: self.cfg.mem_pages,
            });
        }
        match self.resolve(snap.0, gfn) {
            PageRef::Zero => out.fill(0),
            PageRef::Data(hash) => match self.pages.get(&hash) {
                Some(entry) if blake3::hash(&entry.data).as_bytes() == &hash => {
                    out.copy_from_slice(&entry.data);
                }
                Some(_) | None => return Err(StoreError::PageIntegrity { gfn }),
            },
        }
        Ok(())
    }

    pub fn diff_pages(
        &self,
        from: Option<SnapshotId>,
        to: SnapshotId,
    ) -> Result<Vec<(u64, [u8; PAGE_SIZE])>, StoreError> {
        self.live_layer(to)?;

        let Some(from) = from else {
            let mut pages = Vec::new();
            for gfn in 0..self.cfg.mem_pages {
                let mut page = [0u8; PAGE_SIZE];
                self.read_page(to, gfn, &mut page)?;
                pages.push((gfn, page));
            }
            return Ok(pages);
        };

        self.live_layer(from)?;

        let mut from_ancestors = BTreeSet::new();
        let mut cur = Some(from.0);
        while let Some(id) = cur {
            if !from_ancestors.insert(id) {
                return Err(StoreError::UnknownSnapshot(SnapshotId(id)));
            }
            let Some(layer) = self.layers.get(&id) else {
                return Err(StoreError::UnknownSnapshot(SnapshotId(id)));
            };
            cur = layer.parent;
        }

        let mut common = None;
        let mut to_visited = BTreeSet::new();
        cur = Some(to.0);
        while let Some(id) = cur {
            if !to_visited.insert(id) {
                return Err(StoreError::UnknownSnapshot(SnapshotId(id)));
            }
            if from_ancestors.contains(&id) {
                common = Some(id);
                break;
            }
            let Some(layer) = self.layers.get(&id) else {
                return Err(StoreError::UnknownSnapshot(SnapshotId(id)));
            };
            cur = layer.parent;
        }

        let mut changed_gfns = BTreeSet::new();
        for start in [from.0, to.0] {
            let mut side_visited = BTreeSet::new();
            cur = Some(start);
            while let Some(id) = cur {
                if Some(id) == common {
                    break;
                }
                if !side_visited.insert(id) {
                    return Err(StoreError::UnknownSnapshot(SnapshotId(id)));
                }
                let Some(layer) = self.layers.get(&id) else {
                    return Err(StoreError::UnknownSnapshot(SnapshotId(id)));
                };
                changed_gfns.extend(layer.pages.keys().copied());
                cur = layer.parent;
            }
        }

        let mut pages = Vec::with_capacity(changed_gfns.len());
        for gfn in changed_gfns {
            let mut page = [0u8; PAGE_SIZE];
            self.read_page(to, gfn, &mut page)?;
            pages.push((gfn, page));
        }
        Ok(pages)
    }

    pub fn vm_state(&self, snap: SnapshotId) -> Result<&[u8], StoreError> {
        let layer = self.live_layer(snap)?;
        if blake3::hash(&layer.vm_state).as_bytes() != &layer.vm_state_hash {
            return Err(StoreError::VmStateIntegrity);
        }
        Ok(&layer.vm_state)
    }

    pub fn materialize(&self, snap: SnapshotId) -> Result<Mapping, StoreError> {
        self.live_layer(snap)?;
        let len = self
            .cfg
            .mem_pages
            .checked_mul(PAGE_SIZE as u64)
            .ok_or_else(|| {
                StoreError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "logical image size overflows u64",
                ))
            })?;

        let mut resolved: BTreeMap<u64, PageRef> = BTreeMap::new();
        let mut cur = Some(snap.0);
        while let Some(id) = cur {
            let Some(layer) = self.layers.get(&id) else {
                debug_assert!(false, "dangling parent link");
                break;
            };
            for (&gfn, &pref) in &layer.pages {
                resolved.entry(gfn).or_insert(pref);
            }
            cur = layer.parent;
        }

        let mut verified_pages = Vec::with_capacity(resolved.len());
        for (&gfn, &pref) in &resolved {
            let PageRef::Data(hash) = pref else {
                continue;
            };
            let Some(entry) = self.pages.get(&hash) else {
                return Err(StoreError::PageIntegrity { gfn });
            };
            if blake3::hash(&entry.data).as_bytes() != &hash {
                return Err(StoreError::PageIntegrity { gfn });
            }
            verified_pages.push((gfn, &*entry.data));
        }

        let file = tempfile::tempfile()?;
        file.set_len(len)?;
        Mapping::populate(&file, len, verified_pages.into_iter())?;
        Ok(Mapping::new(file, len)?)
    }

    pub fn retain(&mut self, snap: SnapshotId) -> Result<(), StoreError> {
        let layer = self.live_layer_mut(snap)?;
        layer.refcount = layer.refcount.saturating_add(1);
        Ok(())
    }

    pub fn release(&mut self, snap: SnapshotId) -> Result<(), StoreError> {
        let layer = self.live_layer_mut(snap)?;
        layer.refcount -= 1;
        Ok(())
    }

    pub fn gc(&mut self) -> u64 {
        let mut reachable: BTreeSet<u64> = BTreeSet::new();
        for (&id, layer) in &self.layers {
            if layer.refcount == 0 {
                continue;
            }
            let mut cur = Some(id);
            while let Some(c) = cur {
                if !reachable.insert(c) {
                    break;
                }
                cur = self.layers.get(&c).and_then(|l| l.parent);
            }
        }
        let dead: Vec<u64> = self
            .layers
            .keys()
            .copied()
            .filter(|id| !reachable.contains(id))
            .collect();
        let mut freed = 0u64;
        for id in dead {
            if let Some(layer) = self.layers.remove(&id) {
                freed += layer.vm_state.len() as u64;
                for (_gfn, pref) in layer.pages {
                    if let PageRef::Data(hash) = pref {
                        freed += self.release_page_ref(hash);
                    }
                }
            }
        }
        freed
    }

    pub fn stats(&self, snap: SnapshotId) -> Result<SnapStats, StoreError> {
        let layer = self.live_layer(snap)?;
        Ok(SnapStats {
            logical_pages: self.cfg.mem_pages,
            owned_pages: layer.pages.len() as u64,
            chain_len: layer.chain_len,
        })
    }

    pub fn store_stats(&self) -> StoreStats {
        let snapshots = self.layers.values().filter(|l| l.refcount > 0).count() as u64;
        let vm_state_bytes: u64 = self.layers.values().map(|l| l.vm_state.len() as u64).sum();
        StoreStats {
            snapshots,
            stored_unique_pages: self.pages.len() as u64,
            logical_pages_total: snapshots.saturating_mul(self.cfg.mem_pages),
            bytes_resident: (self.pages.len() as u64).saturating_mul(PAGE_SIZE as u64)
                + vm_state_bytes,
        }
    }

    #[cfg(feature = "test-utils")]
    pub fn corrupt_page_for_test(
        &mut self,
        snap: SnapshotId,
        gfn: u64,
        byte: usize,
        mask: u8,
    ) -> Result<(), StoreError> {
        self.live_layer(snap)?;
        if gfn >= self.cfg.mem_pages {
            return Err(StoreError::GfnOutOfRange {
                gfn,
                mem_pages: self.cfg.mem_pages,
            });
        }
        let PageRef::Data(hash) = self.resolve(snap.0, gfn) else {
            return Err(StoreError::BuilderMisuse(
                "cannot corrupt an implicit zero page",
            ));
        };
        let target = self
            .pages
            .get_mut(&hash)
            .and_then(|entry| entry.data.get_mut(byte))
            .ok_or(StoreError::BuilderMisuse(
                "corruption byte lies outside a resident page",
            ))?;
        *target ^= mask;
        Ok(())
    }

    #[cfg(feature = "test-utils")]
    pub fn corrupt_vm_state_for_test(
        &mut self,
        snap: SnapshotId,
        byte: usize,
        mask: u8,
    ) -> Result<(), StoreError> {
        let target =
            self.live_layer_mut(snap)?
                .vm_state
                .get_mut(byte)
                .ok_or(StoreError::BuilderMisuse(
                    "corruption byte lies outside vCPU/device state",
                ))?;
        *target ^= mask;
        Ok(())
    }

    fn live_layer(&self, snap: SnapshotId) -> Result<&Layer, StoreError> {
        self.layers
            .get(&snap.0)
            .filter(|l| l.refcount > 0)
            .ok_or(StoreError::UnknownSnapshot(snap))
    }

    fn live_layer_mut(&mut self, snap: SnapshotId) -> Result<&mut Layer, StoreError> {
        self.layers
            .get_mut(&snap.0)
            .filter(|l| l.refcount > 0)
            .ok_or(StoreError::UnknownSnapshot(snap))
    }

    fn resolve(&self, start: u64, gfn: u64) -> PageRef {
        let mut visited: Vec<u64> = Vec::new();
        let mut cur = Some(start);
        let mut result = PageRef::Zero;
        while let Some(id) = cur {
            let Some(layer) = self.layers.get(&id) else {
                debug_assert!(false, "dangling parent link");
                break;
            };
            if let Some(&p) = layer.pages.get(&gfn) {
                result = p;
                break;
            }
            if let Some(&p) = layer.resolve_cache.borrow().get(&gfn) {
                result = p;
                break;
            }
            visited.push(id);
            cur = layer.parent;
        }
        for id in visited {
            if let Some(layer) = self.layers.get(&id) {
                layer.resolve_cache.borrow_mut().insert(gfn, result);
            }
        }
        result
    }

    fn intern_page(&mut self, hash: PageHash, data: &[u8]) {
        self.pages
            .entry(hash)
            .and_modify(|e| e.refs = e.refs.saturating_add(1))
            .or_insert_with(|| PageEntry {
                data: data.into(),
                refs: 1,
            });
    }

    fn release_page_ref(&mut self, hash: PageHash) -> u64 {
        match self.pages.get_mut(&hash) {
            Some(entry) if entry.refs > 1 => {
                entry.refs -= 1;
                0
            }
            Some(_) => {
                self.pages.remove(&hash);
                PAGE_SIZE as u64
            }
            None => {
                debug_assert!(false, "release of untracked page");
                0
            }
        }
    }
}

struct BuilderCore<'a> {
    store: &'a mut Store,
    parent: Option<u64>,
    pages: BTreeMap<u64, PageRef>,
}

impl BuilderCore<'_> {
    fn write_page(&mut self, gfn: u64, data: &[u8]) -> Result<(), StoreError> {
        if data.len() != PAGE_SIZE {
            return Err(StoreError::BadPageLength { len: data.len() });
        }
        if gfn >= self.store.cfg.mem_pages {
            return Err(StoreError::GfnOutOfRange {
                gfn,
                mem_pages: self.store.cfg.mem_pages,
            });
        }
        let pref = if data == &ZERO_PAGE[..] {
            PageRef::Zero
        } else {
            let hash = *blake3::hash(data).as_bytes();
            debug_assert_ne!(
                hash, self.store.zero_hash,
                "non-zero page hashed to zero_hash"
            );
            self.store.intern_page(hash, data);
            PageRef::Data(hash)
        };
        if let Some(PageRef::Data(old)) = self.pages.insert(gfn, pref) {
            self.store.release_page_ref(old);
        }
        Ok(())
    }

    fn insert_page_ref(&mut self, gfn: u64, pref: PageRef) -> Result<(), StoreError> {
        if let PageRef::Data(hash) = pref {
            let Some(entry) = self.store.pages.get_mut(&hash) else {
                return Err(StoreError::PageIntegrity { gfn });
            };
            entry.refs = entry.refs.saturating_add(1);
        }
        if let Some(PageRef::Data(old)) = self.pages.insert(gfn, pref) {
            self.store.release_page_ref(old);
        }
        Ok(())
    }

    fn write_page_against(
        &mut self,
        gfn: u64,
        data: &[u8],
        inherited: PageRef,
    ) -> Result<(), StoreError> {
        if data.len() != PAGE_SIZE {
            return Err(StoreError::BadPageLength { len: data.len() });
        }
        let unchanged = match inherited {
            PageRef::Zero => data == &ZERO_PAGE[..],
            PageRef::Data(hash) => {
                let Some(entry) = self.store.pages.get(&hash) else {
                    return Err(StoreError::PageIntegrity { gfn });
                };
                entry.data.as_ref() == data
            }
        };
        if unchanged {
            self.insert_page_ref(gfn, inherited)
        } else {
            self.write_page(gfn, data)
        }
    }

    fn seal(mut self, vm_state: Vec<u8>) -> SnapshotId {
        let vm_state_hash = *blake3::hash(&vm_state).as_bytes();
        let pages = std::mem::take(&mut self.pages);
        let mut kept: BTreeMap<u64, PageRef> = BTreeMap::new();
        for (gfn, pref) in pages {
            let inherited = match self.parent {
                Some(p) => self.store.resolve(p, gfn),
                None => PageRef::Zero,
            };
            if pref == inherited {
                if let PageRef::Data(hash) = pref {
                    self.store.release_page_ref(hash);
                }
            } else {
                kept.insert(gfn, pref);
            }
        }
        let chain_len = match self.parent {
            Some(p) => self
                .store
                .layers
                .get(&p)
                .map_or(1, |l| l.chain_len.saturating_add(1)),
            None => 1,
        };
        let id = self.store.next_id;
        self.store.next_id += 1;
        self.store.layers.insert(
            id,
            Layer {
                parent: self.parent,
                pages: kept,
                vm_state,
                vm_state_hash,
                refcount: 1,
                chain_len,
                #[allow(clippy::disallowed_types)]
                resolve_cache: RefCell::new(HashMap::new()),
            },
        );
        SnapshotId(id)
    }
}

impl Drop for BuilderCore<'_> {
    fn drop(&mut self) {
        let pages = std::mem::take(&mut self.pages);
        for (_gfn, pref) in pages {
            if let PageRef::Data(hash) = pref {
                self.store.release_page_ref(hash);
            }
        }
    }
}

pub struct BaseBuilder<'a> {
    core: BuilderCore<'a>,
}

impl BaseBuilder<'_> {
    pub fn write_page(&mut self, gfn: u64, data: &[u8]) -> Result<(), StoreError> {
        self.core.write_page(gfn, data)
    }

    pub fn seal(self, vm_state: Vec<u8>) -> SnapshotId {
        self.core.seal(vm_state)
    }
}

pub struct DeltaBuilder<'a> {
    core: BuilderCore<'a>,
}

impl DeltaBuilder<'_> {
    pub fn write_page(&mut self, gfn: u64, data: &[u8]) -> Result<(), StoreError> {
        self.core.write_page(gfn, data)
    }

    pub fn seal(self, vm_state: Vec<u8>) -> SnapshotId {
        self.core.seal(vm_state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(mem_pages: u64) -> StoreConfig {
        StoreConfig { mem_pages }
    }

    #[test]
    fn page_hash_hasher_backs_a_working_map() {
        use std::hash::{BuildHasher, Hash, Hasher};

        fn fold(key: &PageHash) -> u64 {
            let mut h = PageHashHasher::default();
            key.hash(&mut h);
            h.finish()
        }

        let a: PageHash = *blake3::hash(b"a").as_bytes();
        let b: PageHash = *blake3::hash(b"b").as_bytes();
        let zero: PageHash = [0u8; 32];
        assert_eq!(fold(&a), fold(&a));
        assert_ne!(fold(&a), fold(&b));

        let mut raw = PageHashHasher::default();
        raw.write(&zero);
        assert_eq!(raw.finish(), 0);
        assert_ne!(fold(&zero), 0);

        let mut tail = PageHashHasher::default();
        tail.write(&[0x12, 0x34, 0x56]);
        assert_eq!(tail.finish(), 0x56_3412);

        let mut partial = PageHashHasher::default();
        partial.write(&[1]);
        assert_eq!(partial.finish(), 1);
        partial.write(&[1]);
        assert_eq!(partial.finish(), 0);

        assert_eq!(fold(&[0xFFu8; 32]), fold(&zero));

        for probe in [0usize, 7, 8, 31] {
            let mut k = zero;
            k[probe] = 1;
            assert_ne!(
                fold(&k),
                fold(&zero),
                "byte {probe} does not reach the fold"
            );
        }

        let keys: Vec<PageHash> = (0..512u32)
            .map(|i| *blake3::hash(&i.to_le_bytes()).as_bytes())
            .collect();
        let folds: BTreeSet<u64> = keys.iter().map(fold).collect();
        assert_eq!(
            folds.len(),
            keys.len(),
            "XOR-fold collided on BLAKE3 digests"
        );

        #[allow(clippy::disallowed_types)]
        let mut map: HashMap<PageHash, u32, BuildPageHashHasher> = HashMap::default();
        for (i, k) in keys.iter().enumerate() {
            assert!(map.insert(*k, i as u32).is_none());
        }
        assert_eq!(map.len(), keys.len());
        for (i, k) in keys.iter().enumerate() {
            assert_eq!(map.get(k), Some(&(i as u32)));
        }
        for k in &keys {
            assert!(map.remove(k).is_some());
        }
        assert!(map.is_empty());
        assert_eq!(BuildPageHashHasher::default().hash_one(a), fold(&a));
    }

    #[test]
    fn flatten_base_walks_chain_and_preserves_zero_transitions() {
        let mut store = Store::new(cfg(8));
        let mut base_image = vec![0u8; 8 * PAGE_SIZE];
        base_image[..PAGE_SIZE].fill(1);
        base_image[PAGE_SIZE..2 * PAGE_SIZE].fill(2);
        let mut builder = store.begin_base();
        for (gfn, frame) in base_image.as_chunks::<PAGE_SIZE>().0.iter().enumerate() {
            builder.write_page(gfn as u64, frame).unwrap();
        }
        let base = builder.seal(Vec::new());

        let mut child_image = base_image.clone();
        child_image[..PAGE_SIZE].fill(0);
        child_image[2 * PAGE_SIZE..3 * PAGE_SIZE].fill(3);
        let mut builder = store.derive(base).unwrap();
        builder.write_page(0, &child_image[..PAGE_SIZE]).unwrap();
        builder
            .write_page(2, &child_image[2 * PAGE_SIZE..3 * PAGE_SIZE])
            .unwrap();
        let child = builder.seal(Vec::new());

        let mut grand_image = child_image.clone();
        grand_image[PAGE_SIZE..2 * PAGE_SIZE].fill(0);
        let mut builder = store.derive(child).unwrap();
        builder
            .write_page(1, &grand_image[PAGE_SIZE..2 * PAGE_SIZE])
            .unwrap();
        let grand = builder.seal(Vec::new());
        let mut current = grand_image;
        current[5 * PAGE_SIZE..6 * PAGE_SIZE].fill(9);

        let flat = store
            .flatten_base(grand, &current, &[5], Vec::new())
            .unwrap();
        assert_eq!(store.stats(flat).unwrap().chain_len, 1);
        assert_eq!(store.stats(flat).unwrap().owned_pages, 2);
        let mut page = [0u8; PAGE_SIZE];
        for gfn in 0..8 {
            store.read_page(flat, gfn, &mut page).unwrap();
            assert_eq!(
                page,
                current[gfn as usize * PAGE_SIZE..(gfn as usize + 1) * PAGE_SIZE]
            );
        }
    }

    #[test]
    fn flatten_base_rejects_wrong_image_and_dirty_gfn() {
        let mut store = Store::new(cfg(2));
        let base = store.begin_base().seal(Vec::new());
        assert!(matches!(
            store.flatten_base(base, &[0; PAGE_SIZE], &[], Vec::new()),
            Err(StoreError::BadMemoryLength { got, expected })
                if got == PAGE_SIZE && expected == 2 * PAGE_SIZE
        ));
        let memory = vec![0u8; 2 * PAGE_SIZE];
        assert!(matches!(
            store.flatten_base(base, &memory, &[2], Vec::new()),
            Err(StoreError::GfnOutOfRange {
                gfn: 2,
                mem_pages: 2
            })
        ));
    }

    #[test]
    fn abandoned_builder_leaks_nothing() {
        let mut store = Store::new(cfg(8));
        let mut b = store.begin_base();
        b.write_page(0, &[1u8; PAGE_SIZE]).unwrap();
        b.write_page(1, &[2u8; PAGE_SIZE]).unwrap();
        drop(b);
        let s = store.store_stats();
        assert_eq!(s.stored_unique_pages, 0);
        assert_eq!(s.bytes_resident, 0);
        assert_eq!(s.snapshots, 0);
    }

    #[test]
    fn overwrite_in_builder_releases_old_content() {
        let mut store = Store::new(cfg(8));
        let mut b = store.begin_base();
        b.write_page(0, &[1u8; PAGE_SIZE]).unwrap();
        b.write_page(0, &[2u8; PAGE_SIZE]).unwrap();
        let id = b.seal(vec![]);
        let s = store.store_stats();
        assert_eq!(s.stored_unique_pages, 1);
        let mut out = [0u8; PAGE_SIZE];
        store.read_page(id, 0, &mut out).unwrap();
        assert_eq!(out, [2u8; PAGE_SIZE]);
    }

    #[test]
    fn zero_writes_are_never_stored() {
        let mut store = Store::new(cfg(8));
        let mut b = store.begin_base();
        b.write_page(3, &[0u8; PAGE_SIZE]).unwrap();
        let base = b.seal(vec![]);
        assert_eq!(store.store_stats().stored_unique_pages, 0);
        assert_eq!(store.stats(base).unwrap().owned_pages, 0);
    }

    #[test]
    fn zero_write_over_data_is_owned_but_unstored() {
        let mut store = Store::new(cfg(8));
        let mut b = store.begin_base();
        b.write_page(0, &[7u8; PAGE_SIZE]).unwrap();
        let base = b.seal(vec![]);
        let mut d = store.derive(base).unwrap();
        d.write_page(0, &[0u8; PAGE_SIZE]).unwrap();
        let child = d.seal(vec![]);
        assert_eq!(store.stats(child).unwrap().owned_pages, 1);
        assert_eq!(store.store_stats().stored_unique_pages, 1);
        let mut out = [1u8; PAGE_SIZE];
        store.read_page(child, 0, &mut out).unwrap();
        assert_eq!(out, [0u8; PAGE_SIZE]);
        store.read_page(base, 0, &mut out).unwrap();
        assert_eq!(out, [7u8; PAGE_SIZE]);
    }

    #[test]
    fn zero_shortcut_matches_hash_comparison() {
        let mut store = Store::new(cfg(8));
        for probe in [0usize, 1, PAGE_SIZE / 2, PAGE_SIZE - 2, PAGE_SIZE - 1] {
            let mut data = [0u8; PAGE_SIZE];
            data[probe] = 1;
            assert_ne!(
                *blake3::hash(&data).as_bytes(),
                store.zero_hash,
                "byte {probe} set must not hash to zero_hash"
            );
            let mut b = store.begin_base();
            b.write_page(0, &data).unwrap();
            let snap = b.seal(vec![]);
            assert_eq!(store.stats(snap).unwrap().owned_pages, 1);
            let mut out = [0u8; PAGE_SIZE];
            store.read_page(snap, 0, &mut out).unwrap();
            assert_eq!(out, data);
            store.release(snap).unwrap();
            store.gc();
        }
        assert_eq!(store.store_stats().stored_unique_pages, 0);

        let mut b = store.begin_base();
        b.write_page(0, &[0u8; PAGE_SIZE]).unwrap();
        let snap = b.seal(vec![]);
        assert_eq!(store.stats(snap).unwrap().owned_pages, 0);
        assert_eq!(store.store_stats().stored_unique_pages, 0);
    }

    #[test]
    fn zero_write_over_buffered_data_releases_the_content() {
        let mut store = Store::new(cfg(8));
        let mut b = store.begin_base();
        b.write_page(0, &[7u8; PAGE_SIZE]).unwrap();
        assert_eq!(b.core.store.store_stats().stored_unique_pages, 1);
        b.write_page(0, &[0u8; PAGE_SIZE]).unwrap();
        assert_eq!(b.core.store.store_stats().stored_unique_pages, 0);
        let snap = b.seal(vec![]);
        assert_eq!(store.stats(snap).unwrap().owned_pages, 0);
        assert_eq!(store.store_stats().stored_unique_pages, 0);
    }

    #[test]
    fn resolve_memoizes_along_the_path() {
        let mut store = Store::new(cfg(4));
        let base = store.begin_base().seal(vec![]);
        let mut b = store.derive(base).unwrap();
        b.write_page(0, &[9u8; PAGE_SIZE]).unwrap();
        let mid = b.seal(vec![]);
        let leaf = store.derive(mid).unwrap().seal(vec![]);
        let mut out = [0u8; PAGE_SIZE];
        store.read_page(leaf, 0, &mut out).unwrap();
        assert_eq!(
            store.layers[&leaf.0].resolve_cache.borrow().get(&0),
            Some(&PageRef::Data(*blake3::hash(&[9u8; PAGE_SIZE]).as_bytes()))
        );
        store.read_page(leaf, 0, &mut out).unwrap();
        assert_eq!(out, [9u8; PAGE_SIZE]);
    }

    #[test]
    fn diff_pages_sibling_branches_are_sorted_and_target_resolved() {
        let mut store = Store::new(cfg(4));
        let base = store.begin_base().seal(vec![]);

        let mut left_builder = store.derive(base).unwrap();
        left_builder.write_page(1, &[0x11; PAGE_SIZE]).unwrap();
        let left = left_builder.seal(vec![]);

        let mut right_builder = store.derive(base).unwrap();
        right_builder.write_page(2, &[0x22; PAGE_SIZE]).unwrap();
        let right = right_builder.seal(vec![]);

        assert_eq!(
            store.diff_pages(Some(left), right).unwrap(),
            vec![(1, [0u8; PAGE_SIZE]), (2, [0x22u8; PAGE_SIZE])]
        );
    }

    #[test]
    fn diff_pages_ancestor_descendant_handles_explicit_and_implicit_zero() {
        let mut store = Store::new(cfg(4));
        let mut base_builder = store.begin_base();
        base_builder.write_page(0, &[0x10; PAGE_SIZE]).unwrap();
        let base = base_builder.seal(vec![]);

        let mut child_builder = store.derive(base).unwrap();
        child_builder.write_page(0, &[0u8; PAGE_SIZE]).unwrap();
        child_builder.write_page(2, &[0x20; PAGE_SIZE]).unwrap();
        let child = child_builder.seal(vec![]);

        assert_eq!(
            store.diff_pages(Some(base), child).unwrap(),
            vec![(0, [0u8; PAGE_SIZE]), (2, [0x20u8; PAGE_SIZE])]
        );
        assert_eq!(
            store.diff_pages(Some(child), base).unwrap(),
            vec![(0, [0x10u8; PAGE_SIZE]), (2, [0u8; PAGE_SIZE])]
        );
    }

    #[test]
    fn diff_pages_identical_snapshots_are_empty() {
        let mut store = Store::new(cfg(4));
        let base = store.begin_base().seal(vec![]);
        let child = store.derive(base).unwrap().seal(vec![]);

        assert!(store.diff_pages(Some(base), base).unwrap().is_empty());
        assert!(store.diff_pages(Some(base), child).unwrap().is_empty());
    }

    #[test]
    fn diff_pages_from_none_returns_full_resolved_image() {
        let mut store = Store::new(cfg(4));
        let mut base_builder = store.begin_base();
        base_builder.write_page(1, &[0x11; PAGE_SIZE]).unwrap();
        let base = base_builder.seal(vec![]);
        let mut child_builder = store.derive(base).unwrap();
        child_builder.write_page(2, &[0x22; PAGE_SIZE]).unwrap();
        let child = child_builder.seal(vec![]);

        assert_eq!(
            store.diff_pages(None, child).unwrap(),
            vec![
                (0, [0u8; PAGE_SIZE]),
                (1, [0x11u8; PAGE_SIZE]),
                (2, [0x22u8; PAGE_SIZE]),
                (3, [0u8; PAGE_SIZE]),
            ]
        );
    }

    #[test]
    fn diff_pages_rejects_unknown_ids() {
        let mut store = Store::new(cfg(1));
        let base = store.begin_base().seal(vec![]);
        let unknown = SnapshotId(u64::MAX);

        assert!(matches!(
            store.diff_pages(Some(unknown), base),
            Err(StoreError::UnknownSnapshot(id)) if id == unknown
        ));
        assert!(matches!(
            store.diff_pages(Some(base), unknown),
            Err(StoreError::UnknownSnapshot(id)) if id == unknown
        ));
    }

    #[test]
    fn released_snapshot_behaves_as_unknown() {
        let mut store = Store::new(cfg(4));
        let base = store.begin_base().seal(vec![]);
        store.release(base).unwrap();
        let mut out = [0u8; PAGE_SIZE];
        assert!(matches!(
            store.read_page(base, 0, &mut out),
            Err(StoreError::UnknownSnapshot(_))
        ));
        assert!(matches!(
            store.retain(base),
            Err(StoreError::UnknownSnapshot(_))
        ));
        assert!(matches!(
            store.release(base),
            Err(StoreError::UnknownSnapshot(_))
        ));
        assert!(matches!(
            store.derive(base),
            Err(StoreError::UnknownSnapshot(_))
        ));
        assert!(matches!(
            store.stats(base),
            Err(StoreError::UnknownSnapshot(_))
        ));
    }

    #[test]
    fn empty_store_and_zero_sized_image() {
        let mut store = Store::new(cfg(0));
        let base = store.begin_base().seal(b"state".to_vec());
        assert_eq!(store.vm_state(base).unwrap(), b"state");
        let mapping = store.materialize(base).unwrap();
        assert_eq!(mapping.len(), 0);
        assert!(mapping.is_empty());
        assert_eq!(mapping.as_slice(), &[] as &[u8]);
        let mut out = [0u8; PAGE_SIZE];
        assert!(matches!(
            store.read_page(base, 0, &mut out),
            Err(StoreError::GfnOutOfRange { .. })
        ));
    }

    #[cfg(feature = "test-utils")]
    #[test]
    fn sealed_page_and_vm_state_corruption_are_detected() {
        let mut store = Store::new(StoreConfig { mem_pages: 2 });
        let mut builder = store.begin_base();
        builder.write_page(1, &[0x5a; PAGE_SIZE]).unwrap();
        let snap = builder.seal(b"vcpu-device-state".to_vec());

        store.corrupt_page_for_test(snap, 1, 7, 0x5a).unwrap();
        let mut out = [0_u8; PAGE_SIZE];
        assert!(matches!(
            store.read_page(snap, 1, &mut out),
            Err(StoreError::PageIntegrity { gfn: 1 })
        ));
        assert!(matches!(
            store.materialize(snap),
            Err(StoreError::PageIntegrity { gfn: 1 })
        ));

        let mut clean = Store::new(StoreConfig { mem_pages: 1 });
        let snap = clean.begin_base().seal(b"vcpu-device-state".to_vec());
        clean.corrupt_vm_state_for_test(snap, 5, b'd').unwrap();
        assert!(matches!(
            clean.vm_state(snap),
            Err(StoreError::VmStateIntegrity)
        ));
    }
}
