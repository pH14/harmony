// SPDX-License-Identifier: AGPL-3.0-or-later

mod budget;
pub mod extent;
mod lock_work;
pub mod segments;

use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Bound,
    sync::{Arc, Mutex, Weak},
};

use sha2::{Digest, Sha256};

pub use budget::{
    MemoryNeeds, MemoryPlan, RESERVE_BYTES, headroom_bytes, measured_budget, plan_memory,
};
pub use segments::{CommittedExtent, PageSegments, SEGMENT_BYTES, WritableExtent};

pub const ANCHOR_DEPTH: u32 = 32;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Namespace([u8; 32], KeyMode);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum KeyMode {
    Prefix,
    Exact,
}

impl Namespace {
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    #[must_use]
    pub fn new(parts: &[&[u8]]) -> Self {
        Self::with_mode(parts, KeyMode::Prefix)
    }

    #[must_use]
    pub fn exact(parts: &[&[u8]]) -> Self {
        Self::with_mode(parts, KeyMode::Exact)
    }

    fn with_mode(parts: &[&[u8]], mode: KeyMode) -> Self {
        let mut digest = Sha256::new();
        digest.update(execution_contract::Assurance::ordinary().identity());
        digest.update(match mode {
            KeyMode::Prefix => b"consonance-snapshot-cache-namespace-v1".as_slice(),
            KeyMode::Exact => b"consonance-snapshot-cache-exact-namespace-v1".as_slice(),
        });
        for part in parts {
            digest.update((part.len() as u64).to_le_bytes());
            digest.update(part);
        }
        Self(digest.finalize().into(), mode)
    }
}

#[derive(Debug)]
pub struct Lease {
    owner: Arc<()>,
    entry: u64,
    key_len: usize,
    depth: u32,
}

impl PartialEq for Lease {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.owner, &other.owner)
            && self.entry == other.entry
            && self.key_len == other.key_len
            && self.depth == other.depth
    }
}

impl Eq for Lease {}

impl Lease {
    #[must_use]
    pub fn key_len(&self) -> usize {
        self.key_len
    }

    #[must_use]
    pub fn depth(&self) -> u32 {
        self.depth
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("the snapshot cache budget of {budget} bytes cannot hold {needed} more bytes")]
    Refused { needed: usize, budget: usize },
    #[error("the snapshot cache holds no entry for lease {0}")]
    UnknownLease(u64),
    #[error("a snapshot cache parent belongs to another namespace")]
    NamespaceMismatch,
    #[error("a snapshot cache lease belongs to another index")]
    ForeignLease,
    #[error("a snapshot cache extent belongs to another index")]
    ForeignExtent,
    #[error("malformed snapshot cache extent: {0}")]
    Malformed(&'static str),
    #[error(
        "snapshot cache key {:?} in namespace {:?} names target {:?}, not {:?}",
        .evidence.key, .evidence.namespace,
        .evidence.existing.target_commitment, .evidence.published.target_commitment
    )]
    DivergentDuplicate { evidence: Arc<CacheDivergence> },
    #[error("snapshot cache namespace {namespace:?} is quarantined after a determinism violation")]
    Quarantined {
        namespace: Namespace,
        evidence: Arc<CacheDivergence>,
    },
    #[error("snapshot cache segment: {0}")]
    Segment(#[from] std::io::Error),
}

#[derive(Clone, Debug)]
pub struct CacheWitness {
    pub target_commitment: [u8; 32],
    pub chain: Vec<CommittedExtent>,
}

#[derive(Clone, Debug)]
pub struct CacheDivergence {
    pub namespace: Namespace,
    pub key: Vec<u8>,
    pub existing: CacheWitness,
    pub published: CacheWitness,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CacheStats {
    pub budget: usize,
    pub lowest_budget: usize,
    pub charged: usize,
    pub stored: usize,
    pub segments: usize,
    pub entries: usize,
    pub anchors: usize,
    pub leased: usize,
    pub lookups: u64,
    pub hits: u64,
    pub publishes: u64,
    pub duplicates: u64,
    pub refusals: u64,
    pub evictions: u64,
    pub shrinks: u64,
}

impl CacheStats {
    #[must_use]
    pub fn counters(&self) -> Vec<(&'static str, u64)> {
        vec![
            ("budget_bytes", self.budget as u64),
            ("lowest_budget_bytes", self.lowest_budget as u64),
            ("charged_bytes", self.charged as u64),
            ("store_bytes", self.stored as u64),
            ("segments", self.segments as u64),
            ("entries", self.entries as u64),
            ("anchors", self.anchors as u64),
            ("leased", self.leased as u64),
            ("lookups", self.lookups),
            ("hits", self.hits),
            ("publishes", self.publishes),
            ("duplicates", self.duplicates),
            ("refusals", self.refusals),
            ("evictions", self.evictions),
            ("shrinks", self.shrinks),
        ]
    }
}

pub trait CacheIndex: Send + Sync + std::fmt::Debug {
    fn divergences(&self) -> Vec<Arc<CacheDivergence>>;
    fn verify_namespace(&self, namespace: Namespace) -> Result<(), CacheError>;
    fn verify_integrity(&self) -> Result<(), CacheError>;
    fn verify_lease(&self, lease: &Lease) -> Result<(), CacheError>;

    fn lookup(&self, namespace: Namespace, key: &[u8]) -> Option<Lease>;
    fn extent(&self, len: usize) -> Result<WritableExtent, CacheError>;
    fn publish(
        &self,
        namespace: Namespace,
        key: &[u8],
        parent: Option<&Lease>,
        extent: WritableExtent,
        target_commitment: [u8; 32],
        cost: u64,
    ) -> Result<Lease, CacheError>;
    fn target_commitment(&self, lease: &Lease) -> Result<[u8; 32], CacheError>;
    fn chain(&self, lease: &Lease) -> Result<Vec<CommittedExtent>, CacheError>;
    fn release(&self, lease: Lease);
    fn revoke(&self, lease: Lease);
    fn report_store(&self, holder: u64, bytes: u64) -> bool;
    fn forget_store(&self, holder: u64);
    fn stats(&self) -> CacheStats;
    fn lock_counters(&self) -> Vec<(String, u64)> {
        Vec::new()
    }
}

#[derive(Debug)]
struct Entry {
    namespace: Namespace,
    key: Vec<u8>,
    parent: Option<u64>,
    depth: u32,
    extent: CommittedExtent,
    target_commitment: [u8; 32],
    cost: u64,
    leases: u32,
    children: u32,
    priority: u128,
}

impl Entry {
    fn worth(&self) -> u128 {
        (u128::from(self.cost) << 32) / self.extent.span().2.max(1) as u128
    }
}

#[derive(Debug)]
struct State {
    owner: Arc<()>,
    planned: usize,
    budget: usize,
    segments: PageSegments,
    entries: BTreeMap<u64, Entry>,
    keys: BTreeMap<Namespace, BTreeMap<Vec<u8>, u64>>,
    evictable: BTreeSet<(u128, u64)>,
    divergences: BTreeMap<Namespace, Arc<CacheDivergence>>,
    stores: BTreeMap<u64, usize>,
    fitted: BTreeMap<u64, usize>,
    stored: usize,
    next_id: u64,
    floor: u128,
    stats: CacheStats,
}

impl State {
    fn verify_namespace(&self, namespace: Namespace) -> Result<(), CacheError> {
        if let Some(evidence) = self.divergences.get(&namespace) {
            Err(CacheError::Quarantined {
                namespace,
                evidence: Arc::clone(evidence),
            })
        } else {
            Ok(())
        }
    }

