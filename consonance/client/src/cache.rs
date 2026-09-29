// SPDX-License-Identifier: AGPL-3.0-or-later

mod budget;
pub mod extent;
pub mod segments;

use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Bound,
    sync::{Arc, Mutex, MutexGuard, Weak},
};

use sha2::{Digest, Sha256};

pub use budget::{MemoryPlan, RESERVE_BYTES, headroom_bytes, plan_memory};
pub use segments::{CommittedExtent, PageSegments, SEGMENT_BYTES, WritableExtent};

pub const ANCHOR_DEPTH: u32 = 32;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Namespace([u8; 32]);

impl Namespace {
    #[must_use]
    pub fn new(parts: &[&[u8]]) -> Self {
        let mut digest = Sha256::new();
        digest.update(b"consonance-snapshot-cache-namespace-v1");
        for part in parts {
            digest.update((part.len() as u64).to_le_bytes());
            digest.update(part);
        }
        Self(digest.finalize().into())
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct Lease {
    entry: u64,
    key_len: usize,
    depth: u32,
}

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
    #[error("malformed snapshot cache extent: {0}")]
    Malformed(&'static str),
    #[error("snapshot cache segment: {0}")]
    Segment(#[from] std::io::Error),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CacheStats {
    pub budget: usize,
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
    fn lookup(&self, namespace: Namespace, key: &[u8]) -> Option<Lease>;
    fn extent(&self, len: usize) -> Result<WritableExtent, CacheError>;
    fn publish(
        &self,
        namespace: Namespace,
        key: &[u8],
        parent: Option<&Lease>,
        extent: WritableExtent,
        cost: u64,
    ) -> Result<Lease, CacheError>;
    fn chain(&self, lease: &Lease) -> Result<Vec<CommittedExtent>, CacheError>;
    fn release(&self, lease: Lease);
    fn report_store(&self, holder: u64, bytes: u64) -> bool;
    fn forget_store(&self, holder: u64);
    fn stats(&self) -> CacheStats;
}

#[derive(Debug)]
struct Entry {
    namespace: Namespace,
    key: Vec<u8>,
    parent: Option<u64>,
    depth: u32,
    extent: CommittedExtent,
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
    budget: usize,
    segments: PageSegments,
    entries: BTreeMap<u64, Entry>,
    keys: BTreeMap<Namespace, BTreeMap<Vec<u8>, u64>>,
    evictable: BTreeSet<(u128, u64)>,
    stores: BTreeMap<u64, usize>,
    stored: usize,
    next_id: u64,
    floor: u128,
    stats: CacheStats,
}

impl State {
    fn longest_prefix(&self, namespace: Namespace, key: &[u8]) -> Option<u64> {
        let keys = self.keys.get(&namespace)?;
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
            entry: id,
            key_len: entry.key.len(),
            depth: entry.depth,
        })
    }

    fn settle(&mut self, id: u64) {
        if let Some(entry) = self.entries.get(&id)
            && entry.leases == 0
            && entry.children == 0
        {
            self.evictable.insert((entry.priority, id));
        }
    }

    fn evict(&mut self, id: u64) {
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
        self.free(&entry.extent);
        self.stats.evictions += 1;
    }

    fn free(&mut self, extent: &CommittedExtent) {
        let (segment_id, offset, len) = extent.span();
        self.segments.free(segment_id, offset, len);
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
        while self.segments.charged() + needed > room {
            let Some(&(_, id)) = self.evictable.first() else {
                return Err(refused);
            };
            self.evict(id);
        }
        Ok(())
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
    }
}

#[derive(Clone, Debug)]
pub struct LocalIndex {
    state: Arc<Mutex<State>>,
}

impl LocalIndex {
    #[must_use]
    pub fn new(budget: usize) -> Self {
        Self::with_segment_bytes(budget, SEGMENT_BYTES)
    }

    #[must_use]
    pub fn with_segment_bytes(budget: usize, segment_bytes: usize) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                budget,
                segments: PageSegments::new(segment_bytes),
                entries: BTreeMap::new(),
                keys: BTreeMap::new(),
                evictable: BTreeSet::new(),
                stores: BTreeMap::new(),
                stored: 0,
                next_id: 0,
                floor: 0,
                stats: CacheStats::default(),
            })),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn abandon(state: Weak<Mutex<State>>) -> segments::Abandon {
        Box::new(move |segment_id, offset, len| {
            if let Some(state) = state.upgrade() {
                state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .segments
                    .free(segment_id, offset, len);
            }
        })
    }
}

impl CacheIndex for LocalIndex {
    fn lookup(&self, namespace: Namespace, key: &[u8]) -> Option<Lease> {
        let mut state = self.state();
        state.stats.lookups += 1;
        let id = state.longest_prefix(namespace, key)?;
        state.stats.hits += 1;
        state.lease(id).ok()
    }

    fn extent(&self, len: usize) -> Result<WritableExtent, CacheError> {
        let mut state = self.state();
        let needed =
            segments::rounded(len.max(1)).ok_or(CacheError::Malformed("extent size overflow"))?;
        if let Err(error) = state.make_room(needed) {
            state.stats.refusals += 1;
            return Err(error);
        }
        let mut extent = state.segments.allocate(needed)?;
        extent.set_abandon(Self::abandon(Arc::downgrade(&self.state)));
        Ok(extent)
    }

