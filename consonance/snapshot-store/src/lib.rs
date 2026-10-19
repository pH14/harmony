// SPDX-License-Identifier: AGPL-3.0-or-later

mod mapping;

pub use mapping::Mapping;

use std::cell::RefCell;
#[allow(clippy::disallowed_types)]
use std::collections::HashMap;
use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};

pub const PAGE_SIZE: usize = 4096;

static ZERO_PAGE: [u8; PAGE_SIZE] = [0u8; PAGE_SIZE];
static NEXT_STORE_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Copy, Clone, Eq, PartialEq, Hash)]
pub struct SnapshotId(u64, u64);

impl std::fmt::Debug for SnapshotId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_tuple("SnapshotId").field(&self.0).finish()
    }
}

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
    #[error("target page {gfn} does not follow page {previous}")]
    TargetPageOrder { previous: u64, gfn: u64 },
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

pub type PageHash = [u8; 32];

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
    Data(NonZeroUsize),
}

struct PageEntry {
    hash: PageHash,
    data: Box<[u8]>,
    refs: u64,
}

struct Layer {
    parent: Option<u64>,
    pages: Box<[(u64, PageRef)]>,
    vm_state: Vec<u8>,
    vm_state_hash: [u8; 32],
    refcount: u64,
    children: usize,
    chain_len: u32,
    #[allow(clippy::disallowed_types)]
    resolve_cache: RefCell<HashMap<u64, PageRef>>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VerificationTelemetry {
    pub page_requests: u64,
    pub memo_hits: u64,
    pub zero_requests: u64,
    pub resident_hashes: u64,
    pub resident_bytes: u64,
    pub nanos: u64,
    pub errors: u64,
    pub vm_state_hashes: u64,
    pub vm_state_bytes: u64,
    pub vm_state_nanos: u64,
    pub vm_state_errors: u64,
}

impl VerificationTelemetry {
    pub fn counters(&self) -> Vec<(String, u64)> {
        [
            ("page_requests", self.page_requests),
            ("memo_hits", self.memo_hits),
            ("zero_requests", self.zero_requests),
            ("resident_hashes", self.resident_hashes),
            ("resident_bytes", self.resident_bytes),
            ("ns", self.nanos),
            ("errors", self.errors),
            ("vm_state_hashes", self.vm_state_hashes),
            ("vm_state_bytes", self.vm_state_bytes),
            ("vm_state_ns", self.vm_state_nanos),
            ("vm_state_errors", self.vm_state_errors),
        ]
        .into_iter()
        .map(|(name, value)| (format!("snapshot.verify.{name}"), value))
        .collect()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CopyTelemetry {
    pub intern_calls: u64,
    pub intern_hits: u64,
    pub intern_copy_bytes: u64,
    pub read_copy_bytes: u64,
}
impl CopyTelemetry {
    pub fn counters(self) -> Vec<(String, u64)> {
        [
            ("intern_calls", self.intern_calls),
            ("intern_hits", self.intern_hits),
            ("intern_copy_bytes", self.intern_copy_bytes),
            ("read_copy_bytes", self.read_copy_bytes),
        ]
        .into_iter()
        .map(|(name, value)| (format!("snapshot.copy.{name}"), value))
        .collect()
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct VerificationWork {
    scans: usize,
    layers: usize,
    rows: usize,
}

pub struct Store {
    cfg: StoreConfig,
    #[cfg(test)]
    verification_work: std::cell::Cell<VerificationWork>,
    #[cfg(test)]
    dense_gfn_scans: std::cell::Cell<usize>,
    verification: std::cell::Cell<VerificationTelemetry>,
    copies: std::cell::Cell<CopyTelemetry>,
    verification_batch_depth: std::cell::Cell<usize>,
    store_id: u64,
    next_id: u64,
    layers: BTreeMap<u64, Layer>,
    collectible: Vec<u64>,
    live_snapshots: u64,
    vm_state_bytes: u64,
    #[allow(clippy::disallowed_types)]
    page_index: HashMap<PageHash, NonZeroUsize, BuildPageHashHasher>,
    pages: Vec<Option<PageEntry>>,
    free_pages: Vec<NonZeroUsize>,
    zero_hash: PageHash,
    #[cfg(any(test, feature = "test-utils"))]
    page_verifications: std::cell::Cell<usize>,
}

const DENSE_MEMBERSHIP_MIN_KEYS: usize = 64;
const MAX_DENSE_MEMBERSHIP_WORDS: usize = 1024 * 1024 / size_of::<u64>();

enum VerifiedPages {
    Dense(Vec<u64>),
    Sparse {
        ids: BTreeSet<NonZeroUsize>,
        dense_words: Option<usize>,
    },
}
impl VerifiedPages {
    fn new(slots: usize, live: usize) -> Self {
        let dense_words = (slots <= MAX_DENSE_MEMBERSHIP_WORDS * 64
            && slots <= live.saturating_mul(8))
        .then_some(slots.div_ceil(64));
        Self::Sparse {
            ids: BTreeSet::new(),
            dense_words,
        }
    }

    fn contains(&self, id: &NonZeroUsize) -> bool {
        match self {
            Self::Dense(words) => words[(id.get() - 1) / 64] & (1 << ((id.get() - 1) % 64)) != 0,
            Self::Sparse { ids, .. } => ids.contains(id),
        }
    }

    fn insert(&mut self, id: NonZeroUsize) {
        match self {
            Self::Dense(words) => words[(id.get() - 1) / 64] |= 1 << ((id.get() - 1) % 64),
            Self::Sparse { ids, dense_words } => {
                ids.insert(id);
                if let Some(len) = *dense_words
                    && ids.len() >= DENSE_MEMBERSHIP_MIN_KEYS
                    && ids.len() >= len
                {
                    let mut words = vec![0; len];
                    for id in ids.iter() {
                        words[(id.get() - 1) / 64] |= 1 << ((id.get() - 1) % 64);
                    }
                    *self = Self::Dense(words);
                }
            }
        }
    }
}

enum GfnMembership {
    Dense(Vec<u64>),
    Sparse {
        gfns: BTreeSet<u64>,
        dense_words: Option<usize>,
    },
}

struct ResolvedGfns {
    mem_pages: u64,
    membership: GfnMembership,
}

impl ResolvedGfns {
    fn new(mem_pages: u64) -> Self {
        let dense_words = usize::try_from(mem_pages.div_ceil(64))
            .ok()
            .filter(|&words| words <= MAX_DENSE_MEMBERSHIP_WORDS);
        Self {
            mem_pages,
            membership: GfnMembership::Sparse {
                gfns: BTreeSet::new(),
                dense_words,
            },
        }
    }

    fn insert(&mut self, gfn: u64) -> Result<bool, StoreError> {
        let mem_pages = self.mem_pages;
        if gfn >= mem_pages {
            return Err(StoreError::GfnOutOfRange { gfn, mem_pages });
        }
        match &mut self.membership {
            GfnMembership::Dense(words) => {
                let index = usize::try_from(gfn)
                    .map_err(|_| StoreError::GfnOutOfRange { gfn, mem_pages })?;
                let word = words
                    .get_mut(index / 64)
                    .ok_or(StoreError::GfnOutOfRange { gfn, mem_pages })?;
                let mask = 1 << (index % 64);
                let inserted = *word & mask == 0;
                *word |= mask;
                Ok(inserted)
            }
            GfnMembership::Sparse { gfns, dense_words } => {
                let inserted = gfns.insert(gfn);
                if let Some(len) = *dense_words
                    && gfns.len() >= DENSE_MEMBERSHIP_MIN_KEYS
                    && gfns.len() >= len
                {
                    let mut words = vec![0; len];
                    for &gfn in gfns.iter() {
                        let index = usize::try_from(gfn)
                            .map_err(|_| StoreError::GfnOutOfRange { gfn, mem_pages })?;
                        let word = words
                            .get_mut(index / 64)
                            .ok_or(StoreError::GfnOutOfRange { gfn, mem_pages })?;
                        *word |= 1 << (index % 64);
                    }
                    self.membership = GfnMembership::Dense(words);
                }
                Ok(inserted)
            }
        }
    }
}

pub struct ReadTransaction<'store> {
    store: &'store Store,
    verified_pages: VerifiedPages,
    verified_snapshots: BTreeSet<(u64, u64)>,
}

struct VerificationBatch<'a> {
    store: &'a Store,
    started: Option<std::time::Instant>,
}

impl Drop for VerificationBatch<'_> {
    fn drop(&mut self) {
        self.store
            .verification_batch_depth
            .set(self.store.verification_batch_depth.get() - 1);
        if let Some(started) = self.started {
            self.store.note_verification(|t| {
                t.nanos = t.nanos.saturating_add(
                    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
                );
            });
        }
    }
}

impl Store {
    #[allow(clippy::disallowed_methods)]
    fn verification_batch(&self) -> VerificationBatch<'_> {
        let depth = self.verification_batch_depth.get();
        self.verification_batch_depth
            .set(depth.checked_add(1).expect("verification nesting overflow"));
        VerificationBatch {
            store: self,
            started: (depth == 0).then(std::time::Instant::now),
        }
    }

    #[cfg(feature = "test-utils")]
    pub fn page_verifications_for_test(&self) -> usize {
        self.page_verifications.get()
    }

    pub fn copy_telemetry(&self) -> CopyTelemetry {
        self.copies.get()
    }

    fn note_copy(&self, update: impl FnOnce(&mut CopyTelemetry)) {
        let mut value = self.copies.get();
        update(&mut value);
        self.copies.set(value);
    }

    pub fn verification_telemetry(&self) -> VerificationTelemetry {
        self.verification.get()
    }

    fn note_verification(&self, update: impl FnOnce(&mut VerificationTelemetry)) {
        let mut telemetry = self.verification.get();
        update(&mut telemetry);
        self.verification.set(telemetry);
    }

    #[cfg(test)]
    fn note_verification_work(&self, update: impl FnOnce(&mut VerificationWork)) {
        let mut work = self.verification_work.get();
        update(&mut work);
        self.verification_work.set(work);
    }

    #[allow(clippy::disallowed_methods)]
    fn resident_page_matches(&self, data: &[u8], hash: &PageHash) -> bool {
        let started = (self.verification_batch_depth.get() == 0).then(std::time::Instant::now);
        let matches = blake3::hash(data).as_bytes() == hash;
        self.note_verification(|t| {
            t.resident_hashes = t.resident_hashes.saturating_add(1);
            t.resident_bytes = t.resident_bytes.saturating_add(data.len() as u64);
            if let Some(started) = started {
                t.nanos = t.nanos.saturating_add(
                    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
                );
            }
        });
        matches
    }

    pub fn read_transaction(&self) -> ReadTransaction<'_> {
        ReadTransaction {
            store: self,
            verified_pages: VerifiedPages::new(self.pages.len(), self.page_index.len()),
            verified_snapshots: BTreeSet::new(),
        }
    }