    fn chain(&self, mut cursor: Option<u64>) -> Result<Vec<CommittedExtent>, CacheError> {
        let mut chain = Vec::new();
        while let Some(id) = cursor {
            let entry = self.entries.get(&id).ok_or(CacheError::UnknownLease(id))?;
            chain.push(entry.extent.clone());
            cursor = entry.parent;
        }
        chain.reverse();
        Ok(chain)
    }

    fn lookup_key(&self, namespace: Namespace, key: &[u8]) -> Option<u64> {
        self.verify_namespace(namespace).ok()?;
        let keys = self.keys.get(&namespace)?;
        if namespace.1 == KeyMode::Exact {
            return keys.get(key).copied();
        }
        let mut probe = key;
        loop {
            let (found, id) = keys
                .range::<[u8], _>((Bound::Unbounded, Bound::Included(probe)))
                .next_back()?;
            if probe.starts_with(found) {
                return Some(*id);
            }
            let common = found
                .iter()
                .zip(probe)
                .take_while(|(left, right)| left == right)
                .count();
            probe = &probe[..common];
        }
    }

    fn lease(&mut self, id: u64) -> Result<Lease, CacheError> {
        let floor = self.floor;
        let entry = self
            .entries
            .get_mut(&id)
            .ok_or(CacheError::UnknownLease(id))?;
        self.evictable.remove(&(entry.priority, id));
        entry.priority = floor.saturating_add(entry.worth());
        entry.leases += 1;
        Ok(Lease {
            owner: Arc::clone(&self.owner),
            entry: id,
            key_len: entry.key.len(),
            depth: entry.depth,
        })
    }

    fn settle(&mut self, id: u64) {
        if let Some(entry) = self.entries.get(&id)
            && !self.divergences.contains_key(&entry.namespace)
            && entry.leases == 0
            && entry.children == 0
        {
            self.evictable.insert((entry.priority, id));
        }
    }

    fn evict(&mut self, id: u64) {
        if self
            .entries
            .get(&id)
            .is_some_and(|entry| self.divergences.contains_key(&entry.namespace))
        {
            return;
        }

        let Some(entry) = self.entries.remove(&id) else {
            return;
        };
        self.evictable.remove(&(entry.priority, id));
        self.floor = self.floor.max(entry.priority);
        if let Some(keys) = self.keys.get_mut(&entry.namespace) {
            keys.remove(&entry.key);
            if keys.is_empty() {
                self.keys.remove(&entry.namespace);
            }
        }
        if let Some(parent) = entry.parent
            && let Some(parent_entry) = self.entries.get_mut(&parent)
        {
            parent_entry.children -= 1;
            self.settle(parent);
        }
        self.segments.free_committed(entry.extent);
        self.stats.evictions += 1;
    }

    fn revoke(&mut self, id: u64) {
        if !self.entries.contains_key(&id) {
            return;
        }
        let mut children = BTreeMap::<u64, Vec<u64>>::new();
        for (&child, entry) in &self.entries {
            if let Some(parent) = entry.parent {
                children.entry(parent).or_default().push(child);
            }
        }
        let mut descendants = Vec::new();
        let mut pending = vec![id];
        while let Some(parent) = pending.pop() {
            descendants.push(parent);
            if let Some(children) = children.get(&parent) {
                pending.extend(children.iter().copied());
            }
        }
        for descendant in descendants.into_iter().rev() {
            self.evict(descendant);
        }
    }

    fn make_room(&mut self, needed: usize) -> Result<(), CacheError> {
        let room = self.budget.saturating_sub(self.stored);
        let refused = CacheError::Refused {
            needed,
            budget: room,
        };
        if needed > room {
            return Err(refused);
        }
        while self.segments.charged() + needed > room || self.segments.full(needed) {
            let Some(&(_, id)) = self.evictable.first() else {
                return Err(refused);
            };
            self.evict(id);
        }
        Ok(())
    }

    fn measure(&mut self, headroom: Option<u64>) {
        if let Some(headroom) = headroom {
            self.budget = measured_budget(
                self.planned,
                self.segments.charged().saturating_add(self.stored),
                headroom,
            );
            self.stats.lowest_budget = self.stats.lowest_budget.min(self.budget);
        }
    }

    fn over_budget(&self) -> bool {
        self.segments.charged().saturating_add(self.stored) > self.budget
    }

    fn set_store(&mut self, holder: u64, bytes: Option<usize>) {
        let old = match bytes {
            Some(bytes) => self.stores.insert(holder, bytes),
            None => self.stores.remove(&holder),
        };
        self.stored = self
            .stored
            .saturating_sub(old.unwrap_or(0))
            .saturating_add(bytes.unwrap_or(0));
        if bytes.is_none() {
            self.fitted.remove(&holder);
        }
    }
}

#[derive(Clone, Debug)]
pub struct LocalIndex {
    state: Arc<Mutex<State>>,
    lock_work: Arc<lock_work::LockWork>,
    headroom: Option<fn() -> Option<u64>>,
}

impl LocalIndex {
    #[must_use]
    pub fn new(budget: usize) -> Self {
        Self::with_segment_bytes(budget, SEGMENT_BYTES)
    }

    #[must_use]
    pub fn measured(budget: usize) -> Self {
        Self::new(budget).with_headroom(headroom_bytes)
    }

    #[must_use]
    pub fn with_headroom(mut self, headroom: fn() -> Option<u64>) -> Self {
        self.headroom = Some(headroom);
        self
    }

    #[must_use]
    pub fn with_segment_bytes(budget: usize, segment_bytes: usize) -> Self {
        Self::with_segments(
            budget,
            PageSegments::new(segment_bytes, segments::descriptor_room()),
        )
    }

    #[must_use]
    pub fn with_segments(budget: usize, segments: PageSegments) -> Self {
        Self {
            lock_work: Arc::default(),
            headroom: None,
            state: Arc::new(Mutex::new(State {
                owner: Arc::new(()),
                planned: budget,
                budget,
                segments,
                entries: BTreeMap::new(),
                keys: BTreeMap::new(),
                evictable: BTreeSet::new(),
                divergences: BTreeMap::new(),
                stores: BTreeMap::new(),
                fitted: BTreeMap::new(),
                stored: 0,
                next_id: 0,
                floor: 0,
                stats: CacheStats {
                    lowest_budget: budget,
                    ..CacheStats::default()
                },
            })),
        }
    }

    fn state(&self, site: lock_work::Site) -> lock_work::Guard<'_, State> {
        self.lock_work.lock(&self.state, site)
    }