    fn publish(
        &self,
        namespace: Namespace,
        key: &[u8],
        parent: Option<&Lease>,
        extent: WritableExtent,
        cost: u64,
    ) -> Result<Lease, CacheError> {
        let mut state = self.state();
        let committed = extent.commit();
        if let Some(&id) = state.keys.get(&namespace).and_then(|keys| keys.get(key)) {
            state.free(&committed);
            state.stats.duplicates += 1;
            return state.lease(id);
        }
        let depth = match parent {
            Some(lease) => {
                let Some(parent) = state.entries.get_mut(&lease.entry) else {
                    state.free(&committed);
                    return Err(CacheError::UnknownLease(lease.entry));
                };
                if parent.namespace != namespace {
                    state.free(&committed);
                    return Err(CacheError::NamespaceMismatch);
                }
                parent.children += 1;
                parent.depth + 1
            }
            None => 0,
        };
        let id = state.next_id;
        state.next_id += 1;
        let mut entry = Entry {
            namespace,
            key: key.to_vec(),
            parent: parent.map(|lease| lease.entry),
            depth,
            extent: committed,
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
            entry: id,
            key_len: key.len(),
            depth,
        })
    }

    fn chain(&self, lease: &Lease) -> Result<Vec<CommittedExtent>, CacheError> {
        let state = self.state();
        let mut chain = Vec::new();
        let mut cursor = Some(lease.entry);
        while let Some(id) = cursor {
            let entry = state.entries.get(&id).ok_or(CacheError::UnknownLease(id))?;
            chain.push(entry.extent.clone());
            cursor = entry.parent;
        }
        chain.reverse();
        Ok(chain)
    }

    fn release(&self, lease: Lease) {
        let mut state = self.state();
        if let Some(entry) = state.entries.get_mut(&lease.entry) {
            entry.leases -= 1;
            state.settle(lease.entry);
        }
    }

    fn report_store(&self, holder: u64, bytes: u64) -> bool {
        let mut state = self.state();
        state.set_store(holder, Some(usize::try_from(bytes).unwrap_or(usize::MAX)));
        while state.over_budget() {
            let Some(&(_, id)) = state.evictable.first() else {
                break;
            };
            state.evict(id);
        }
        let largest = state
            .stores
            .iter()
            .max_by_key(|&(&id, &bytes)| (bytes, std::cmp::Reverse(id)))
            .map(|(&id, _)| id);
        let shrink = state.over_budget() && largest == Some(holder);
        if shrink {
            state.stats.shrinks += 1;
        }
        shrink
    }

    fn forget_store(&self, holder: u64) {
        self.state().set_store(holder, None);
    }

    fn stats(&self) -> CacheStats {
        let state = self.state();
        CacheStats {
            budget: state.budget,
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
}

#[cfg(test)]
mod tests {
    use super::extent::{HashedPage, extent_len, read_extent, resolve, write_extent};
    use super::segments::PAGE;
    use super::*;

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
        let len = extent_len(rows.len(), reverted.len(), key.len()).unwrap();
        let mut extent = index.extent(len)?;
        write_extent(extent.bytes_mut(), &rows, reverted, key)?;
        index.publish(namespace, key, parent, extent, 1)
    }

    fn put_costing(index: &LocalIndex, key: &[u8], cost: u64) {
        let mut extent = index.extent(extent_len(0, 0, key.len()).unwrap()).unwrap();
        write_extent(extent.bytes_mut(), &[], &[], key).unwrap();
        let lease = index.publish(ns("a"), key, None, extent, cost).unwrap();
        index.release(lease);
    }

    fn held(index: &LocalIndex, key: &[u8]) -> bool {
        index
            .state()
            .keys
            .get(&ns("a"))
            .is_some_and(|keys| keys.contains_key(key))
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
                .pages
                .iter()
                .map(|&(gfn, _, data)| (gfn, data[0]))
                .collect(),
            resolved.sidecar.to_vec(),
        )
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
    fn a_duplicate_key_keeps_the_first_entry_and_frees_the_second_extent() {
        let index = LocalIndex::with_segment_bytes(1 << 20, 4 * PAGE);
        let space = ns("a");
        let first = put(&index, space, b"k", None, &[(0, 1)], &[]).unwrap();
        let charged = index.stats().charged;
        let second = put(&index, space, b"k", None, &[(0, 2)], &[]).unwrap();
        assert_eq!(first, second);
        assert_eq!(resolved(&index, &second).0, vec![(0, 1)]);
        let stats = index.stats();
        assert_eq!((stats.entries, stats.duplicates), (1, 1));
        assert!(stats.charged >= charged);
    }

    #[test]
    fn eviction_takes_an_unleased_leaf_and_refuses_when_all_are_held() {
        let per_entry = extent_len(1, 0, 1).unwrap();
        let index = LocalIndex::with_segment_bytes(3 * per_entry, per_entry);
        let space = ns("a");
        let a = put(&index, space, b"a", None, &[(0, 1)], &[]).unwrap();
        let b = put(&index, space, b"b", Some(&a), &[(1, 1)], &[]).unwrap();
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
            index.lookup(space, b"b").is_none(),
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
        let per_entry = extent_len(0, 0, 1).unwrap();
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
        let left = put(&index, space, b"L", Some(base), &[(2, 7)], &[1]).unwrap();
        let right = put(&index, space, b"R", Some(base), &[(2, 8)], &[]).unwrap();
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
        let per_entry = extent_len(1, 0, 1).unwrap();
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
    fn worker_stores_share_the_budget_and_the_largest_store_shrinks() {
        let per_entry = extent_len(1, 0, 1).unwrap();
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
        assert!(!index.report_store(1, bytes + 1));
        assert!(index.report_store(2, 2 * bytes));
        assert_eq!(index.stats().shrinks, 1);
        index.release(b);
        assert!(!index.report_store(2, 2 * bytes));
        assert_eq!(index.stats().charged, 0);
        index.forget_store(1);
        index.forget_store(2);
        assert_eq!(index.stats().stored, 0);
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