    pub fn new(cfg: StoreConfig) -> Store {
        Store {
            cfg,
            #[cfg(test)]
            verification_work: std::cell::Cell::default(),
            #[cfg(test)]
            dense_gfn_scans: std::cell::Cell::default(),
            verification: std::cell::Cell::default(),
            copies: std::cell::Cell::default(),
            verification_batch_depth: std::cell::Cell::new(0),
            store_id: NEXT_STORE_ID
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
                .expect("snapshot store identifiers exhausted"),
            next_id: 0,
            layers: BTreeMap::new(),
            collectible: Vec::new(),
            live_snapshots: 0,
            vm_state_bytes: 0,
            #[allow(clippy::disallowed_types)]
            page_index: HashMap::default(),
            pages: Vec::new(),
            free_pages: Vec::new(),
            zero_hash: *blake3::hash(&[0u8; PAGE_SIZE]).as_bytes(),
            #[cfg(any(test, feature = "test-utils"))]
            page_verifications: std::cell::Cell::new(0),
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
            for &(gfn, pref) in &layer.pages {
                inherited.entry(gfn).or_insert(pref);
            }
            cur = layer.parent;
        }
        let mut dirty_set = BTreeSet::new();
        for &gfn in dirty {
            if gfn >= self.cfg.mem_pages {
                return Err(StoreError::GfnOutOfRange {
                    gfn,
                    mem_pages: self.cfg.mem_pages,
                });
            }
            dirty_set.insert(gfn);
        }

        let timing = self.verification_batch();
        let mut verified_pages = VerifiedPages::new(self.pages.len(), self.page_index.len());
        for (&gfn, &pref) in &inherited {
            self.page_entry(pref, gfn, &mut verified_pages)?;
        }
        drop(timing);
        for &pref in inherited.values() {
            if let PageRef::Data(id) = pref {
                let entry = self.pages[id.get() - 1].as_mut().expect("inherited page");
                entry.refs = entry.refs.saturating_add(1);
            }
        }
        let mem_pages = self.cfg.mem_pages;
        let mut builder = BaseBuilder {
            core: BuilderCore {
                store: self,
                parent: None,
                pages: inherited,
            },
        };
        for gfn in dirty_set {
            let inherited = builder
                .core
                .pages
                .get(&gfn)
                .copied()
                .unwrap_or(PageRef::Zero);
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
        out.copy_from_slice(self.read_transaction().read_page(snap, gfn)?);
        self.note_copy(|t| t.read_copy_bytes = t.read_copy_bytes.saturating_add(out.len() as u64));
        Ok(())
    }

    fn stored_page(&self, snap: SnapshotId, gfn: u64) -> Result<&[u8; PAGE_SIZE], StoreError> {
        match self.resolve(snap.0, gfn) {
            PageRef::Zero => Ok(&ZERO_PAGE),
            PageRef::Data(id) => self
                .pages
                .get(id.get() - 1)
                .and_then(Option::as_ref)
                .and_then(|entry| entry.data.as_ref().try_into().ok())
                .ok_or(StoreError::PageIntegrity { gfn }),
        }
    }

    fn checked_page_ref(&self, pref: PageRef, gfn: u64) -> Result<&[u8; PAGE_SIZE], StoreError> {
        self.note_verification(|t| t.page_requests = t.page_requests.saturating_add(1));
        let result = match pref {
            PageRef::Zero => {
                self.note_verification(|t| t.zero_requests = t.zero_requests.saturating_add(1));
                Ok(&ZERO_PAGE)
            }
            PageRef::Data(id) => match self.pages.get(id.get() - 1).and_then(Option::as_ref) {
                Some(entry) if self.resident_page_matches(&entry.data, &entry.hash) => entry
                    .data
                    .as_ref()
                    .try_into()
                    .map_err(|_| StoreError::PageIntegrity { gfn }),
                Some(_) | None => Err(StoreError::PageIntegrity { gfn }),
            },
        };
        if result.is_err() {
            self.note_verification(|t| t.errors = t.errors.saturating_add(1));
        }
        result
    }

    pub fn page_ref_eq(
        &self,
        a: SnapshotId,
        a_gfn: u64,
        b: SnapshotId,
        b_gfn: u64,
    ) -> Result<bool, StoreError> {
        self.live_layer(a)?;
        self.live_layer(b)?;
        if a_gfn >= self.cfg.mem_pages {
            return Err(StoreError::GfnOutOfRange {
                gfn: a_gfn,
                mem_pages: self.cfg.mem_pages,
            });
        }
        if b_gfn >= self.cfg.mem_pages {
            return Err(StoreError::GfnOutOfRange {
                gfn: b_gfn,
                mem_pages: self.cfg.mem_pages,
            });
        }
        let a_ref = self.resolve(a.0, a_gfn);
        self.checked_page_ref(a_ref, a_gfn)?;
        let b_ref = self.resolve(b.0, b_gfn);
        if a_ref != b_ref {
            self.checked_page_ref(b_ref, b_gfn)?;
        }
        Ok(a_ref == b_ref)
    }

    pub fn diff_pages(
        &self,
        from: Option<SnapshotId>,
        to: SnapshotId,
    ) -> Result<Vec<(u64, &[u8; PAGE_SIZE])>, StoreError> {
        self.restore_pages(from, to, &[])
    }

    pub fn restore_pages(
        &self,
        from: Option<SnapshotId>,
        to: SnapshotId,
        dirty: &[u64],
    ) -> Result<Vec<(u64, &[u8; PAGE_SIZE])>, StoreError> {
        self.read_transaction().restore_pages(from, to, dirty)
    }

    pub fn page_delta(
        &self,
        base: SnapshotId,
        parent: Option<SnapshotId>,
        target: SnapshotId,
    ) -> Result<PageDelta<'_>, StoreError> {
        self.read_transaction().page_delta(base, parent, target)
    }

    pub fn rebase(
        &self,
        origin: SnapshotId,
        near: SnapshotId,
        target: &[(u64, &PageHash)],
    ) -> Result<Rebase, StoreError> {
        self.live_layer(origin)?;
        self.live_layer(near)?;
        let mut previous = None;
        for &(gfn, _) in target {
            if gfn >= self.cfg.mem_pages {
                return Err(StoreError::GfnOutOfRange {
                    gfn,
                    mem_pages: self.cfg.mem_pages,
                });
            }
            if let Some(prior) = previous
                && prior >= gfn
            {
                return Err(StoreError::TargetPageOrder {
                    previous: prior,
                    gfn,
                });
            }
            previous = Some(gfn);
        }
        let mut verified_pages = self.verified_snapshot_pages(near)?;
        let mut rebase = Rebase::default();
        for (index, &(gfn, hash)) in target.iter().enumerate() {
            let (near_hash, _) =
                self.page_entry(self.resolve(near.0, gfn), gfn, &mut verified_pages)?;
            if near_hash != hash {
                rebase.keep.push(index);
            }
        }
        for gfn in self.changed_gfns(origin, near, &[])? {
            if target.binary_search_by_key(&gfn, |row| row.0).is_ok() {
                continue;
            }
            let page = self.resolve(origin.0, gfn);
            if page == self.resolve(near.0, gfn) {
                continue;
            }
            let (hash, data) = self.page_entry(page, gfn, &mut verified_pages)?;
            rebase.origin.push((gfn, *hash, Box::new(*data)));
        }
        Ok(rebase)
    }

    fn page_entry(
        &self,
        page: PageRef,
        gfn: u64,
        verified_pages: &mut VerifiedPages,
    ) -> Result<(&PageHash, &[u8; PAGE_SIZE]), StoreError> {
        self.note_verification(|t| t.page_requests = t.page_requests.saturating_add(1));
        let result = (|| match page {
            PageRef::Zero => {
                self.note_verification(|t| t.zero_requests = t.zero_requests.saturating_add(1));
                Ok((&self.zero_hash, &ZERO_PAGE))
            }
            PageRef::Data(id) => {
                let entry = self
                    .pages
                    .get(id.get() - 1)
                    .and_then(Option::as_ref)
                    .ok_or(StoreError::PageIntegrity { gfn })?;
                let data: &[u8; PAGE_SIZE] = entry
                    .data
                    .as_ref()
                    .try_into()
                    .map_err(|_| StoreError::PageIntegrity { gfn })?;
                if !verified_pages.contains(&id) {
                    #[cfg(any(test, feature = "test-utils"))]
                    self.page_verifications
                        .set(self.page_verifications.get() + 1);
                    if !self.resident_page_matches(data, &entry.hash) {
                        return Err(StoreError::PageIntegrity { gfn });
                    }
                } else {
                    self.note_verification(|t| t.memo_hits = t.memo_hits.saturating_add(1));
                }
                verified_pages.insert(id);
                Ok((&entry.hash, data))
            }
        })();
        if result.is_err() {
            self.note_verification(|t| t.errors = t.errors.saturating_add(1));
        }
        result
    }

    pub fn verify_snapshot_pages(&self, snap: SnapshotId) -> Result<(), StoreError> {
        self.verify_snapshots(&[snap])
    }

    pub fn verify_snapshots(&self, snaps: &[SnapshotId]) -> Result<(), StoreError> {
        self.read_transaction().verify_snapshots(snaps)
    }

    fn verified_snapshot_pages(&self, snap: SnapshotId) -> Result<VerifiedPages, StoreError> {
        let mut verified_pages = VerifiedPages::new(self.pages.len(), self.page_index.len());
        self.verify_snapshot_pages_into(snap, &mut verified_pages)?;
        Ok(verified_pages)
    }

    fn verify_snapshot_pages_into(
        &self,
        snap: SnapshotId,
        verified_pages: &mut VerifiedPages,
    ) -> Result<(), StoreError> {
        let _timing = self.verification_batch();
        #[cfg(test)]
        self.note_verification_work(|work| work.scans += 1);
        let mut seen_gfns = ResolvedGfns::new(self.cfg.mem_pages);
        let mut current = Some(snap.0);
        while let Some(id) = current {
            let layer = self
                .layers
                .get(&id)
                .ok_or(StoreError::UnknownSnapshot(SnapshotId(id, self.store_id)))?;
            #[cfg(test)]
            self.note_verification_work(|work| work.layers += 1);
            for &(gfn, page) in &layer.pages {
                #[cfg(test)]
                self.note_verification_work(|work| work.rows += 1);
                if !seen_gfns.insert(gfn)? {
                    continue;
                }
                self.page_entry(page, gfn, verified_pages)?;
            }
            current = layer.parent;
        }
        #[cfg(test)]
        if matches!(seen_gfns.membership, GfnMembership::Dense(_)) {
            self.dense_gfn_scans.set(self.dense_gfn_scans.get() + 1);
        }
        Ok(())
    }

    fn changed_gfns(
        &self,
        from: SnapshotId,
        to: SnapshotId,
        dirty: &[u64],
    ) -> Result<BTreeSet<u64>, StoreError> {
        let mut left = Some(from.0);
        let mut right = Some(to.0);
        let mut changed_gfns: BTreeSet<_> = dirty.iter().copied().collect();
        while left != right {
            let id = left.max(right).expect("distinct ancestry cursors");
            let Some(layer) = self.layers.get(&id) else {
                return Err(StoreError::UnknownSnapshot(SnapshotId(id, self.store_id)));
            };
            if layer.parent.is_some_and(|parent| parent >= id) {
                return Err(StoreError::UnknownSnapshot(SnapshotId(id, self.store_id)));
            }
            changed_gfns.extend(layer.pages.iter().map(|&(gfn, _)| gfn));
            if left == Some(id) {
                left = layer.parent;
            } else {
                right = layer.parent;
            }
        }
        Ok(changed_gfns)
    }

    #[allow(clippy::disallowed_methods)]
    pub fn vm_state(&self, snap: SnapshotId) -> Result<&[u8], StoreError> {
        let layer = self.live_layer(snap)?;
        let started = std::time::Instant::now();
        let matches = blake3::hash(&layer.vm_state).as_bytes() == &layer.vm_state_hash;
        self.note_verification(|t| {
            t.vm_state_hashes = t.vm_state_hashes.saturating_add(1);
            t.vm_state_bytes = t.vm_state_bytes.saturating_add(layer.vm_state.len() as u64);
            t.vm_state_nanos = t
                .vm_state_nanos
                .saturating_add(u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX));
            t.vm_state_errors = t.vm_state_errors.saturating_add(u64::from(!matches));
        });
        if !matches {
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
            for &(gfn, pref) in &layer.pages {
                resolved.entry(gfn).or_insert(pref);
            }
            cur = layer.parent;
        }

        let timing = self.verification_batch();
        let mut verified_pages = Vec::with_capacity(resolved.len());
        for (&gfn, &pref) in &resolved {
            let data = self.checked_page_ref(pref, gfn)?;
            if matches!(pref, PageRef::Data(_)) {
                verified_pages.push((gfn, data.as_slice()));
            }
        }

        drop(timing);
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

    pub fn release(&mut self, snap: SnapshotId) -> Result<u64, StoreError> {
        let layer = self.live_layer_mut(snap)?;
        layer.refcount -= 1;
        let remaining = layer.refcount;
        let collectible = remaining == 0 && layer.children == 0;
        if remaining == 0 {
            self.live_snapshots -= 1;
        }
        if collectible {
            self.collectible.push(snap.0);
        }
        Ok(remaining)
    }