    fn headroom(&self) -> Option<u64> {
        self.headroom.and_then(|read| read())
    }

    fn abandon(state: Weak<Mutex<State>>, work: Arc<lock_work::LockWork>) -> segments::Abandon {
        Box::new(move |reclamation| {
            if let Some(state) = state.upgrade() {
                work.lock(&state, lock_work::Site::Abandon)
                    .segments
                    .reclaim(reclamation);
            }
        })
    }
}

impl CacheIndex for LocalIndex {
    fn divergences(&self) -> Vec<Arc<CacheDivergence>> {
        self.state(lock_work::Site::Divergences)
            .divergences
            .values()
            .cloned()
            .collect()
    }

    fn verify_namespace(&self, namespace: Namespace) -> Result<(), CacheError> {
        self.state(lock_work::Site::VerifyNamespace)
            .verify_namespace(namespace)
    }

    fn verify_integrity(&self) -> Result<(), CacheError> {
        let state = self.state(lock_work::Site::VerifyIntegrity);
        match state.divergences.keys().next() {
            Some(&namespace) => state.verify_namespace(namespace),
            None => Ok(()),
        }
    }

    fn verify_lease(&self, lease: &Lease) -> Result<(), CacheError> {
        let state = self.state(lock_work::Site::VerifyLease);
        if !Arc::ptr_eq(&lease.owner, &state.owner) {
            return Err(CacheError::ForeignLease);
        }
        let entry = state
            .entries
            .get(&lease.entry)
            .ok_or(CacheError::UnknownLease(lease.entry))?;
        state.verify_namespace(entry.namespace)
    }

    fn lookup(&self, namespace: Namespace, key: &[u8]) -> Option<Lease> {
        let mut state = self.state(lock_work::Site::Lookup);
        state.stats.lookups += 1;
        let id = state.lookup_key(namespace, key)?;
        state.stats.hits += 1;
        state.lease(id).ok()
    }

    fn extent(&self, len: usize) -> Result<WritableExtent, CacheError> {
        let headroom = self.headroom();
        let mut state = self.state(lock_work::Site::Extent);
        state.measure(headroom);
        let needed =
            segments::rounded(len.max(1)).ok_or(CacheError::Malformed("extent size overflow"))?;
        if let Err(error) = state.make_room(needed) {
            state.stats.refusals += 1;
            return Err(error);
        }
        let mut extent = state.segments.allocate(needed)?;
        extent.set_abandon(Self::abandon(
            Arc::downgrade(&self.state),
            Arc::clone(&self.lock_work),
        ));
        Ok(extent)
    }

    fn publish(
        &self,
        namespace: Namespace,
        key: &[u8],
        parent: Option<&Lease>,
        extent: WritableExtent,
        target_commitment: [u8; 32],
        cost: u64,
    ) -> Result<Lease, CacheError> {
        let mut state = self.state(lock_work::Site::Publish);
        if !state.segments.owns(&extent) {
            return Err(CacheError::ForeignExtent);
        }
        let committed = extent.commit();
        if let Err(error) = state.verify_namespace(namespace) {
            state.segments.free_committed(committed);
            return Err(error);
        }
        let validated = (|| {
            let Some(lease) = parent else { return Ok(0) };
            if !Arc::ptr_eq(&lease.owner, &state.owner) {
                return Err(CacheError::ForeignLease);
            }
            let parent = state
                .entries
                .get(&lease.entry)
                .ok_or(CacheError::UnknownLease(lease.entry))?;
            if parent.namespace != namespace {
                return Err(CacheError::NamespaceMismatch);
            }
            if namespace.1 == KeyMode::Prefix
                && (!key.starts_with(&parent.key) || key.len() <= parent.key.len())
            {
                return Err(CacheError::Malformed("parent key is not a prefix"));
            }
            parent
                .depth
                .checked_add(1)
                .ok_or(CacheError::Malformed("cache chain depth overflows"))
        })();
        let depth = match validated {
            Ok(depth) => depth,
            Err(error) => {
                state.segments.free_committed(committed);
                return Err(error);
            }
        };
        if let Some(&id) = state.keys.get(&namespace).and_then(|keys| keys.get(key)) {
            let existing_commitment = state.entries[&id].target_commitment;
            if existing_commitment != target_commitment {
                let existing = CacheWitness {
                    target_commitment: existing_commitment,
                    chain: state.chain(Some(id))?,
                };
                let mut chain = state.chain(parent.map(|lease| lease.entry))?;
                chain.push(committed);
                let evidence = Arc::new(CacheDivergence {
                    namespace,
                    key: key.to_vec(),
                    existing,
                    published: CacheWitness {
                        target_commitment,
                        chain,
                    },
                });
                state.divergences.insert(namespace, Arc::clone(&evidence));
                let pinned: BTreeSet<_> = state
                    .entries
                    .iter()
                    .filter(|(_, entry)| entry.namespace == namespace)
                    .map(|(&id, entry)| (entry.priority, id))
                    .collect();
                state.evictable.retain(|entry| !pinned.contains(entry));
                return Err(CacheError::DivergentDuplicate { evidence });
            }
            state.segments.free_committed(committed);
            state.stats.duplicates += 1;
            return state.lease(id);
        }
        if let Some(parent) = parent {
            state.entries.get_mut(&parent.entry).unwrap().children += 1;
        }
        let id = state.next_id;
        state.next_id += 1;
        let mut entry = Entry {
            namespace,
            key: key.to_vec(),
            parent: parent.map(|lease| lease.entry),
            depth,
            extent: committed,
            target_commitment,
            cost,
            leases: 1,
            children: 0,
            priority: 0,
        };
        entry.priority = state.floor.saturating_add(entry.worth());
        state.entries.insert(id, entry);
        state
            .keys
            .entry(namespace)
            .or_default()
            .insert(key.to_vec(), id);
        state.stats.publishes += 1;
        Ok(Lease {
            owner: Arc::clone(&state.owner),
            entry: id,
            key_len: key.len(),
            depth,
        })
    }

    fn target_commitment(&self, lease: &Lease) -> Result<[u8; 32], CacheError> {
        let state = self.state(lock_work::Site::TargetCommitment);
        if !Arc::ptr_eq(&lease.owner, &state.owner) {
            return Err(CacheError::ForeignLease);
        }
        let entry = state
            .entries
            .get(&lease.entry)
            .ok_or(CacheError::UnknownLease(lease.entry))?;
        state.verify_namespace(entry.namespace)?;
        Ok(entry.target_commitment)
    }

    fn chain(&self, lease: &Lease) -> Result<Vec<CommittedExtent>, CacheError> {
        let state = self.state(lock_work::Site::Chain);
        if !Arc::ptr_eq(&lease.owner, &state.owner) {
            return Err(CacheError::ForeignLease);
        }
        let entry = state
            .entries
            .get(&lease.entry)
            .ok_or(CacheError::UnknownLease(lease.entry))?;
        state.verify_namespace(entry.namespace)?;
        state.chain(Some(lease.entry))
    }

    fn release(&self, lease: Lease) {
        let mut state = self.state(lock_work::Site::Release);
        if !Arc::ptr_eq(&lease.owner, &state.owner) {
            return;
        }
        if let Some(entry) = state.entries.get_mut(&lease.entry) {
            entry.leases -= 1;
            state.settle(lease.entry);
        }
    }

    fn revoke(&self, lease: Lease) {
        let mut state = self.state(lock_work::Site::Revoke);
        if Arc::ptr_eq(&lease.owner, &state.owner) {
            state.revoke(lease.entry);
        }
    }

    fn report_store(&self, holder: u64, bytes: u64) -> bool {
        let headroom = self.headroom();
        let mut state = self.state(lock_work::Site::ReportStore);
        let bytes = usize::try_from(bytes).unwrap_or(usize::MAX);
        state.set_store(holder, Some(bytes));
        state.measure(headroom);
        while state.over_budget() {
            let Some(&(_, id)) = state.evictable.first() else {
                break;
            };
            state.evict(id);
        }
        let over = state.over_budget();
        if !over {
            state.fitted.insert(holder, bytes);
        }
        let grew = state
            .fitted
            .get(&holder)
            .is_none_or(|&fitted| bytes > fitted);
        let largest = state
            .stores
            .iter()
            .max_by_key(|&(&id, &bytes)| (bytes, std::cmp::Reverse(id)))
            .map(|(&id, _)| id);
        let shrink = over && (grew || largest == Some(holder));
        if shrink {
            state.stats.shrinks += 1;
        }
        shrink
    }

    fn forget_store(&self, holder: u64) {
        self.state(lock_work::Site::ForgetStore)
            .set_store(holder, None);
    }

    fn stats(&self) -> CacheStats {
        let state = self.state(lock_work::Site::Stats);
        CacheStats {
            budget: state.planned,
            charged: state.segments.charged(),
            stored: state.stored,
            segments: state.segments.segments(),
            entries: state.entries.len(),
            anchors: state
                .entries
                .values()
                .filter(|entry| entry.parent.is_none())
                .count(),
            leased: state
                .entries
                .values()
                .filter(|entry| entry.leases > 0)
                .count(),
            ..state.stats
        }
    }
    fn lock_counters(&self) -> Vec<(String, u64)> {
        self.lock_work.counters()
    }
}

#[cfg(test)]
mod tests {
    use super::extent::{HashedPage, encode, read_extent, resolve};
    use super::segments::PAGE;
    use super::*;

    #[test]
    fn cache_namespace_binds_the_ordinary_unqualified_assurance_policy() {
        for (namespace, domain) in [
            (
                Namespace::new(&[b"fixture"]),
                b"consonance-snapshot-cache-namespace-v1".as_slice(),
            ),
            (
                Namespace::exact(&[b"fixture"]),
                b"consonance-snapshot-cache-exact-namespace-v1".as_slice(),
            ),
        ] {
            let mut old = Sha256::new();
            old.update(domain);
            old.update(7_u64.to_le_bytes());
            old.update(b"fixture");
            let legacy = Namespace(old.finalize().into(), namespace.1);
            assert_ne!(*namespace.as_bytes(), *legacy.as_bytes());
            let index = LocalIndex::new(1 << 20);
            let retained = put(&index, legacy, b"same-input", None, &[(0, 1)], &[]).unwrap();
            assert!(index.lookup(namespace, b"same-input").is_none());
            let current = put(&index, namespace, b"same-input", None, &[(0, 2)], &[]).unwrap();
            assert_eq!(index.stats().entries, 2);
            index.release(retained);
            index.release(current);
            let mut expected = Sha256::new();
            expected.update(execution_contract::Assurance::ordinary().identity());
            expected.update(domain);
            expected.update(7_u64.to_le_bytes());
            expected.update(b"fixture");
            assert_eq!(*namespace.as_bytes(), <[u8; 32]>::from(expected.finalize()));
        }
    }

    #[test]
    fn lock_work_is_shared_only_by_clones_and_abandon_does_not_keep_state_alive() {
        let index = LocalIndex::with_segment_bytes(PAGE * 4, PAGE * 4);
        let cloned = index.clone();
        let separate = LocalIndex::with_segment_bytes(PAGE * 4, PAGE * 4);
        assert!(index.lock_counters().is_empty());
        assert!(index.lookup(ns("missing"), b"key").is_none());
        assert_eq!(cloned.lock_counters(), index.lock_counters());
        assert!(separate.lock_counters().is_empty());
        let before = index.stats();
        let counters = index.lock_counters();
        assert_eq!(index.stats(), before);
        assert!(!counters.is_empty());
        let extent = cloned.extent(PAGE).unwrap();
        drop(extent);
        let values = index
            .lock_counters()
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        assert_eq!(values["abandon.attempts"], 1);
        assert_eq!(values["abandon.completed"], 1);
        let extent = index.extent(PAGE).unwrap();
        let weak = Arc::downgrade(&index.state);
        let work = Arc::clone(&index.lock_work);
        drop(index);
        drop(cloned);
        assert!(weak.upgrade().is_none());
        drop(extent);
        let values = work.counters().into_iter().collect::<BTreeMap<_, _>>();
        assert_eq!(values["abandon.completed"], 1);
    }

    fn ns(name: &str) -> Namespace {
        Namespace::new(&[name.as_bytes()])
    }

    fn put(
        index: &LocalIndex,
        namespace: Namespace,
        key: &[u8],
        parent: Option<&Lease>,
        pages: &[(u64, u8)],
        reverted: &[u64],
    ) -> Result<Lease, CacheError> {
        let owned: Vec<_> = pages
            .iter()
            .map(|&(gfn, fill)| (gfn, [fill; 32], [fill; PAGE]))
            .collect();
        let rows: Vec<HashedPage<'_>> = owned
            .iter()
            .map(|(gfn, hash, data)| (*gfn, hash, data))
            .collect();
        let encoded = encode(&rows, reverted, key)?;
        let mut extent = index.extent(encoded.len())?;
        encoded.write(extent.bytes_mut())?;
        let target_commitment = Sha256::digest(extent.bytes_mut()).into();
        index.publish(namespace, key, parent, extent, target_commitment, 1)
    }

    fn put_empty(index: &LocalIndex, key: &[u8], cost: u64) -> Result<Lease, CacheError> {
        let encoded = encode(&[], &[], key).unwrap();
        let mut extent = index.extent(encoded.len())?;
        encoded.write(extent.bytes_mut()).unwrap();
        let target_commitment = Sha256::digest(extent.bytes_mut()).into();
        index.publish(ns("a"), key, None, extent, target_commitment, cost)
    }

    fn put_costing(index: &LocalIndex, key: &[u8], cost: u64) {
        let lease = put_empty(index, key, cost).unwrap();
        index.release(lease);
    }

    fn held(index: &LocalIndex, key: &[u8]) -> bool {
        index
            .state(lock_work::Site::Stats)
            .keys
            .get(&ns("a"))
            .is_some_and(|keys| keys.contains_key(key))
    }