    pub fn gc(&mut self) -> u64 {
        let mut freed = 0u64;
        while let Some(id) = self.collectible.pop() {
            let layer = self.layers.remove(&id).expect("collectible layer");
            debug_assert_eq!(layer.refcount, 0);
            debug_assert_eq!(layer.children, 0);
            let state_bytes = layer.vm_state.len() as u64;
            self.vm_state_bytes -= state_bytes;
            freed += state_bytes;
            for &(_gfn, pref) in &layer.pages {
                if let PageRef::Data(id) = pref {
                    freed += self.release_page_ref(id);
                }
            }
            if let Some(parent) = layer.parent {
                let ancestor = self.layers.get_mut(&parent).expect("retained parent");
                ancestor.children -= 1;
                if ancestor.children == 0 && ancestor.refcount == 0 {
                    self.collectible.push(parent);
                }
            }
        }
        if self.page_index.is_empty() {
            self.page_index.shrink_to_fit();
            self.pages = Vec::new();
            self.free_pages = Vec::new();
        }
        if self.layers.is_empty() {
            self.collectible = Vec::new();
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
        let snapshots = self.live_snapshots;
        StoreStats {
            snapshots,
            stored_unique_pages: self.page_index.len() as u64,
            logical_pages_total: snapshots.saturating_mul(self.cfg.mem_pages),
            bytes_resident: (self.page_index.len() as u64).saturating_mul(PAGE_SIZE as u64)
                + self.vm_state_bytes,
        }
    }

    #[cfg(feature = "test-utils")]
    pub fn remove_page_for_test(&mut self, snap: SnapshotId, gfn: u64) -> Result<(), StoreError> {
        self.live_layer(snap)?;
        let PageRef::Data(id) = self.resolve(snap.0, gfn) else {
            return Err(StoreError::BuilderMisuse(
                "cannot remove an implicit zero page",
            ));
        };
        let entry = self
            .pages
            .get_mut(id.get() - 1)
            .ok_or(StoreError::BuilderMisuse("page lies outside the store"))?;
        *entry = None;
        Ok(())
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
        let PageRef::Data(id) = self.resolve(snap.0, gfn) else {
            return Err(StoreError::BuilderMisuse(
                "cannot corrupt an implicit zero page",
            ));
        };
        let target = self
            .pages
            .get_mut(id.get() - 1)
            .and_then(Option::as_mut)
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
        if snap.1 != self.store_id {
            return Err(StoreError::UnknownSnapshot(snap));
        }
        self.layers
            .get(&snap.0)
            .filter(|l| l.refcount > 0)
            .ok_or(StoreError::UnknownSnapshot(snap))
    }

    fn live_layer_mut(&mut self, snap: SnapshotId) -> Result<&mut Layer, StoreError> {
        if snap.1 != self.store_id {
            return Err(StoreError::UnknownSnapshot(snap));
        }
        self.layers
            .get_mut(&snap.0)
            .filter(|l| l.refcount > 0)
            .ok_or(StoreError::UnknownSnapshot(snap))
    }

    fn resolve(&self, start: u64, gfn: u64) -> PageRef {
        let mut current = start;
        let result = loop {
            let Some(layer) = self.layers.get(&current) else {
                debug_assert!(false, "dangling parent link");
                break PageRef::Zero;
            };
            if let Ok(index) = layer.pages.binary_search_by_key(&gfn, |&(gfn, _)| gfn) {
                break layer.pages[index].1;
            }
            if let Some(&page) = layer.resolve_cache.borrow().get(&gfn) {
                break page;
            }
            let Some(parent) = layer.parent else {
                break PageRef::Zero;
            };
            current = parent;
        };
        if current != start {
            self.layers[&start]
                .resolve_cache
                .borrow_mut()
                .insert(gfn, result);
        }
        result
    }

    fn intern_page(
        &mut self,
        hash: PageHash,
        data: &[u8],
        gfn: u64,
    ) -> Result<NonZeroUsize, StoreError> {
        self.note_copy(|t| t.intern_calls = t.intern_calls.saturating_add(1));
        if let Some(&id) = self.page_index.get(&hash) {
            let entry = self
                .pages
                .get_mut(id.get() - 1)
                .and_then(Option::as_mut)
                .ok_or(StoreError::PageIntegrity { gfn })?;
            if entry.data.as_ref() != data {
                return Err(StoreError::PageIntegrity { gfn });
            }
            entry.refs = entry.refs.saturating_add(1);
            self.note_copy(|t| t.intern_hits = t.intern_hits.saturating_add(1));
            return Ok(id);
        }
        let entry = PageEntry {
            hash,
            data: data.into(),
            refs: 1,
        };
        self.note_copy(|t| {
            t.intern_copy_bytes = t.intern_copy_bytes.saturating_add(data.len() as u64)
        });
        let id = match self.free_pages.pop() {
            Some(id) => {
                self.pages[id.get() - 1] = Some(entry);
                id
            }
            None => {
                self.pages.push(Some(entry));
                NonZeroUsize::new(self.pages.len()).expect("nonempty page table")
            }
        };
        self.page_index.insert(hash, id);
        Ok(id)
    }

    fn release_page_ref(&mut self, id: NonZeroUsize) -> u64 {
        let slot = &mut self.pages[id.get() - 1];
        match slot {
            Some(entry) if entry.refs > 1 => {
                entry.refs -= 1;
                0
            }
            Some(_) => {
                let entry = slot.take().expect("resident page");
                self.page_index.remove(&entry.hash);
                self.free_pages.push(id);
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
        let is_zero = data == &ZERO_PAGE[..];
        if is_zero && self.parent.is_none() {
            if let Some(PageRef::Data(old)) = self.pages.remove(&gfn) {
                self.store.release_page_ref(old);
            }
            return Ok(());
        }
        let pref = if is_zero {
            PageRef::Zero
        } else {
            let hash = *blake3::hash(data).as_bytes();
            debug_assert_ne!(
                hash, self.store.zero_hash,
                "non-zero page hashed to zero_hash"
            );
            PageRef::Data(self.store.intern_page(hash, data, gfn)?)
        };
        if let Some(PageRef::Data(old)) = self.pages.insert(gfn, pref) {
            self.store.release_page_ref(old);
        }
        Ok(())
    }

    fn write_changed_page(&mut self, gfn: u64, data: &[u8]) -> Result<(), StoreError> {
        if data.len() != PAGE_SIZE {
            return Err(StoreError::BadPageLength { len: data.len() });
        }
        if gfn >= self.store.cfg.mem_pages {
            return Err(StoreError::GfnOutOfRange {
                gfn,
                mem_pages: self.store.cfg.mem_pages,
            });
        }
        if let Some(parent) = self.parent
            && !self.pages.contains_key(&gfn)
            && self
                .store
                .stored_page(SnapshotId(parent, self.store.store_id), gfn)?[..]
                == *data
        {
            let inherited = self.store.resolve(parent, gfn);
            self.store.checked_page_ref(inherited, gfn)?;
            return Ok(());
        }
        self.write_page(gfn, data)
    }

    fn write_hashed_page(
        &mut self,
        gfn: u64,
        data: &[u8],
        expected: &PageHash,
    ) -> Result<(), StoreError> {
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
            if expected != &self.store.zero_hash {
                return Err(StoreError::PageIntegrity { gfn });
            }
            PageRef::Zero
        } else {
            let hash = *blake3::hash(data).as_bytes();
            if &hash != expected {
                return Err(StoreError::PageIntegrity { gfn });
            }
            PageRef::Data(self.store.intern_page(hash, data, gfn)?)
        };
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
            PageRef::Data(id) => {
                let Some(entry) = self.store.pages.get(id.get() - 1).and_then(Option::as_ref)
                else {
                    return Err(StoreError::PageIntegrity { gfn });
                };
                entry.data.as_ref() == data
            }
        };
        if unchanged {
            Ok(())
        } else {
            self.write_page(gfn, data)
        }
    }

    fn seal(mut self, vm_state: Vec<u8>) -> SnapshotId {
        let vm_state_hash = *blake3::hash(&vm_state).as_bytes();
        let pages = std::mem::take(&mut self.pages);
        let mut kept = Vec::with_capacity(pages.len());
        for (gfn, pref) in pages {
            let inherited = match self.parent {
                Some(p) => self.store.resolve(p, gfn),
                None => PageRef::Zero,
            };
            if pref == inherited {
                if let PageRef::Data(id) = pref {
                    self.store.release_page_ref(id);
                }
            } else {
                kept.push((gfn, pref));
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
        if let Some(parent) = self.parent {
            self.store
                .layers
                .get_mut(&parent)
                .expect("live parent")
                .children += 1;
        }
        self.store.live_snapshots += 1;
        self.store.vm_state_bytes += vm_state.len() as u64;
        let id = self.store.next_id;
        self.store.next_id += 1;
        self.store.layers.insert(
            id,
            Layer {
                parent: self.parent,
                pages: kept.into_boxed_slice(),
                vm_state,
                vm_state_hash,
                refcount: 1,
                children: 0,
                chain_len,
                resolve_cache: RefCell::default(),
            },
        );
        SnapshotId(id, self.store.store_id)
    }
}

impl Drop for BuilderCore<'_> {
    fn drop(&mut self) {
        let pages = std::mem::take(&mut self.pages);
        for (_gfn, pref) in pages {
            if let PageRef::Data(id) = pref {
                self.store.release_page_ref(id);
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct Rebase {
    pub keep: Vec<usize>,
    pub origin: Vec<(u64, PageHash, Box<[u8; PAGE_SIZE]>)>,
}

#[derive(Debug, Default)]
pub struct PageDelta<'a> {
    pub changed: Vec<(u64, &'a PageHash, &'a [u8; PAGE_SIZE])>,
    pub reverted: Vec<u64>,
}

impl<'store> ReadTransaction<'store> {
    pub fn read_page(
        &mut self,
        snap: SnapshotId,
        gfn: u64,
    ) -> Result<&'store [u8; PAGE_SIZE], StoreError> {
        self.store.live_layer(snap)?;
        if gfn >= self.store.cfg.mem_pages {
            return Err(StoreError::GfnOutOfRange {
                gfn,
                mem_pages: self.store.cfg.mem_pages,
            });
        }
        let (_, data) = self.store.page_entry(
            self.store.resolve(snap.0, gfn),
            gfn,
            &mut self.verified_pages,
        )?;
        Ok(data)
    }

    fn verify_snapshot(&mut self, snap: SnapshotId) -> Result<(), StoreError> {
        self.store.live_layer(snap)?;
        let key = (snap.0, snap.1);
        if !self.verified_snapshots.contains(&key) {
            self.store
                .verify_snapshot_pages_into(snap, &mut self.verified_pages)?;
            self.verified_snapshots.insert(key);
        }
        Ok(())
    }

    pub fn verify_snapshots(&mut self, snaps: &[SnapshotId]) -> Result<(), StoreError> {
        for &snap in snaps {
            self.verify_snapshot(snap)?;
        }
        Ok(())
    }

    pub fn diff_pages(
        &mut self,
        from: Option<SnapshotId>,
        to: SnapshotId,
    ) -> Result<Vec<(u64, &'store [u8; PAGE_SIZE])>, StoreError> {
        self.restore_pages(from, to, &[])
    }

    fn restore_pages(
        &mut self,
        from: Option<SnapshotId>,
        to: SnapshotId,
        dirty: &[u64],
    ) -> Result<Vec<(u64, &'store [u8; PAGE_SIZE])>, StoreError> {
        let store = self.store;
        store.live_layer(to)?;
        if from == Some(to) && dirty.is_empty() {
            return Ok(Vec::new());
        }
        for &gfn in dirty {
            if gfn >= store.cfg.mem_pages {
                return Err(StoreError::GfnOutOfRange {
                    gfn,
                    mem_pages: store.cfg.mem_pages,
                });
            }
        }

        let Some(from) = from else {
            let _timing = store.verification_batch();
            return (0..store.cfg.mem_pages)
                .map(|gfn| {
                    let (_, page) = store.page_entry(
                        store.resolve(to.0, gfn),
                        gfn,
                        &mut self.verified_pages,
                    )?;
                    Ok((gfn, page))
                })
                .collect();
        };

        store.live_layer(from)?;
        let mut pages = Vec::new();
        for gfn in store.changed_gfns(from, to, dirty)? {
            let (_, page) =
                store.page_entry(store.resolve(to.0, gfn), gfn, &mut self.verified_pages)?;
            pages.push((gfn, page));
        }
        Ok(pages)
    }

    pub fn page_delta(
        &mut self,
        base: SnapshotId,
        parent: Option<SnapshotId>,
        target: SnapshotId,
    ) -> Result<PageDelta<'store>, StoreError> {
        let store = self.store;
        store.live_layer(base)?;
        store.live_layer(target)?;
        let from = parent.unwrap_or(base);
        store.live_layer(from)?;
        let mut delta = PageDelta::default();
        self.verify_snapshot(target)?;
        if from != target {
            self.verify_snapshot(from)?;
        }
        if base != from && base != target {
            self.verify_snapshot(base)?;
        }
        for gfn in store.changed_gfns(from, target, &[])? {
            let page = store.resolve(target.0, gfn);
            if page == store.resolve(from.0, gfn) {
                continue;
            }
            if parent.is_some() && page == store.resolve(base.0, gfn) {
                delta.reverted.push(gfn);
                continue;
            }
            let (hash, data) = store.page_entry(page, gfn, &mut self.verified_pages)?;
            delta.changed.push((gfn, hash, data));
        }
        Ok(delta)
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

    pub fn write_changed_page(&mut self, gfn: u64, data: &[u8]) -> Result<(), StoreError> {
        self.core.write_changed_page(gfn, data)
    }

    pub fn write_hashed_page(
        &mut self,
        gfn: u64,
        data: &[u8],
        hash: &PageHash,
    ) -> Result<(), StoreError> {
        self.core.write_hashed_page(gfn, data, hash)
    }

    pub fn seal(self, vm_state: Vec<u8>) -> SnapshotId {
        self.core.seal(vm_state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_snapshot_verification_skips_only_successful_exact_images_in_one_transaction() {
        let mut store = Store::new(cfg(2));
        let mut builder = store.begin_base();
        builder.write_page(0, &[1; PAGE_SIZE]).unwrap();
        let base = builder.seal(vec![]);
        let parent = snap_with(&mut store, base, &[(1, 2)]);
        let target = snap_with(&mut store, parent, &[(0, 3)]);
        let before = store.verification_telemetry();
        {
            let mut read = store.read_transaction();
            read.verify_snapshots(&[target, target]).unwrap();
            assert_eq!(
                store.verification_work.get(),
                VerificationWork {
                    scans: 1,
                    layers: 3,
                    rows: 3
                }
            );
            assert_eq!(read.verified_snapshots.len(), 1);
            let delta = read.page_delta(base, Some(parent), target).unwrap();
            assert_eq!(delta_summary(&delta), (vec![(0, 3)], vec![]));
            let work = VerificationWork {
                scans: 3,
                layers: 6,
                rows: 6,
            };
            assert_eq!(store.verification_work.get(), work);
            read.verify_snapshots(&[base, parent, target, base])
                .unwrap();
            read.page_delta(base, Some(parent), target).unwrap();
            assert_eq!(store.verification_work.get(), work);
            assert_eq!(read.verified_snapshots.len(), 3);
        }
        let checked = store.verification_telemetry();
        assert_eq!(checked.resident_hashes - before.resident_hashes, 3);
        assert_eq!(
            checked.resident_bytes - before.resident_bytes,
            3 * PAGE_SIZE as u64
        );
        store
            .read_transaction()
            .verify_snapshots(&[target, target])
            .unwrap();
        assert_eq!(
            store.verification_work.get(),
            VerificationWork {
                scans: 4,
                layers: 9,
                rows: 9
            }
        );
        assert_eq!(
            store.verification_telemetry().resident_hashes - checked.resident_hashes,
            2
        );
    }

    #[test]
    fn empty_snapshot_completion_still_checks_store_domain_and_live_handles() {
        let mut store = Store::new(cfg(0));
        let base = store.begin_base().seal(vec![]);
        let child = store.derive(base).unwrap().seal(vec![]);
        store.release(base).unwrap();
        let mut foreign_store = Store::new(cfg(0));
        let foreign = foreign_store.begin_base().seal(vec![]);
        let mut read = store.read_transaction();
        read.verify_snapshots(&[child, child]).unwrap();
        assert_eq!(
            store.verification_work.get(),
            VerificationWork {
                scans: 1,
                layers: 2,
                rows: 0
            }
        );
        assert_eq!(read.verified_snapshots.len(), 1);
        assert!(
            matches!(read.verify_snapshots(&[foreign]), Err(StoreError::UnknownSnapshot(id)) if id == foreign)
        );
        assert!(
            matches!(read.verify_snapshots(&[base]), Err(StoreError::UnknownSnapshot(id)) if id == base)
        );
        assert_eq!(store.verification_work.get().scans, 1);
        assert_eq!(store.verification_telemetry().resident_hashes, 0);
    }

    #[test]
    fn an_overridden_corrupt_ancestor_is_not_certified_by_its_child() {
        let mut store = Store::new(cfg(1));
        let mut builder = store.begin_base();
        builder.write_page(0, &[1; PAGE_SIZE]).unwrap();
        let base = builder.seal(vec![]);
        let child = snap_with(&mut store, base, &[(0, 2)]);
        let PageRef::Data(id) = store.resolve(base.0, 0) else {
            panic!("resident ancestor")
        };
        store.pages[id.get() - 1].as_mut().unwrap().data[7] ^= 1;
        let before = store.verification_telemetry();
        let mut read = store.read_transaction();
        read.verify_snapshots(&[child, child]).unwrap();
        assert!(read.verified_snapshots.contains(&(child.0, child.1)));
        assert!(!read.verified_snapshots.contains(&(base.0, base.1)));
        for _ in 0..2 {
            assert!(matches!(
                read.verify_snapshots(&[base]),
                Err(StoreError::PageIntegrity { gfn: 0 })
            ));
            assert!(!read.verified_snapshots.contains(&(base.0, base.1)));
        }
        assert_eq!(
            store.verification_work.get(),
            VerificationWork {
                scans: 3,
                layers: 4,
                rows: 4
            }
        );
        let after = store.verification_telemetry();
        assert_eq!(after.resident_hashes - before.resident_hashes, 3);
        assert_eq!(after.errors - before.errors, 2);
    }

    #[test]
    fn failed_complete_verification_retries_rows_and_retains_only_checked_resident_slots() {
        let mut store = Store::new(cfg(2));
        let mut builder = store.begin_base();
        builder.write_page(0, &[1; PAGE_SIZE]).unwrap();
        builder.write_page(1, &[2; PAGE_SIZE]).unwrap();
        let snap = builder.seal(vec![]);
        let PageRef::Data(id) = store.resolve(snap.0, 1) else {
            panic!("resident failing page")
        };
        store.pages[id.get() - 1].as_mut().unwrap().data[7] ^= 1;
        let before = store.verification_telemetry();
        let mut read = store.read_transaction();
        for _ in 0..2 {
            assert!(matches!(
                read.verify_snapshots(&[snap]),
                Err(StoreError::PageIntegrity { gfn: 1 })
            ));
            assert!(read.verified_snapshots.is_empty());
        }
        assert_eq!(
            store.verification_work.get(),
            VerificationWork {
                scans: 2,
                layers: 2,
                rows: 4
            }
        );
        let after = store.verification_telemetry();
        assert_eq!(after.resident_hashes - before.resident_hashes, 3);
        assert_eq!(after.memo_hits - before.memo_hits, 1);
        assert_eq!(after.errors - before.errors, 2);
    }

    #[test]
    fn selected_reads_and_empty_differences_do_not_certify_a_complete_image() {
        let mut store = Store::new(cfg(2));
        let mut builder = store.begin_base();
        builder.write_page(0, &[1; PAGE_SIZE]).unwrap();
        builder.write_page(1, &[2; PAGE_SIZE]).unwrap();
        let snap = builder.seal(vec![]);
        let PageRef::Data(id) = store.resolve(snap.0, 1) else {
            panic!("unread resident page")
        };
        store.pages[id.get() - 1].as_mut().unwrap().data[7] ^= 1;
        let mut read = store.read_transaction();
        assert_eq!(read.read_page(snap, 0).unwrap(), &[1; PAGE_SIZE]);
        assert!(read.diff_pages(Some(snap), snap).unwrap().is_empty());
        assert!(read.verified_snapshots.is_empty());
        assert!(matches!(
            read.verify_snapshots(&[snap]),
            Err(StoreError::PageIntegrity { gfn: 1 })
        ));
        assert_eq!(
            store.verification_work.get(),
            VerificationWork {
                scans: 1,
                layers: 1,
                rows: 2
            }
        );
    }

    #[test]
    fn resolved_gfn_membership_promotes_without_losing_zero_or_partial_word_bits() {
        let mut seen = ResolvedGfns::new(129);
        for gfn in 0..63 {
            assert!(seen.insert(gfn).unwrap());
        }
        assert!(matches!(seen.membership, GfnMembership::Sparse { .. }));
        for gfn in [63, 64, 128] {
            assert!(seen.insert(gfn).unwrap());
        }
        assert!(
            matches!(&seen.membership, GfnMembership::Dense(words) if words == &[u64::MAX, 1, 1])
        );
        for gfn in [0, 63, 64, 128] {
            assert!(!seen.insert(gfn).unwrap());
        }
        assert!(matches!(
            seen.insert(129),
            Err(StoreError::GfnOutOfRange {
                gfn: 129,
                mem_pages: 129
            })
        ));
        assert!(matches!(
            seen.insert(u64::MAX),
            Err(StoreError::GfnOutOfRange { .. })
        ));
        assert!(
            matches!(&seen.membership, GfnMembership::Dense(words) if words == &[u64::MAX, 1, 1])
        );
    }

    #[test]
    fn resolved_gfn_membership_bounds_allocation_by_domain_and_encountered_frames() {
        let mut seen = ResolvedGfns::new(8192);
        for gfn in 0..127 {
            assert!(seen.insert(gfn).unwrap());
        }
        assert!(matches!(seen.membership, GfnMembership::Sparse { .. }));
        assert!(seen.insert(8191).unwrap());
        assert!(matches!(&seen.membership, GfnMembership::Dense(words) if words.len() == 128));
        for gfn in (0..127).chain([8191]) {
            assert!(!seen.insert(gfn).unwrap());
        }
        assert!(seen.insert(127).unwrap());
        let maximum = u64::try_from(MAX_DENSE_MEMBERSHIP_WORDS).unwrap() * 64;
        assert!(matches!(
            ResolvedGfns::new(maximum).membership,
            GfnMembership::Sparse {
                dense_words: Some(MAX_DENSE_MEMBERSHIP_WORDS),
                ..
            }
        ));
        assert!(matches!(
            ResolvedGfns::new(maximum + 1).membership,
            GfnMembership::Sparse {
                dense_words: None,
                ..
            }
        ));
        let mut huge = ResolvedGfns::new(u64::MAX);
        for gfn in (0..64).chain([u64::MAX - 1]) {
            assert!(huge.insert(gfn).unwrap());
            assert!(!huge.insert(gfn).unwrap());
        }
        assert!(matches!(
            huge.membership,
            GfnMembership::Sparse {
                dense_words: None,
                ..
            }
        ));
        assert!(matches!(
            huge.insert(u64::MAX),
            Err(StoreError::GfnOutOfRange { .. })
        ));
        let mut empty = ResolvedGfns::new(0);
        assert!(matches!(
            empty.insert(0),
            Err(StoreError::GfnOutOfRange {
                gfn: 0,
                mem_pages: 0
            })
        ));
        assert!(matches!(empty.membership, GfnMembership::Sparse { .. }));
    }

    #[test]
    fn resolved_gfn_bitmap_distinguishes_alias_frames_and_zero_overrides_from_resident_slots() {
        let mut store = Store::new(cfg(65));
        let mut builder = store.begin_base();
        for gfn in 0..65 {
            builder.write_page(gfn, &[7; PAGE_SIZE]).unwrap();
        }
        let base = builder.seal(vec![]);
        let child = snap_with(&mut store, base, &[(0, 0)]);
        assert_eq!(store.page_index.len(), 1);
        let before = store.verification_telemetry();
        {
            let mut read = store.read_transaction();
            read.verify_snapshots(&[child, child]).unwrap();
            assert_eq!(store.dense_gfn_scans.get(), 1);
            assert_eq!(
                store.verification_work.get(),
                VerificationWork {
                    scans: 1,
                    layers: 2,
                    rows: 66
                }
            );
            assert!(
                matches!(&read.verified_pages, VerifiedPages::Sparse { ids, .. } if ids.len() == 1)
            );
            let child_checked = store.verification_telemetry();
            assert_eq!(child_checked.page_requests - before.page_requests, 65);
            assert_eq!(child_checked.zero_requests - before.zero_requests, 1);
            assert_eq!(child_checked.resident_hashes - before.resident_hashes, 1);
            assert_eq!(
                child_checked.resident_bytes - before.resident_bytes,
                PAGE_SIZE as u64
            );
            assert_eq!(child_checked.memo_hits - before.memo_hits, 63);
            read.verify_snapshots(&[base]).unwrap();
            assert_eq!(store.dense_gfn_scans.get(), 2);
            let base_checked = store.verification_telemetry();
            assert_eq!(base_checked.page_requests - child_checked.page_requests, 65);
            assert_eq!(base_checked.memo_hits - child_checked.memo_hits, 65);
            assert_eq!(base_checked.resident_hashes, child_checked.resident_hashes);
            assert_eq!(read.read_page(child, 0).unwrap(), &ZERO_PAGE);
            assert_eq!(read.read_page(child, 64).unwrap(), &[7; PAGE_SIZE]);
        }
        let fresh = store.verification_telemetry();
        store.read_transaction().verify_snapshots(&[child]).unwrap();
        assert_eq!(
            store.verification_telemetry().resident_hashes - fresh.resident_hashes,
            1
        );
        let PageRef::Data(id) = store.resolve(base.0, 64) else {
            panic!("aliased resident page")
        };
        store.pages[id.get() - 1].as_mut().unwrap().data[7] ^= 1;
        assert!(matches!(
            store.read_transaction().verify_snapshots(&[child]),
            Err(StoreError::PageIntegrity { gfn: 1 })
        ));
    }

    #[test]
    fn huge_sparse_frame_domains_preserve_high_frame_overrides_without_bitmap_allocation() {
        let mut store = Store::new(cfg(u64::MAX));
        let mut builder = store.begin_base();
        for gfn in [0, u64::MAX - 1] {
            builder.write_page(gfn, &[7; PAGE_SIZE]).unwrap();
        }
        let base = builder.seal(vec![]);
        let child = snap_with(&mut store, base, &[(u64::MAX - 1, 0)]);
        let before = store.verification_telemetry();
        let mut read = store.read_transaction();
        read.verify_snapshots(&[child]).unwrap();
        assert_eq!(store.dense_gfn_scans.get(), 0);
        let delta = read.page_delta(base, None, child).unwrap();
        assert_eq!(delta_summary(&delta), (vec![(u64::MAX - 1, 0)], vec![]));
        assert_eq!(read.read_page(child, u64::MAX - 1).unwrap(), &ZERO_PAGE);
        assert_eq!(read.read_page(base, u64::MAX - 1).unwrap(), &[7; PAGE_SIZE]);
        assert!(matches!(
            read.read_page(child, u64::MAX),
            Err(StoreError::GfnOutOfRange { .. })
        ));
        assert_eq!(store.dense_gfn_scans.get(), 0);
        let after = store.verification_telemetry();
        assert_eq!(after.resident_hashes - before.resident_hashes, 1);
        assert_eq!(after.errors, before.errors);
    }

    #[test]
    fn explicit_zero_overrides_hide_corrupt_ancestors_without_certifying_them() {
        let mut store = Store::new(cfg(1));
        let mut builder = store.begin_base();
        builder.write_page(0, &[7; PAGE_SIZE]).unwrap();
        let base = builder.seal(vec![]);
        let child = snap_with(&mut store, base, &[(0, 0)]);
        let PageRef::Data(id) = store.resolve(base.0, 0) else {
            panic!("masked resident ancestor")
        };
        store.pages[id.get() - 1].as_mut().unwrap().data[7] ^= 1;
        let before = store.verification_telemetry();
        let mut read = store.read_transaction();
        read.verify_snapshots(&[child]).unwrap();
        let masked = store.verification_telemetry();
        assert_eq!(masked.resident_hashes, before.resident_hashes);
        assert_eq!(masked.zero_requests - before.zero_requests, 1);
        assert!(matches!(
            read.verify_snapshots(&[base]),
            Err(StoreError::PageIntegrity { gfn: 0 })
        ));
        assert_eq!(
            store.verification_telemetry().resident_hashes - before.resident_hashes,
            1
        );
        assert_eq!(store.verification_telemetry().errors - before.errors, 1);
    }

    #[test]
    fn verification_membership_promotes_at_threshold_without_losing_word_boundaries() {
        let mut pages = VerifiedPages::new(128, 128);
        for value in 1..64 {
            let id = NonZeroUsize::new(value).unwrap();
            assert!(!pages.contains(&id));
            pages.insert(id);
        }
        assert!(matches!(pages, VerifiedPages::Sparse { .. }));
        for value in 64..=128 {
            let id = NonZeroUsize::new(value).unwrap();
            assert!(!pages.contains(&id));
            pages.insert(id);
            assert!(pages.contains(&id));
        }
        assert!(matches!(pages, VerifiedPages::Dense(_)));
        for value in 1..=128 {
            assert!(pages.contains(&NonZeroUsize::new(value).unwrap()));
        }
        let fresh = VerifiedPages::new(128, 128);
        assert!(!fresh.contains(&NonZeroUsize::new(128).unwrap()));
        for (slots, live) in [(1024, 64), (usize::MAX, usize::MAX)] {
            let mut sparse = VerifiedPages::new(slots, live);
            for value in 1..=64 {
                sparse.insert(NonZeroUsize::new(value).unwrap());
            }
            assert!(matches!(sparse, VerifiedPages::Sparse { .. }));
        }
    }

    #[test]
    fn verification_membership_waits_for_word_scaled_threshold() {
        let mut pages = VerifiedPages::new(8192, 8192);
        for value in 1..=127 {
            let id = NonZeroUsize::new(value).unwrap();
            assert!(!pages.contains(&id));
            pages.insert(id);
        }
        assert!(matches!(pages, VerifiedPages::Sparse { .. }));
        let last = NonZeroUsize::new(8192).unwrap();
        assert!(!pages.contains(&last));
        pages.insert(last);
        assert!(matches!(&pages, VerifiedPages::Dense(words) if words.len() == 128));
        for value in (1..=127).chain([8192]) {
            assert!(pages.contains(&NonZeroUsize::new(value).unwrap()));
        }
        assert!(!pages.contains(&NonZeroUsize::new(128).unwrap()));
        assert!(!pages.contains(&NonZeroUsize::new(8191).unwrap()));
    }

    #[test]
    fn promoted_read_transactions_verify_aliases_once_and_recheck_after_corruption() {
        let mut store = Store::new(cfg(512));
        let mut builder = store.begin_base();
        for gfn in 0..129 {
            let data = [(gfn + 1) as u8; PAGE_SIZE];
            builder.write_page(gfn, &data).unwrap();
            builder.write_page(256 + gfn, &data).unwrap();
        }
        let snap = builder.seal(vec![]);
        let before = store.verification_telemetry();
        {
            let mut read = store.read_transaction();
            for gfn in 0..65 {
                read.read_page(snap, gfn).unwrap();
            }
            read.verify_snapshots(&[snap, snap]).unwrap();
            assert!(matches!(read.verified_pages, VerifiedPages::Dense(_)));
        }
        let first = store.verification_telemetry();
        assert_eq!(first.resident_hashes - before.resident_hashes, 129);
        store.read_transaction().verify_snapshots(&[snap]).unwrap();
        assert_eq!(
            store.verification_telemetry().resident_hashes - first.resident_hashes,
            129
        );
        let PageRef::Data(id) = store.resolve(snap.0, 128) else {
            panic!("resident page");
        };
        store.pages[id.get() - 1].as_mut().unwrap().data[17] ^= 1;
        assert!(matches!(
            store.read_transaction().verify_snapshots(&[snap]),
            Err(StoreError::PageIntegrity { gfn: 128 })
        ));
    }

    #[test]
    fn fresh_membership_handles_retained_high_slots_and_recycled_storage() {
        let mut store = Store::new(cfg(129));
        let mut builder = store.begin_base();
        for gfn in 0..129 {
            builder
                .write_page(gfn, &[(gfn + 1) as u8; PAGE_SIZE])
                .unwrap();
        }
        let original = builder.seal(vec![]);
        store
            .read_transaction()
            .verify_snapshots(&[original])
            .unwrap();
        let mut builder = store.begin_base();
        builder.write_page(0, &[129; PAGE_SIZE]).unwrap();
        let retained = builder.seal(vec![]);
        store.release(original).unwrap();
        store.gc();
        assert_eq!(store.pages.len(), 129);
        assert_eq!(store.page_index.len(), 1);
        {
            let mut read = store.read_transaction();
            read.verify_snapshots(&[retained]).unwrap();
            assert!(matches!(
                read.verified_pages,
                VerifiedPages::Sparse {
                    dense_words: None,
                    ..
                }
            ));
        }
        let mut builder = store.begin_base();
        builder.write_page(1, &[233; PAGE_SIZE]).unwrap();
        let recycled = builder.seal(vec![]);
        assert_eq!(store.pages.len(), 129);
        let before = store.verification_telemetry();
        let mut read = store.read_transaction();
        read.verify_snapshots(&[retained, recycled]).unwrap();
        assert_eq!(read.read_page(recycled, 1).unwrap(), &[233; PAGE_SIZE]);
        assert_eq!(
            store.verification_telemetry().resident_hashes - before.resident_hashes,
            2
        );
    }

    #[test]
    fn copy_telemetry_counts_only_executed_payload_copies() {
        let mut store = Store::new(cfg(4));
        let mut builder = store.begin_base();
        builder.write_page(0, &[7; PAGE_SIZE]).unwrap();
        builder.write_page(1, &[7; PAGE_SIZE]).unwrap();
        builder.write_page(2, &[0; PAGE_SIZE]).unwrap();
        let snap = builder.seal(vec![]);
        assert_eq!(
            store.copy_telemetry(),
            CopyTelemetry {
                intern_calls: 2,
                intern_hits: 1,
                intern_copy_bytes: PAGE_SIZE as u64,
                read_copy_bytes: 0
            }
        );
        store.read_transaction().read_page(snap, 0).unwrap();
        assert_eq!(store.copy_telemetry().read_copy_bytes, 0);
        let mut out = [0; PAGE_SIZE];
        store.read_page(snap, 0, &mut out).unwrap();
        assert_eq!(out, [7; PAGE_SIZE]);
        assert_eq!(store.copy_telemetry().read_copy_bytes, PAGE_SIZE as u64);
        assert!(store.read_page(snap, 0, &mut [0; 1]).is_err());
        assert_eq!(store.copy_telemetry().read_copy_bytes, PAGE_SIZE as u64);
        let mut builder = store.derive(snap).unwrap();
        builder.write_page(3, &[9; PAGE_SIZE]).unwrap();
        drop(builder);
        assert_eq!(
            store.copy_telemetry().intern_copy_bytes,
            2 * PAGE_SIZE as u64
        );
        assert_eq!(store.copy_telemetry().intern_hits, 1);
    }

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

    fn snap_with(store: &mut Store, parent: SnapshotId, pages: &[(u64, u8)]) -> SnapshotId {
        let mut builder = store.derive(parent).unwrap();
        for &(gfn, fill) in pages {
            builder.write_page(gfn, &[fill; PAGE_SIZE]).unwrap();
        }
        builder.seal(Vec::new())
    }

    fn delta_summary(delta: &PageDelta<'_>) -> (Vec<(u64, u8)>, Vec<u64>) {
        let changed = delta
            .changed
            .iter()
            .map(|&(gfn, hash, data)| {
                assert_eq!(blake3::hash(data).as_bytes(), hash);
                (gfn, data[0])
            })
            .collect();
        (changed, delta.reverted.clone())
    }

    fn transaction_snapshots() -> (Store, SnapshotId, SnapshotId, SnapshotId) {
        let mut store = Store::new(cfg(6));
        let mut builder = store.begin_base();
        for (gfn, fill) in [(0, 1), (1, 1), (2, 2), (4, 4)] {
            builder.write_page(gfn, &[fill; PAGE_SIZE]).unwrap();
        }
        let setup = builder.seal(Vec::new());
        let parent = snap_with(&mut store, setup, &[(0, 3), (2, 0), (3, 5)]);
        let target = snap_with(&mut store, parent, &[(0, 1), (1, 6)]);
        (store, setup, parent, target)
    }

    #[test]
    #[allow(clippy::disallowed_methods)]
    fn verification_batch_errors_restore_standalone_timing_and_exact_counts() {
        let mut store = Store::new(cfg(2));
        let mut builder = store.begin_base();
        builder.write_page(0, &[7; PAGE_SIZE]).unwrap();
        let snap = builder.seal(Vec::new());
        let slot = store.pages.iter().position(Option::is_some).unwrap();
        store.pages[slot].as_mut().unwrap().data[0] ^= 1;
        let failed: Result<(), StoreError> = (|| {
            let mut timing = store.verification_batch();
            timing.started = Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
            store.verify_snapshot_pages(snap)?;
            Ok(())
        })();
        assert!(failed.is_err());
        assert_eq!(store.verification_batch_depth.get(), 0);
        let after_batch = store.verification_telemetry();
        assert_eq!(after_batch.resident_hashes, 1);
        assert_eq!(after_batch.resident_bytes, PAGE_SIZE as u64);
        assert_eq!(after_batch.errors, 1);
        assert!(after_batch.nanos >= 1_000_000_000);
        let failed: Result<(), StoreError> = (|| {
            let mut timing = store.verification_batch();
            timing.started = Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
            store.materialize(snap)?;
            Ok(())
        })();
        assert!(failed.is_err());
        assert_eq!(store.verification_batch_depth.get(), 0);
        let after_materialize = store.verification_telemetry();
        assert_eq!(after_materialize.resident_hashes, 2);
        assert_eq!(after_materialize.resident_bytes, 2 * PAGE_SIZE as u64);
        assert_eq!(after_materialize.errors, 2);
        assert!(after_materialize.nanos - after_batch.nanos >= 1_000_000_000);
        let outer = store.verification_batch();
        let inner = store.verification_batch();
        assert!(outer.started.is_some());
        assert!(inner.started.is_none());
        drop(inner);
        drop(outer);
        assert_eq!(store.verification_batch_depth.get(), 0);
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri does not support file-backed memory mappings")]
    fn materialization_counts_actual_alias_hashes_without_changing_store_stats() {
        let mut store = Store::new(cfg(3));
        let mut builder = store.begin_base();
        builder.write_page(0, &[7; PAGE_SIZE]).unwrap();
        builder.write_page(1, &[7; PAGE_SIZE]).unwrap();
        let snap = builder.seal(Vec::new());
        let stats = store.store_stats();
        let before = store.verification_telemetry();
        let materialized = store.materialize(snap).unwrap();
        let after = store.verification_telemetry();
        assert_eq!(after.resident_hashes - before.resident_hashes, 2);
        assert_eq!(
            after.resident_bytes - before.resident_bytes,
            2 * PAGE_SIZE as u64
        );
        assert_eq!(after.memo_hits, before.memo_hits);
        assert_eq!(&materialized.as_slice()[..PAGE_SIZE], &[7; PAGE_SIZE]);
        assert_eq!(store.store_stats(), stats);
    }

    #[test]
    fn physical_verification_counts_alias_zero_memo_and_corruption_work() {
        fn require_send<T: Send>() {}
        require_send::<Store>();
        let mut store = Store::new(cfg(3));
        let mut builder = store.begin_base();
        builder.write_page(0, &[7; PAGE_SIZE]).unwrap();
        builder.write_page(1, &[7; PAGE_SIZE]).unwrap();
        let snap = builder.seal(vec![4, 5, 6]);
        let before = store.verification_telemetry();
        {
            let mut transaction = store.read_transaction();
            for gfn in [0, 1, 0, 2] {
                transaction.read_page(snap, gfn).unwrap();
            }
        }
        let first = store.verification_telemetry();
        assert_eq!(first.page_requests - before.page_requests, 4);
        assert_eq!(first.memo_hits - before.memo_hits, 2);
        assert_eq!(first.zero_requests - before.zero_requests, 1);
        assert_eq!(first.resident_hashes - before.resident_hashes, 1);
        assert_eq!(
            first.resident_bytes - before.resident_bytes,
            PAGE_SIZE as u64
        );
        store.read_transaction().read_page(snap, 0).unwrap();
        assert_eq!(
            store.verification_telemetry().resident_hashes - first.resident_hashes,
            1
        );
        store.vm_state(snap).unwrap();
        let state = store.verification_telemetry();
        assert_eq!(state.vm_state_hashes - first.vm_state_hashes, 1);
        assert_eq!(state.vm_state_bytes - first.vm_state_bytes, 3);
        let PageRef::Data(id) = store.resolve(snap.0, 0) else {
            panic!("expected resident page")
        };
        store.pages[id.get() - 1].as_mut().unwrap().data[0] = 3;
        let before_error = store.verification_telemetry();
        assert!(store.read_transaction().read_page(snap, 1).is_err());
        let after_error = store.verification_telemetry();
        assert_eq!(after_error.errors - before_error.errors, 1);
        assert_eq!(
            after_error.resident_hashes - before_error.resident_hashes,
            1
        );
        assert_eq!(
            after_error.resident_bytes - before_error.resident_bytes,
            PAGE_SIZE as u64
        );
        store.layers.get_mut(&snap.0).unwrap().vm_state[0] = 99;
        assert!(store.vm_state(snap).is_err());
        assert_eq!(store.verification_telemetry().vm_state_errors, 1);
    }

    #[test]
    fn read_transaction_verifies_each_resident_slot_once_and_renews_trust() {
        let (store, setup, parent, target) = transaction_snapshots();
        store.page_verifications.set(0);
        let physical_before = store.verification_telemetry();
        for operation in 1..=2 {
            let (delta, pages, inherited) = {
                let mut transaction = store.read_transaction();
                let delta = transaction.page_delta(setup, Some(parent), target).unwrap();
                let pages = transaction.diff_pages(Some(setup), target).unwrap();
                for &(gfn, _) in &pages {
                    transaction.read_page(setup, gfn).unwrap();
                }
                for gfn in 0..6 {
                    transaction.read_page(setup, gfn).unwrap();
                }
                transaction
                    .verify_snapshots(&[setup, parent, target])
                    .unwrap();
                let inherited = transaction.read_page(target, 4).unwrap();
                (delta, pages, inherited)
            };
            assert_eq!(delta_summary(&delta), (vec![(1, 6)], vec![0]));
            assert_eq!(
                pages.iter().map(|row| row.0).collect::<Vec<_>>(),
                vec![0, 1, 2, 3]
            );
            assert_eq!(inherited, &[4; PAGE_SIZE]);
            assert_eq!(store.page_verifications.get(), operation * 6);
            assert_eq!(
                store.verification_telemetry().resident_hashes - physical_before.resident_hashes,
                (operation * 6) as u64
            );
            assert_eq!(
                store.verification_telemetry().resident_bytes - physical_before.resident_bytes,
                (operation * 6 * PAGE_SIZE) as u64
            );
        }
    }

    #[test]
    fn a_new_read_transaction_rejects_corruption_omitted_from_a_previous_delta() {
        for (snapshot, gfn) in [(0, 2), (1, 0), (2, 3), (2, 4)] {
            let (mut store, setup, parent, target) = transaction_snapshots();
            let snapshots = [setup, parent, target];
            {
                let mut transaction = store.read_transaction();
                transaction.page_delta(setup, Some(target), target).unwrap();
                transaction.read_page(snapshots[snapshot], gfn).unwrap();
            }
            let PageRef::Data(id) = store.resolve(snapshots[snapshot].0, gfn) else {
                panic!("the planted corruption needs a resident page");
            };
            store.pages[id.get() - 1].as_mut().unwrap().data[0] ^= 1;
            assert!(matches!(
                store
                    .read_transaction()
                    .page_delta(setup, Some(parent), target),
                Err(StoreError::PageIntegrity { .. })
            ));
        }
    }

    #[test]
    fn page_delta_splits_changed_and_reverted_pages() {
        let mut store = Store::new(cfg(8));
        let mut builder = store.begin_base();
        builder.write_page(0, &[1; PAGE_SIZE]).unwrap();
        builder.write_page(1, &[2; PAGE_SIZE]).unwrap();
        let setup = builder.seal(Vec::new());
        let parent = snap_with(&mut store, setup, &[(0, 5), (2, 6), (3, 7)]);
        let target = snap_with(
            &mut store,
            parent,
            &[(0, 1), (1, 9), (2, 6), (3, 0), (4, 8)],
        );

        let delta = store.page_delta(setup, Some(parent), target).unwrap();
        assert_eq!(delta_summary(&delta), (vec![(1, 9), (4, 8)], vec![0, 3]));

        let anchor = store.page_delta(setup, None, target).unwrap();
        assert_eq!(
            delta_summary(&anchor),
            (vec![(1, 9), (2, 6), (4, 8)], Vec::new())
        );

        let sibling = snap_with(&mut store, setup, &[(1, 3)]);
        let across = store.page_delta(setup, Some(sibling), target).unwrap();
        assert_eq!(
            delta_summary(&across),
            (vec![(1, 9), (2, 6), (4, 8)], Vec::new())
        );
        assert!(
            store
                .page_delta(setup, Some(target), target)
                .unwrap()
                .changed
                .is_empty()
        );
    }

    #[test]
    fn page_delta_rejects_corrupted_reverted_page() {
        let mut store = Store::new(cfg(1));
        let mut builder = store.begin_base();
        builder.write_page(0, &[0x11; PAGE_SIZE]).unwrap();
        let base = builder.seal(Vec::new());
        let parent = snap_with(&mut store, base, &[(0, 0x22)]);
        let target = snap_with(&mut store, parent, &[(0, 0x11)]);
        let PageRef::Data(id) = store.resolve(target.0, 0) else {
            panic!("expected reverted resident page");
        };
        store.pages[id.get() - 1].as_mut().unwrap().data[0] ^= 1;

        let mut restored = [0; PAGE_SIZE];
        assert!(matches!(
            store.read_page(target, 0, &mut restored),
            Err(StoreError::PageIntegrity { gfn: 0 })
        ));
        assert!(matches!(
            store.page_delta(base, Some(parent), target),
            Err(StoreError::PageIntegrity { gfn: 0 })
        ));
    }

    #[test]
    fn page_delta_rejects_corrupted_unchanged_page() {
        let mut store = Store::new(cfg(2));
        let mut builder = store.begin_base();
        builder.write_page(0, &[0x11; PAGE_SIZE]).unwrap();
        let base = builder.seal(Vec::new());
        let target = snap_with(&mut store, base, &[(1, 0x22)]);
        let PageRef::Data(id) = store.resolve(target.0, 0) else {
            panic!("expected inherited resident page");
        };
        store.pages[id.get() - 1].as_mut().unwrap().data[0] ^= 1;

        assert!(matches!(
            store.page_delta(base, None, target),
            Err(StoreError::PageIntegrity { gfn: 0 })
        ));
    }

    #[test]
    fn a_changed_page_write_records_only_pages_that_differ_from_the_parent() {
        let mut store = Store::new(cfg(8));
        let mut builder = store.begin_base();
        builder.write_page(0, &[1; PAGE_SIZE]).unwrap();
        builder.write_page(1, &[2; PAGE_SIZE]).unwrap();
        let base = builder.seal(Vec::new());
        let mut builder = store.derive(base).unwrap();
        builder.write_changed_page(0, &[1; PAGE_SIZE]).unwrap();
        builder.write_changed_page(1, &[3; PAGE_SIZE]).unwrap();
        builder.write_changed_page(2, &[0; PAGE_SIZE]).unwrap();
        builder.write_changed_page(3, &[4; PAGE_SIZE]).unwrap();
        let target = builder.seal(Vec::new());

        let delta = store.page_delta(base, None, target).unwrap();
        assert_eq!(delta_summary(&delta), (vec![(1, 3), (3, 4)], Vec::new()));
        let mut page = [0; PAGE_SIZE];
        store.read_page(target, 0, &mut page).unwrap();
        assert_eq!(page, [1; PAGE_SIZE]);

        let mut builder = store.derive(base).unwrap();
        builder.write_page(1, &[5; PAGE_SIZE]).unwrap();
        builder.write_changed_page(1, &[2; PAGE_SIZE]).unwrap();
        let reverted = builder.seal(Vec::new());
        store.read_page(reverted, 1, &mut page).unwrap();
        assert_eq!(page, [2; PAGE_SIZE]);

        assert!(matches!(
            store.derive(target).unwrap().write_changed_page(0, &[1; 8]),
            Err(StoreError::BadPageLength { len: 8 })
        ));
        assert!(matches!(
            store
                .derive(target)
                .unwrap()
                .write_changed_page(8, &[0; PAGE_SIZE]),
            Err(StoreError::GfnOutOfRange { gfn: 8, .. })
        ));
    }

    #[test]
    fn a_rebase_keeps_only_pages_that_differ_from_the_near_snapshot() {
        let mut store = Store::new(cfg(8));
        let mut builder = store.begin_base();
        builder.write_page(0, &[1; PAGE_SIZE]).unwrap();
        builder.write_page(1, &[2; PAGE_SIZE]).unwrap();
        let setup = builder.seal(Vec::new());
        let near = snap_with(&mut store, setup, &[(0, 5), (2, 6), (3, 7)]);
        let hash = |fill: u8| *blake3::hash(&[fill; PAGE_SIZE]).as_bytes();
        let target_hashes = [(0, hash(5)), (1, hash(9)), (4, hash(8))];
        let rows: Vec<_> = target_hashes.iter().map(|(gfn, h)| (*gfn, h)).collect();

        let rebase = store.rebase(setup, near, &rows).unwrap();
        assert_eq!(rebase.keep, vec![1, 2]);
        let origin: Vec<_> = rebase
            .origin
            .iter()
            .map(|(gfn, hash, data)| {
                assert_eq!(hash, blake3::hash(&data[..]).as_bytes());
                (*gfn, data[0])
            })
            .collect();
        assert_eq!(origin, vec![(2, 0), (3, 0)]);

        let mut rebuilt = store.derive(near).unwrap();
        for &index in &rebase.keep {
            let (gfn, fill) = [(0, 5u8), (1, 9), (4, 8)][index];
            rebuilt.write_page(gfn, &[fill; PAGE_SIZE]).unwrap();
        }
        for (gfn, hash, data) in &rebase.origin {
            rebuilt.write_hashed_page(*gfn, &data[..], hash).unwrap();
        }
        let rebuilt = rebuilt.seal(Vec::new());
        let direct = snap_with(&mut store, setup, &[(0, 5), (1, 9), (4, 8)]);
        let mut left = [0u8; PAGE_SIZE];
        let mut right = [0u8; PAGE_SIZE];
        for gfn in 0..8 {
            store.read_page(rebuilt, gfn, &mut left).unwrap();
            store.read_page(direct, gfn, &mut right).unwrap();
            assert_eq!(left, right, "page {gfn}");
        }
    }

    #[test]
    fn rebase_rejects_reordered_and_duplicate_target_pages() {
        let mut store = Store::new(cfg(3));
        let mut builder = store.begin_base();
        builder.write_page(1, &[1; PAGE_SIZE]).unwrap();
        let origin = builder.seal(Vec::new());
        let near = snap_with(&mut store, origin, &[(1, 2)]);
        let changed = *blake3::hash(&[3; PAGE_SIZE]).as_bytes();
        let extra = *blake3::hash(&[4; PAGE_SIZE]).as_bytes();
        let rows = [(1, &changed), (0, &extra)];

        assert!(matches!(
            store.rebase(origin, near, &rows),
            Err(StoreError::TargetPageOrder {
                previous: 1,
                gfn: 0
            })
        ));
        let duplicate = [(1, &changed), (1, &extra)];
        assert!(matches!(
            store.rebase(origin, near, &duplicate),
            Err(StoreError::TargetPageOrder {
                previous: 1,
                gfn: 1
            })
        ));
        let out_of_range = [(3, &store.zero_hash)];
        assert!(matches!(
            store.rebase(origin, near, &out_of_range),
            Err(StoreError::GfnOutOfRange {
                gfn: 3,
                mem_pages: 3
            })
        ));
    }

    #[test]
    fn hashed_writes_reject_a_page_that_does_not_match_its_hash() {
        let mut store = Store::new(cfg(4));
        let setup = store.begin_base().seal(Vec::new());
        let page = [4u8; PAGE_SIZE];
        let hash = *blake3::hash(&page).as_bytes();
        let zero_hash = *blake3::hash(&[0u8; PAGE_SIZE]).as_bytes();
        let mut builder = store.derive(setup).unwrap();
        builder.write_hashed_page(1, &page, &hash).unwrap();
        builder
            .write_hashed_page(2, &[0; PAGE_SIZE], &zero_hash)
            .unwrap();
        let mut planted = page;
        planted[17] ^= 1;
        assert!(matches!(
            builder.write_hashed_page(3, &planted, &hash),
            Err(StoreError::PageIntegrity { gfn: 3 })
        ));
        assert!(matches!(
            builder.write_hashed_page(3, &[0; PAGE_SIZE], &hash),
            Err(StoreError::PageIntegrity { gfn: 3 })
        ));
        let child = builder.seal(Vec::new());
        let mut out = [0u8; PAGE_SIZE];
        store.read_page(child, 1, &mut out).unwrap();
        assert_eq!(out, page);
        store.read_page(child, 3, &mut out).unwrap();
        assert_eq!(out, [0; PAGE_SIZE]);
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
        assert!(b.core.pages.is_empty());
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
        assert!(b.core.pages.is_empty());
        let snap = b.seal(vec![]);
        assert_eq!(store.stats(snap).unwrap().owned_pages, 0);
        assert_eq!(store.store_stats().stored_unique_pages, 0);
    }

    #[test]
    fn restore_pages_borrows_validated_contents_and_merges_dirty_frames() {
        let mut store = Store::new(cfg(4));
        let mut builder = store.begin_base();
        builder.write_page(1, &[7; PAGE_SIZE]).unwrap();
        let base = builder.seal(vec![]);
        let mut builder = store.derive(base).unwrap();
        builder.write_page(2, &[9; PAGE_SIZE]).unwrap();
        let child = builder.seal(vec![]);
        let plan = store
            .restore_pages(Some(base), child, &[3, 1, 2, 1])
            .unwrap();
        assert_eq!(
            plan.iter().map(|&(gfn, _)| gfn).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        for &(gfn, page) in &plan {
            let source = match store.resolve(child.0, gfn) {
                PageRef::Zero => &ZERO_PAGE[..],
                PageRef::Data(id) => store.pages[id.get() - 1].as_ref().unwrap().data.as_ref(),
            };
            assert_eq!(page.as_ptr(), source.as_ptr());
        }
        assert_eq!(plan[0].1, &[7; PAGE_SIZE]);
        assert_eq!(plan[1].1, &[9; PAGE_SIZE]);
        assert_eq!(plan[2].1, &ZERO_PAGE);
        assert!(matches!(
            store.restore_pages(None, child, &[4]),
            Err(StoreError::GfnOutOfRange { gfn: 4, .. })
        ));
        assert_eq!(store.restore_pages(None, child, &[3, 3]).unwrap().len(), 4);
        let PageRef::Data(id) = store.resolve(child.0, 2) else {
            panic!("stored page")
        };
        store.pages[id.get() - 1] = None;
        assert!(matches!(
            store.restore_pages(Some(base), child, &[1]),
            Err(StoreError::PageIntegrity { gfn: 2 })
        ));
    }

    #[test]
    fn restore_and_diff_reject_corrupt_resident_pages() {
        let mut store = Store::new(cfg(2));
        let mut builder = store.begin_base();
        builder.write_page(0, &[0x11; PAGE_SIZE]).unwrap();
        let base = builder.seal(vec![]);
        let target = snap_with(&mut store, base, &[(1, 0x5a)]);
        let PageRef::Data(id) = store.resolve(target.0, 1) else {
            panic!("expected resident page");
        };
        store.pages[id.get() - 1].as_mut().unwrap().data[0] ^= 1;

        assert!(matches!(
            store.restore_pages(Some(base), target, &[]),
            Err(StoreError::PageIntegrity { gfn: 1 })
        ));
        assert!(matches!(
            store.diff_pages(Some(base), target),
            Err(StoreError::PageIntegrity { gfn: 1 })
        ));
        assert!(matches!(
            store.restore_pages(None, target, &[]),
            Err(StoreError::PageIntegrity { gfn: 1 })
        ));

        store.pages[id.get() - 1].as_mut().unwrap().data[0] ^= 1;
        let PageRef::Data(id) = store.resolve(target.0, 0) else {
            panic!("expected inherited resident page");
        };
        store.pages[id.get() - 1].as_mut().unwrap().data[0] ^= 1;
        assert!(matches!(
            store.restore_pages(Some(base), target, &[0]),
            Err(StoreError::PageIntegrity { gfn: 0 })
        ));
    }

    #[test]
    fn inherited_reads_cache_only_the_requested_snapshot() {
        let mut store = Store::new(cfg(4));
        let mut builder = store.begin_base();
        builder.write_page(0, &[9u8; PAGE_SIZE]).unwrap();
        let base = builder.seal(vec![]);
        let mid = store.derive(base).unwrap().seal(vec![]);
        let leaf = store.derive(mid).unwrap().seal(vec![]);
        let mut out = [0u8; PAGE_SIZE];
        store.read_page(leaf, 0, &mut out).unwrap();
        assert_eq!(out, [9u8; PAGE_SIZE]);
        assert!(
            store.layers[&leaf.0]
                .resolve_cache
                .borrow()
                .contains_key(&0)
        );
        assert!(store.layers[&mid.0].resolve_cache.borrow().is_empty());
        store.read_page(leaf, 0, &mut out).unwrap();
        assert_eq!(out, [9u8; PAGE_SIZE]);
    }

    #[test]
    fn inherited_scans_retain_cached_answers_and_preserve_zero_overrides() {
        let pages = if cfg!(miri) { 16 } else { 8192 };
        let mut store = Store::new(cfg(pages));
        let mut builder = store.begin_base();
        builder.write_page(7, &[9; PAGE_SIZE]).unwrap();
        builder.write_page(pages - 1, &[8; PAGE_SIZE]).unwrap();
        let base = builder.seal(vec![]);
        let mut builder = store.derive(base).unwrap();
        builder.write_page(7, &[0; PAGE_SIZE]).unwrap();
        let mut leaf = builder.seal(vec![]);
        for _ in 0..8 {
            leaf = store.derive(leaf).unwrap().seal(vec![]);
        }
        let mut out = [0; PAGE_SIZE];
        for _ in 0..2 {
            for gfn in 0..pages {
                store.read_page(leaf, gfn, &mut out).unwrap();
                assert_eq!(out, [if gfn == pages - 1 { 8 } else { 0 }; PAGE_SIZE]);
            }
            assert_eq!(
                store.layers[&leaf.0].resolve_cache.borrow().len(),
                pages as usize
            );
            for gfn in 0..pages {
                assert!(
                    store.layers[&leaf.0]
                        .resolve_cache
                        .borrow()
                        .contains_key(&gfn)
                );
            }
        }
        store.read_page(base, 7, &mut out).unwrap();
        assert_eq!(out, [9; PAGE_SIZE]);
    }

    #[test]
    fn cached_answers_are_snapshot_local_and_reclaimed_with_their_layer() {
        let mut store = Store::new(cfg(4));
        let mut builder = store.begin_base();
        builder.write_page(0, &[7; PAGE_SIZE]).unwrap();
        let left_base = builder.seal(vec![]);
        let left = store.derive(left_base).unwrap().seal(vec![]);
        let mut builder = store.begin_base();
        builder.write_page(0, &[8; PAGE_SIZE]).unwrap();
        let right_base = builder.seal(vec![]);
        let right = store.derive(right_base).unwrap().seal(vec![]);
        let mut out = [0; PAGE_SIZE];
        for _ in 0..3 {
            store.read_page(left, 0, &mut out).unwrap();
            assert_eq!(out, [7; PAGE_SIZE]);
            store.read_page(right, 0, &mut out).unwrap();
            assert_eq!(out, [8; PAGE_SIZE]);
        }
        assert_eq!(store.layers[&left.0].resolve_cache.borrow().len(), 1);
        assert_eq!(store.layers[&right.0].resolve_cache.borrow().len(), 1);
        store.release(left).unwrap();
        store.release(left_base).unwrap();
        store.gc();
        assert!(!store.layers.contains_key(&left.0));
        assert_eq!(store.layers[&right.0].resolve_cache.borrow().len(), 1);
        let mut builder = store.begin_base();
        builder.write_page(0, &[9; PAGE_SIZE]).unwrap();
        let replacement = builder.seal(vec![]);
        assert!(!store.page_ref_eq(right, 0, replacement, 0).unwrap());
        store.read_page(right, 0, &mut out).unwrap();
        assert_eq!(out, [8; PAGE_SIZE]);
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
            store
                .diff_pages(Some(left), right)
                .unwrap()
                .into_iter()
                .map(|(gfn, page)| (gfn, *page))
                .collect::<Vec<_>>(),
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
            store
                .diff_pages(Some(base), child)
                .unwrap()
                .into_iter()
                .map(|(gfn, page)| (gfn, *page))
                .collect::<Vec<_>>(),
            vec![(0, [0u8; PAGE_SIZE]), (2, [0x20u8; PAGE_SIZE])]
        );
        assert_eq!(
            store
                .diff_pages(Some(child), base)
                .unwrap()
                .into_iter()
                .map(|(gfn, page)| (gfn, *page))
                .collect::<Vec<_>>(),
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
            store
                .diff_pages(None, child)
                .unwrap()
                .into_iter()
                .map(|(gfn, page)| (gfn, *page))
                .collect::<Vec<_>>(),
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
        let unknown = SnapshotId(u64::MAX, store.store_id);

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

    #[test]
    fn skipping_gc_while_a_layer_still_has_refs_frees_the_same_set() {
        fn live_ids(store: &Store) -> BTreeSet<u64> {
            store.layers.keys().copied().collect()
        }

        let mut eager = Store::new(cfg(4));
        let base = eager.begin_base().seal(vec![]);
        eager.retain(base).unwrap();
        let mut child_builder = eager.derive(base).unwrap();
        child_builder.write_page(0, &[1u8; PAGE_SIZE]).unwrap();
        let child = child_builder.seal(vec![]);
        eager.release(base).unwrap();
        eager.gc();
        eager.release(base).unwrap();
        eager.gc();
        eager.release(child).unwrap();
        eager.gc();

        let mut lazy = Store::new(cfg(4));
        let base = lazy.begin_base().seal(vec![]);
        lazy.retain(base).unwrap();
        let mut child_builder = lazy.derive(base).unwrap();
        child_builder.write_page(0, &[1u8; PAGE_SIZE]).unwrap();
        let child = child_builder.seal(vec![]);
        if lazy.release(base).unwrap() == 0 {
            lazy.gc();
        }
        if lazy.release(base).unwrap() == 0 {
            lazy.gc();
        }
        if lazy.release(child).unwrap() == 0 {
            lazy.gc();
        }

        assert_eq!(live_ids(&eager), live_ids(&lazy));
        assert!(live_ids(&lazy).is_empty());
    }

    #[test]
    fn page_ref_eq_matches_content_across_snapshots() {
        let mut store = Store::new(cfg(4));
        let mut base_builder = store.begin_base();
        base_builder.write_page(0, &[0x11; PAGE_SIZE]).unwrap();
        let base = base_builder.seal(vec![]);

        let mut other_builder = store.begin_base();
        other_builder.write_page(0, &[0x11; PAGE_SIZE]).unwrap();
        other_builder.write_page(1, &[0x22; PAGE_SIZE]).unwrap();
        let other = other_builder.seal(vec![]);

        assert!(store.page_ref_eq(base, 0, other, 0).unwrap());
        assert!(!store.page_ref_eq(base, 1, other, 1).unwrap());
        assert!(store.page_ref_eq(base, 1, other, 2).unwrap());
    }

    #[test]
    fn page_ref_eq_reports_integrity_error_for_a_missing_entry() {
        let mut store = Store::new(cfg(2));
        let mut base_builder = store.begin_base();
        base_builder.write_page(0, &[0x33; PAGE_SIZE]).unwrap();
        let base = base_builder.seal(vec![]);
        let other = store.begin_base().seal(vec![]);

        let PageRef::Data(id) = store.resolve(base.0, 0) else {
            panic!("expected a data page ref");
        };
        store.pages[id.get() - 1] = None;

        assert!(matches!(
            store.page_ref_eq(base, 0, other, 0),
            Err(StoreError::PageIntegrity { gfn: 0 })
        ));
        assert!(matches!(
            store.page_ref_eq(other, 0, base, 0),
            Err(StoreError::PageIntegrity { gfn: 0 })
        ));
    }

    #[test]
    fn resident_corruption_is_rejected_by_delta_and_reference_equality() {
        let mut store = Store::new(cfg(2));
        let base = store.begin_base().seal(vec![]);
        let mut builder = store.derive(base).unwrap();
        builder.write_page(1, &[0x5a; PAGE_SIZE]).unwrap();
        let child = builder.seal(vec![]);
        let PageRef::Data(id) = store.resolve(child.0, 1) else {
            panic!("expected resident page");
        };
        store.pages[id.get() - 1].as_mut().unwrap().data[7] ^= 1;

        assert!(matches!(
            store.page_delta(base, None, child),
            Err(StoreError::PageIntegrity { gfn: 1 })
        ));
        assert!(matches!(
            store.page_ref_eq(child, 1, child, 1),
            Err(StoreError::PageIntegrity { gfn: 1 })
        ));
    }

    #[test]
    fn snapshot_verification_checks_inherited_resident_pages() {
        let mut store = Store::new(cfg(2));
        let mut builder = store.begin_base();
        builder.write_page(0, &[0x11; PAGE_SIZE]).unwrap();
        let base = builder.seal(vec![]);
        let child = snap_with(&mut store, base, &[(1, 0x22)]);
        store.verify_snapshot_pages(child).unwrap();

        let PageRef::Data(id) = store.resolve(child.0, 0) else {
            panic!("expected inherited resident page");
        };
        store.pages[id.get() - 1].as_mut().unwrap().data[0] ^= 1;
        assert!(matches!(
            store.verify_snapshot_pages(child),
            Err(StoreError::PageIntegrity { gfn: 0 })
        ));
    }

    #[test]
    fn multiple_snapshot_verification_checks_pages_unique_to_target() {
        let mut store = Store::new(cfg(2));
        let mut builder = store.begin_base();
        builder.write_page(0, &[0x11; PAGE_SIZE]).unwrap();
        let base = builder.seal(vec![]);
        let target = snap_with(&mut store, base, &[(1, 0x22)]);
        store.verify_snapshots(&[base, target]).unwrap();

        let PageRef::Data(id) = store.resolve(target.0, 1) else {
            panic!("expected target resident page");
        };
        store.pages[id.get() - 1].as_mut().unwrap().data[0] ^= 1;
        assert!(matches!(
            store.verify_snapshots(&[base, target]),
            Err(StoreError::PageIntegrity { gfn: 1 })
        ));
    }

    #[test]
    fn rebase_rejects_corrupted_near_pages_before_reusing_them() {
        let mut store = Store::new(cfg(3));
        let mut builder = store.begin_base();
        builder.write_page(0, &[0x11; PAGE_SIZE]).unwrap();
        let origin = builder.seal(vec![]);
        let near = snap_with(&mut store, origin, &[(1, 0x22)]);
        let near_hash = *blake3::hash(&[0x22; PAGE_SIZE]).as_bytes();
        let PageRef::Data(id) = store.resolve(near.0, 1) else {
            panic!("expected resident page");
        };
        store.pages[id.get() - 1].as_mut().unwrap().data[7] ^= 1;

        assert!(matches!(
            store.rebase(origin, near, &[(1, &near_hash)]),
            Err(StoreError::PageIntegrity { gfn: 1 })
        ));
        assert!(matches!(
            store.rebase(origin, near, &[]),
            Err(StoreError::PageIntegrity { gfn: 1 })
        ));

        store.pages[id.get() - 1].as_mut().unwrap().data[7] ^= 1;
        let PageRef::Data(id) = store.resolve(near.0, 0) else {
            panic!("expected inherited resident page");
        };
        store.pages[id.get() - 1].as_mut().unwrap().data[7] ^= 1;
        assert!(matches!(
            store.rebase(origin, near, &[]),
            Err(StoreError::PageIntegrity { gfn: 0 })
        ));
    }

    #[test]
    fn rebase_rejects_corrupted_origin_page_before_copying_it() {
        let mut store = Store::new(cfg(1));
        let mut builder = store.begin_base();
        builder.write_page(0, &[0x11; PAGE_SIZE]).unwrap();
        let origin = builder.seal(vec![]);
        let near = snap_with(&mut store, origin, &[(0, 0x22)]);
        let PageRef::Data(id) = store.resolve(origin.0, 0) else {
            panic!("expected origin resident page");
        };
        store.pages[id.get() - 1].as_mut().unwrap().data[7] ^= 1;

        assert!(matches!(
            store.rebase(origin, near, &[]),
            Err(StoreError::PageIntegrity { gfn: 0 })
        ));
    }

    #[test]
    fn interned_corrupt_page_cannot_replace_valid_write_data() {
        let mut store = Store::new(cfg(2));
        let mut builder = store.begin_base();
        builder.write_page(0, &[0x5a; PAGE_SIZE]).unwrap();
        let snap = builder.seal(vec![]);
        let PageRef::Data(id) = store.resolve(snap.0, 0) else {
            panic!("expected resident page");
        };
        store.pages[id.get() - 1].as_mut().unwrap().data[7] ^= 1;
        let before_refs = store.pages[id.get() - 1].as_ref().unwrap().refs;
        let before_stats = store.store_stats();
        let valid = [0x5a; PAGE_SIZE];
        let hash = *blake3::hash(&valid).as_bytes();

        let mut builder = store.derive(snap).unwrap();
        assert!(matches!(
            builder.write_hashed_page(1, &valid, &hash),
            Err(StoreError::PageIntegrity { gfn: 1 })
        ));
        assert!(matches!(
            builder.write_page(1, &valid),
            Err(StoreError::PageIntegrity { gfn: 1 })
        ));
        drop(builder);
        assert_eq!(
            store.pages[id.get() - 1].as_ref().unwrap().refs,
            before_refs
        );
        assert_eq!(store.store_stats(), before_stats);
    }

    #[test]
    fn changed_write_rejects_corrupt_parent_before_skipping_equal_bytes() {
        let mut store = Store::new(cfg(1));
        let mut builder = store.begin_base();
        builder.write_page(0, &[0x5a; PAGE_SIZE]).unwrap();
        let parent = builder.seal(vec![]);
        let PageRef::Data(id) = store.resolve(parent.0, 0) else {
            panic!("expected resident page");
        };
        store.pages[id.get() - 1].as_mut().unwrap().data[7] ^= 1;
        let bytes = store.pages[id.get() - 1].as_ref().unwrap().data.to_vec();
        let before_stats = store.store_stats();

        let mut builder = store.derive(parent).unwrap();
        assert!(matches!(
            builder.write_changed_page(0, &bytes),
            Err(StoreError::PageIntegrity { gfn: 0 })
        ));
        drop(builder);
        assert_eq!(store.store_stats(), before_stats);
    }

    #[test]
    fn flatten_rejects_corrupt_inherited_page_before_acquiring_refs() {
        let mut store = Store::new(cfg(1));
        let mut builder = store.begin_base();
        builder.write_page(0, &[0x5a; PAGE_SIZE]).unwrap();
        let parent = builder.seal(vec![]);
        let PageRef::Data(id) = store.resolve(parent.0, 0) else {
            panic!("expected resident page");
        };
        store.pages[id.get() - 1].as_mut().unwrap().data[7] ^= 1;
        let before_refs = store.pages[id.get() - 1].as_ref().unwrap().refs;
        let before_stats = store.store_stats();

        assert!(matches!(
            store.flatten_base(parent, &[0; PAGE_SIZE], &[], vec![]),
            Err(StoreError::PageIntegrity { gfn: 0 })
        ));
        assert_eq!(
            store.pages[id.get() - 1].as_ref().unwrap().refs,
            before_refs
        );
        assert_eq!(store.store_stats(), before_stats);
    }

    #[test]
    fn flatten_rejects_missing_content_before_acquiring_inherited_refs() {
        let mut store = Store::new(cfg(2));
        let mut builder = store.begin_base();
        builder.write_page(0, &[1; PAGE_SIZE]).unwrap();
        builder.write_page(1, &[2; PAGE_SIZE]).unwrap();
        let base = builder.seal(vec![]);
        let PageRef::Data(first) = store.resolve(base.0, 0) else {
            unreachable!();
        };
        let PageRef::Data(second) = store.resolve(base.0, 1) else {
            unreachable!();
        };
        let removed = store.pages[second.get() - 1].take();
        let before = store.pages[first.get() - 1].as_ref().unwrap().refs;
        assert!(matches!(
            store.flatten_base(base, &[0; 2 * PAGE_SIZE], &[], vec![]),
            Err(StoreError::PageIntegrity { gfn: 1 })
        ));
        assert_eq!(store.pages[first.get() - 1].as_ref().unwrap().refs, before);
        assert_eq!(store.store_stats().snapshots, 1);
        store.pages[second.get() - 1] = removed;
        store.release(base).unwrap();
        assert_eq!(store.gc(), (2 * PAGE_SIZE) as u64);
    }
}