    #[test]
    fn revoking_a_published_entry_invalidates_existing_leases_and_descendants() {
        let index = LocalIndex::new(1 << 20);
        let root = put_empty(&index, b"r", 1).unwrap();
        let other = index.lookup(ns("a"), b"r").unwrap();
        let child = put(&index, ns("a"), b"r1", Some(&root), &[], &[]).unwrap();
        index.revoke(root);
        assert!(index.lookup(ns("a"), b"r1").is_none());
        assert!(index.lookup(ns("a"), b"r").is_none());
        assert!(matches!(
            index.chain(&other),
            Err(CacheError::UnknownLease(_))
        ));
        assert!(matches!(
            index.chain(&child),
            Err(CacheError::UnknownLease(_))
        ));
        index.release(other);
        index.release(child);
        assert_eq!((index.stats().entries, index.stats().leased), (0, 0));
        let replacement = put_empty(&index, b"r", 1).unwrap();
        assert_eq!(index.chain(&replacement).unwrap().len(), 1);
        index.release(replacement);
    }

    fn resolved(index: &LocalIndex, lease: &Lease) -> (Vec<(u64, u8)>, Vec<u8>) {
        let chain = index.chain(lease).unwrap();
        let deltas: Vec<_> = chain
            .iter()
            .map(|extent| read_extent(extent.bytes()).unwrap())
            .collect();
        let resolved = resolve(&deltas).unwrap();
        (
            resolved
                .pages()
                .iter()
                .map(|&(gfn, _, data)| (gfn, data[0]))
                .collect(),
            resolved.sidecar.to_vec(),
        )
    }

    #[test]
    fn exact_keys_resolve_the_supplied_parent_and_never_match_a_prefix() {
        let index = LocalIndex::new(1 << 20);
        let space = Namespace::exact(&[b"opaque"]);
        let prefix = Namespace::new(&[b"opaque"]);
        assert_ne!(space.as_bytes(), prefix.as_bytes());
        let root = put(&index, space, &[9; 32], None, &[(0, 1)], &[]).unwrap();
        let child = put(&index, space, &[3; 32], Some(&root), &[(1, 2)], &[]).unwrap();
        assert_eq!(child.depth(), 1);
        assert_eq!(resolved(&index, &child).0, vec![(0, 1), (1, 2)]);
        assert_eq!(index.chain(&child).unwrap().len(), 2);
        let found = index.lookup(space, &[3; 32]).unwrap();
        assert_eq!(found, child);
        index.release(found);
        let mut longer = vec![3; 32];
        longer.push(0);
        for probe in [&longer[..], &[3; 31][..], &[][..]] {
            assert!(index.lookup(space, probe).is_none());
        }
        assert!(index.lookup(prefix, &[3; 32]).is_none());
        index.release(child);
        index.release(root);
    }

    #[test]
    fn exact_parent_checks_reject_foreign_revoked_and_other_namespace_leases() {
        let index = LocalIndex::new(1 << 20);
        let other = LocalIndex::new(1 << 20);
        let space = Namespace::exact(&[b"opaque"]);
        let foreign = put(&other, space, &[9; 32], None, &[(0, 1)], &[]).unwrap();
        assert!(matches!(
            put(&index, space, &[3; 32], Some(&foreign), &[(1, 2)], &[]),
            Err(CacheError::ForeignLease)
        ));
        let wrong = put(
            &index,
            Namespace::exact(&[b"other"]),
            &[9; 32],
            None,
            &[(0, 1)],
            &[],
        )
        .unwrap();
        assert!(matches!(
            put(&index, space, &[3; 32], Some(&wrong), &[(1, 2)], &[]),
            Err(CacheError::NamespaceMismatch)
        ));
        let prefix = put(
            &index,
            Namespace::new(&[b"opaque"]),
            &[9; 32],
            None,
            &[(0, 1)],
            &[],
        )
        .unwrap();
        assert!(matches!(
            put(&index, space, &[3; 32], Some(&prefix), &[(1, 2)], &[]),
            Err(CacheError::NamespaceMismatch)
        ));
        let root = put(&index, space, &[9; 32], None, &[(0, 1)], &[]).unwrap();
        let revoked = index.lookup(space, &[9; 32]).unwrap();
        index.revoke(root);
        assert!(matches!(
            put(&index, space, &[3; 32], Some(&revoked), &[(1, 2)], &[]),
            Err(CacheError::UnknownLease(_))
        ));
        index.release(revoked);
        index.release(prefix);
        index.release(wrong);
        other.release(foreign);
    }

    #[test]
    fn exact_duplicate_ancestor_does_not_reparent_or_create_a_cycle() {
        let index = LocalIndex::new(1 << 20);
        let space = Namespace::exact(&[b"opaque"]);
        let root = put(&index, space, &[9; 32], None, &[(0, 1)], &[]).unwrap();
        let child = put(&index, space, &[3; 32], Some(&root), &[(0, 2)], &[]).unwrap();
        let duplicate = put(&index, space, &[9; 32], Some(&child), &[(0, 1)], &[]).unwrap();
        assert_eq!(duplicate, root);
        assert_eq!(index.chain(&duplicate).unwrap().len(), 1);
        assert_eq!(index.chain(&child).unwrap().len(), 2);
        assert_eq!(resolved(&index, &duplicate).0, vec![(0, 1)]);
        assert_eq!(resolved(&index, &child).0, vec![(0, 2)]);
        assert_eq!(index.stats().entries, 2);
        index.release(duplicate);
        index.release(child);
        index.release(root);
    }

    #[test]
    fn conflicting_exact_key_quarantines_both_actual_witness_chains() {
        let index = LocalIndex::new(1 << 20);
        let space = Namespace::exact(&[b"opaque"]);
        let parent = put(&index, space, &[9; 32], None, &[(0, 1)], &[]).unwrap();
        let first = put(&index, space, &[3; 32], Some(&parent), &[(1, 2)], &[]).unwrap();
        let error = put(&index, space, &[3; 32], Some(&parent), &[(1, 3)], &[]).unwrap_err();
        let CacheError::DivergentDuplicate { evidence } = error else {
            panic!("expected divergent duplicate")
        };
        assert_eq!(evidence.existing.chain.len(), 2);
        assert_eq!(evidence.published.chain.len(), 2);
        assert_ne!(
            evidence.existing.target_commitment,
            evidence.published.target_commitment
        );
        assert!(matches!(
            index.target_commitment(&first),
            Err(CacheError::Quarantined { .. })
        ));
        assert!(index.lookup(space, &[3; 32]).is_none());
        assert!(matches!(
            put(&index, space, &[7; 32], Some(&parent), &[(1, 4)], &[]),
            Err(CacheError::Quarantined { .. })
        ));
        index.release(first);
        index.release(parent);
    }

    #[test]
    fn lookup_returns_the_longest_cached_prefix() {
        let index = LocalIndex::new(1 << 20);
        let space = ns("a");
        let root = put(&index, space, b"ab", None, &[(0, 1)], &[]).unwrap();
        let child = put(&index, space, b"abcd", Some(&root), &[(1, 2)], &[]).unwrap();
        put(&index, space, b"abce", Some(&root), &[(1, 3)], &[]).unwrap();
        for (probe, expected) in [
            (&b"abcdzz"[..], Some(4)),
            (b"abcd", Some(4)),
            (b"abcf", Some(2)),
            (b"abc", Some(2)),
            (b"aa", None),
            (b"", None),
        ] {
            let found = index.lookup(space, probe);
            assert_eq!(found.as_ref().map(Lease::key_len), expected, "{probe:?}");
            if let Some(lease) = found {
                index.release(lease);
            }
        }
        assert!(index.lookup(ns("b"), b"abcd").is_none());
        assert_eq!(child.depth(), 1);
        assert_eq!(
            resolved(&index, &child),
            (vec![(0, 1), (1, 2)], b"abcd".to_vec())
        );
    }

    #[test]
    fn unrelated_parent_cannot_supply_pages_to_another_action_prefix() {
        let index = LocalIndex::new(1 << 20);
        let space = ns("a");
        let parent = put(&index, space, b"ab", None, &[(0, 1)], &[]).unwrap();
        for key in [&b"ac"[..], &b"a"[..]] {
            let result = put(&index, space, key, Some(&parent), &[(1, 2)], &[]);
            assert!(matches!(
                result,
                Err(CacheError::Malformed("parent key is not a prefix"))
            ));
            assert!(index.lookup(space, key).is_none());
        }
        let child = put(&index, space, b"abcd", Some(&parent), &[(1, 2)], &[]).unwrap();
        assert_eq!(resolved(&index, &child).0, vec![(0, 1), (1, 2)]);
    }

    #[test]
    fn foreign_lease_cannot_alias_an_entry_with_the_same_local_id() {
        let a = LocalIndex::new(1 << 20);
        let b = LocalIndex::new(1 << 20);
        let space = ns("a");
        let a_root = put(&a, space, b"ab", None, &[(0, 1)], &[]).unwrap();
        let b_root = put(&b, space, b"ab", None, &[(0, 2)], &[]).unwrap();
        assert_ne!(a_root, b_root);
        assert!(matches!(b.chain(&a_root), Err(CacheError::ForeignLease)));
        assert!(matches!(
            put(&b, space, b"abcd", Some(&a_root), &[(1, 3)], &[]),
            Err(CacheError::ForeignLease)
        ));
        let leased = b.stats().leased;
        b.release(a_root);
        assert_eq!(b.stats().leased, leased);
        assert_eq!(resolved(&b, &b_root).0, vec![(0, 2)]);
    }

    #[test]
    fn foreign_extent_cannot_be_published_into_another_index() {
        let a = LocalIndex::new(1 << 20);
        let b = LocalIndex::new(1 << 20);
        let space = ns("a");
        let _b_root = put(&b, space, b"b", None, &[(0, 2)], &[]).unwrap();
        for key in [&b"a"[..], &b"b"[..]] {
            let encoded = encode(&[], &[], &[]).unwrap();
            let mut extent = a.extent(encoded.len()).unwrap();
            encoded.write(extent.bytes_mut()).unwrap();
            assert!(matches!(
                b.publish(space, key, None, extent, [0; 32], 1),
                Err(CacheError::ForeignExtent)
            ));
            assert_eq!(a.stats().charged, 0);
        }
        assert!(b.lookup(space, b"a").is_none());
        assert_eq!(b.stats().entries, 1);
    }

    #[test]
    fn a_duplicate_key_keeps_the_first_entry_and_frees_the_second_extent() {
        let index = LocalIndex::with_segment_bytes(1 << 20, 4 * PAGE);
        let space = ns("a");
        let first = put(&index, space, b"k", None, &[(0, 1)], &[]).unwrap();
        let charged = index.stats().charged;
        let second = put(&index, space, b"k", None, &[(0, 1)], &[]).unwrap();
        assert_eq!(first, second);
        assert_eq!(resolved(&index, &second).0, vec![(0, 1)]);
        let stats = index.stats();
        assert_eq!((stats.entries, stats.duplicates), (1, 1));
        assert!(stats.charged >= charged);
    }

    #[test]
    fn a_divergent_duplicate_key_must_not_return_the_first_lease() {
        let index = LocalIndex::with_segment_bytes(1 << 20, 4 * PAGE);
        let space = ns("a");
        let first = put(&index, space, b"k", None, &[(0, 1)], &[]).unwrap();
        let existing_commitment =
            index.state(lock_work::Site::Stats).entries[&first.entry].target_commitment;
        let page = [2; PAGE];
        let encoded = encode(&[(0, &[2; 32], &page)], &[], b"k").unwrap();
        let mut extent = index.extent(encoded.len()).unwrap();
        encoded.write(extent.bytes_mut()).unwrap();
        let published_commitment = Sha256::digest(extent.bytes_mut()).into();
        let result = index.publish(space, b"k", None, extent, published_commitment, 1);
        assert!(matches!(
            result,
            Err(CacheError::DivergentDuplicate { evidence }) if evidence.namespace == space
                && evidence.key == b"k"
                && evidence.existing.target_commitment == existing_commitment
                && evidence.published.target_commitment == published_commitment
        ));
        assert!(matches!(
            index.chain(&first),
            Err(CacheError::Quarantined { .. })
        ));
        assert!(index.lookup(space, b"k").is_none());
        let evidence = index.divergences();
        assert_eq!(evidence.len(), 1);
        assert_eq!(
            resolve(&[read_extent(evidence[0].existing.chain[0].bytes()).unwrap()])
                .unwrap()
                .pages()[0]
                .2[0],
            1
        );
        assert_eq!(
            resolve(&[read_extent(evidence[0].published.chain[0].bytes()).unwrap()])
                .unwrap()
                .pages()[0]
                .2[0],
            2
        );
        assert_eq!(index.stats().entries, 1);
        assert_eq!(index.stats().duplicates, 0);
    }

    #[test]
    fn quarantine_blocks_inflight_dependents_and_preserves_both_witness_chains() {
        let index = LocalIndex::new(1 << 20);
        let space = ns("disputed");
        let parent = put(&index, space, b"a", None, &[(0, 1)], &[]).unwrap();
        let first = put(&index, space, b"ab", Some(&parent), &[(1, 2)], &[]).unwrap();
        let descendant = put(&index, space, b"abc", Some(&first), &[(2, 3)], &[]).unwrap();
        let pending = index.extent(encode(&[], &[], &[]).unwrap().len()).unwrap();
        let pending_anchor = index.extent(encode(&[], &[], &[]).unwrap().len()).unwrap();
        let healthy = put(&index, ns("healthy"), b"a", None, &[(0, 9)], &[]).unwrap();
        index.release(healthy);
        let error = put(&index, space, b"ab", Some(&parent), &[(1, 4)], &[]).unwrap_err();
        let evidence = match error {
            CacheError::DivergentDuplicate { evidence, .. } => evidence,
            error => panic!("wrong error: {error}"),
        };
        assert_eq!(evidence.namespace, space);
        assert_eq!(evidence.key, b"ab");
        for lease in [&parent, &first, &descendant] {
            assert!(matches!(
                index.chain(lease),
                Err(CacheError::Quarantined { .. })
            ));
            assert!(matches!(
                index.verify_lease(lease),
                Err(CacheError::Quarantined { .. })
            ));
        }
        assert!(index.lookup(space, b"abc").is_none());
        assert!(matches!(
            index.publish(space, b"abcd", Some(&descendant), pending, [1; 32], 1),
            Err(CacheError::Quarantined { .. })
        ));
        assert!(matches!(
            index.publish(space, b"z", None, pending_anchor, [1; 32], 1),
            Err(CacheError::Quarantined { .. })
        ));
        index.revoke(first);
        index.release(parent);
        index.release(descendant);
        assert!(index.report_store(1, 1 << 20));
        assert!(index.lookup(ns("healthy"), b"a").is_none());
        let retained = match index.verify_integrity().unwrap_err() {
            CacheError::Quarantined { evidence, .. } => evidence,
            error => panic!("wrong error: {error}"),
        };
        assert!(Arc::ptr_eq(&retained, &evidence));
        drop(index);
        for (witness, expected) in [(&retained.existing, 2), (&retained.published, 4)] {
            let deltas: Vec<_> = witness
                .chain
                .iter()
                .map(|extent| read_extent(extent.bytes()).unwrap())
                .collect();
            let resolved = resolve(&deltas).unwrap();
            assert_eq!(
                resolved
                    .pages()
                    .iter()
                    .map(|&(gfn, _, data)| (gfn, data[0]))
                    .collect::<Vec<_>>(),
                vec![(0, 1), (1, expected)]
            );
            assert_eq!(resolved.sidecar, b"ab");
        }
    }

    #[test]
    fn equal_target_with_different_parent_delta_keeps_the_first_lease() {
        let index = LocalIndex::new(1 << 20);
        let space = ns("a");
        let parent = put(&index, space, b"a", None, &[(0, 1)], &[]).unwrap();
        let target_commitment = [7; 32];
        let publish = |parent: Option<&Lease>, pages: &[(u64, u8)]| {
            let owned: Vec<_> = pages
                .iter()
                .map(|&(gfn, fill)| (gfn, [fill; 32], [fill; PAGE]))
                .collect();
            let rows: Vec<HashedPage<'_>> = owned
                .iter()
                .map(|(gfn, hash, data)| (*gfn, hash, data))
                .collect();
            let encoded = encode(&rows, &[], &[]).unwrap();
            let mut extent = index.extent(encoded.len()).unwrap();
            encoded.write(extent.bytes_mut()).unwrap();
            index.publish(space, b"ab", parent, extent, target_commitment, 1)
        };
        let anchor = publish(None, &[(0, 1), (1, 2)]).unwrap();
        let duplicate = publish(Some(&parent), &[(1, 2)]).unwrap();
        assert_eq!(anchor, duplicate);
        assert_eq!(resolved(&index, &duplicate).0, vec![(0, 1), (1, 2)]);
        assert_eq!(index.stats().duplicates, 1);
    }

    #[test]
    fn eviction_takes_an_unleased_leaf_and_refuses_when_all_are_held() {
        let per_entry = PAGE;
        let index = LocalIndex::with_segment_bytes(3 * per_entry, per_entry);
        let space = ns("a");
        let a = put(&index, space, b"a", None, &[(0, 1)], &[]).unwrap();
        let b = put(&index, space, b"ab", Some(&a), &[(1, 1)], &[]).unwrap();
        let c = put(&index, space, b"c", None, &[(2, 1)], &[]).unwrap();
        assert!(matches!(
            put(&index, space, b"d", None, &[(3, 1)], &[]),
            Err(CacheError::Refused { .. })
        ));
        index.release(a);
        assert!(
            put(&index, space, b"d", None, &[(3, 1)], &[]).is_err(),
            "a parent with a live child stays"
        );
        index.release(b);
        index.release(c);
        let d = put(&index, space, b"d", None, &[(3, 1)], &[]).unwrap();
        assert!(
            !held(&index, b"ab"),
            "of two equal leaves the earlier one goes first"
        );
        let a = index.lookup(space, b"a").unwrap();
        assert_eq!(a.key_len(), 1);
        let stats = index.stats();
        assert_eq!((stats.evictions, stats.refusals), (1, 2));
        assert!(stats.charged <= stats.budget);
        index.release(a);
        index.release(d);
    }

    #[test]
    fn eviction_takes_the_least_cost_per_byte_and_ages_what_stays() {
        let per_entry = PAGE;
        let index = LocalIndex::with_segment_bytes(2 * per_entry, per_entry);
        put_costing(&index, b"x", 10);
        put_costing(&index, b"y", 1);
        put_costing(&index, b"z", 1);
        assert!(held(&index, b"x"), "the costlier leaf stays");
        assert!(!held(&index, b"y"), "the cheaper leaf goes first");
        let mut outlived = 0;
        for step in 0..32u8 {
            put_costing(&index, &[step], 1);
            if !held(&index, b"x") {
                break;
            }
            outlived += 1;
        }
        assert!(!held(&index, b"x"), "an unused costly leaf ages out");
        assert!((2..31).contains(&outlived), "{outlived}");
    }

    #[test]
    fn a_new_segment_past_the_limit_waits_for_eviction_to_retire_one() {
        let index = LocalIndex::with_segments(1 << 20, PageSegments::new(2 * PAGE, 1));
        let hold = |key: &[u8]| put_empty(&index, key, 1);
        let x = hold(b"x").unwrap();
        let y = hold(b"y").unwrap();
        assert!(matches!(hold(b"z"), Err(CacheError::Refused { .. })));
        index.release(y);
        assert!(
            hold(b"z").is_err(),
            "a leased extent keeps the only segment alive"
        );
        index.release(x);
        let z = hold(b"z").unwrap();
        let stats = index.stats();
        assert_eq!((stats.segments, stats.evictions, stats.refusals), (1, 2, 2));
        index.release(z);
    }

    #[test]
    fn a_segment_that_a_chain_copy_holds_counts_against_the_limit() {
        let index = LocalIndex::with_segments(1 << 20, PageSegments::new(PAGE, 1));
        let x = put_empty(&index, b"x", 1).unwrap();
        let copy = index.chain(&x).unwrap();
        index.release(x);
        assert!(matches!(
            put_empty(&index, b"y", 1),
            Err(CacheError::Refused { .. })
        ));
        assert_eq!(index.stats().segments, 0);
        drop(copy);
        let y = put_empty(&index, b"y", 1).unwrap();
        index.release(y);
    }

    #[test]
    fn deep_chains_resolve_from_their_anchor_and_siblings_share_parents() {
        let index = LocalIndex::new(64 << 20);
        let space = ns("a");
        let mut key = vec![0u8];
        let mut lease = put(&index, space, &key, None, &[(0, 1), (1, 1)], &[]).unwrap();
        let mut leases = Vec::new();
        for step in 1..40u8 {
            key.push(step);
            let (parent, pages) = if lease.depth() + 1 >= ANCHOR_DEPTH {
                (None, vec![(0, step), (1, 1)])
            } else {
                (Some(&lease), vec![(0, step)])
            };
            let next = put(&index, space, &key, parent, &pages, &[]).unwrap();
            leases.push(std::mem::replace(&mut lease, next));
        }
        assert_eq!(lease.depth(), 39 - ANCHOR_DEPTH);
        assert_eq!(
            index.chain(&lease).unwrap().len(),
            usize::try_from(lease.depth()).unwrap() + 1
        );
        assert_eq!(resolved(&index, &lease).0, vec![(0, 39), (1, 1)]);
        let base = &leases[3];
        let left = put(
            &index,
            space,
            &[0, 1, 2, 3, b'L'],
            Some(base),
            &[(2, 7)],
            &[1],
        )
        .unwrap();
        let right = put(
            &index,
            space,
            &[0, 1, 2, 3, b'R'],
            Some(base),
            &[(2, 8)],
            &[],
        )
        .unwrap();
        assert_eq!(resolved(&index, &left).0, vec![(0, 3), (2, 7)]);
        assert_eq!(resolved(&index, &right).0, vec![(0, 3), (1, 1), (2, 8)]);
    }

    #[test]
    fn an_abandoned_extent_returns_its_segment() {
        let index = LocalIndex::with_segment_bytes(1 << 20, PAGE);
        let first = index.extent(PAGE).unwrap();
        let second = index.extent(PAGE).unwrap();
        assert_eq!(index.stats().segments, 2);
        drop(first);
        assert_eq!(index.stats().segments, 1);
        drop(second);
        assert_eq!(index.stats().charged, 0);
        assert_eq!(index.stats().segments, 0);
    }

    #[test]
    fn eviction_reclaims_the_open_segment_once_its_extents_are_gone() {
        let per_entry = PAGE;
        let index = LocalIndex::with_segment_bytes(2 * per_entry, 2 * per_entry);
        let space = ns("a");
        let a = put(&index, space, b"a", None, &[(0, 1)], &[]).unwrap();
        let b = put(&index, space, b"b", None, &[(1, 1)], &[]).unwrap();
        index.release(a);
        index.release(b);
        let c = put(&index, space, b"c", None, &[(2, 1)], &[]).unwrap();
        let d = put(&index, space, b"d", None, &[(3, 1)], &[]).unwrap();
        let stats = index.stats();
        assert_eq!((stats.evictions, stats.refusals), (2, 0));
        assert!(stats.charged <= stats.budget);
        index.release(c);
        index.release(d);
    }

    #[test]
    fn worker_stores_share_the_budget_and_a_growing_or_largest_store_shrinks() {
        let per_entry = PAGE;
        let bytes = per_entry as u64;
        let index = LocalIndex::with_segment_bytes(4 * per_entry, per_entry);
        let space = ns("a");
        let a = put(&index, space, b"a", None, &[(0, 1)], &[]).unwrap();
        let b = put(&index, space, b"b", None, &[(1, 1)], &[]).unwrap();
        index.release(a);
        assert!(!index.report_store(1, bytes));
        assert!(!index.report_store(2, 2 * bytes));
        assert_eq!(index.stats().evictions, 1);
        assert!(put(&index, space, b"c", None, &[(2, 1)], &[]).is_err());
        assert!(index.report_store(1, bytes + 1));
        assert!(index.report_store(2, 2 * bytes));
        assert_eq!(index.stats().shrinks, 2);
        index.release(b);
        assert!(!index.report_store(2, 2 * bytes));
        assert_eq!(index.stats().charged, 0);
        index.forget_store(1);
        index.forget_store(2);
        assert_eq!(index.stats().stored, 0);
    }

    #[test]
    fn a_store_over_budget_shrinks_until_it_is_back_to_its_last_fitting_size() {
        let page = PAGE as u64;
        let index = LocalIndex::with_segment_bytes(8 * PAGE, PAGE);
        assert!(!index.report_store(1, 6 * page));
        assert!(!index.report_store(2, page));
        assert!(index.report_store(2, 4 * page));
        assert!(index.report_store(2, 3 * page));
        assert!(!index.report_store(2, 2 * page));
        assert!(index.report_store(1, 6 * page + 1));
        assert!(index.report_store(3, page));
        index.forget_store(1);
        assert!(!index.report_store(2, 2 * page));
        assert_eq!(index.stats().shrinks, 4);
    }

    #[test]
    fn measured_headroom_lowers_the_budget_until_memory_returns() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static HEADROOM: AtomicU64 = AtomicU64::new(0);
        fn headroom() -> Option<u64> {
            Some(HEADROOM.load(Ordering::Relaxed))
        }
        let page = PAGE as u64;
        let index = LocalIndex::with_segment_bytes(4 * PAGE, PAGE).with_headroom(headroom);
        let space = ns("a");
        HEADROOM.store(RESERVE_BYTES + 8 * page, Ordering::Relaxed);
        let a = put(&index, space, b"a", None, &[(0, 1)], &[]).unwrap();
        let b = put(&index, space, b"b", None, &[(1, 1)], &[]).unwrap();
        index.release(a);
        index.release(b);
        let charged = index.stats().charged as u64;
        assert!(charged > page && charged <= 4 * page);
        HEADROOM.store(RESERVE_BYTES - page, Ordering::Relaxed);
        assert!(!index.report_store(1, 0));
        let stats = index.stats();
        assert_eq!(stats.lowest_budget as u64, charged - page);
        assert!(stats.charged as u64 <= charged - page);
        assert!(stats.evictions > 0);
        HEADROOM.store(RESERVE_BYTES + 64 * page, Ordering::Relaxed);
        let c = put(&index, space, b"c", None, &[(2, 1)], &[]).unwrap();
        let d = put(&index, space, b"d", None, &[(3, 1)], &[]).unwrap();
        index.release(c);
        index.release(d);
        let stats = index.stats();
        assert_eq!(stats.budget, 4 * PAGE);
        assert_eq!(stats.lowest_budget as u64, charged - page);
        assert!(stats.charged as u64 > charged - page);
        HEADROOM.store(0, Ordering::Relaxed);
        assert!(index.report_store(1, page));
        assert_eq!(index.stats().charged, 0);
        index.forget_store(1);
    }

    #[test]
    fn workers_on_threads_share_entries() {
        let index = LocalIndex::new(16 << 20);
        let space = ns("a");
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let index = &index;
                scope.spawn(move || {
                    for step in 0..8u8 {
                        let key = [step];
                        let lease = put(index, space, &key, None, &[(0, step)], &[]).unwrap();
                        assert_eq!(resolved(index, &lease).0, vec![(0, step)]);
                        index.release(lease);
                    }
                });
            }
        });
        let stats = index.stats();
        assert_eq!(stats.entries, 8);
        assert_eq!(stats.publishes + stats.duplicates, 32);
        assert_eq!(stats.leased, 0);
    }
}
