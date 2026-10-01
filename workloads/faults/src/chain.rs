// SPDX-License-Identifier: AGPL-3.0-or-later

use std::sync::Arc;

use consonance_client::cache::{CacheIndex, Lease, Namespace};
use control_proto::SnapId;

use crate::target::{ACTION_KEY_WIDTH, FaultAction, actions_key};

pub const CHAIN_LIMIT: usize = 32;
const KEY_WIDTH: usize = ACTION_KEY_WIDTH;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Point {
    pub snap: SnapId,
    pub moment: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Hit {
    Exact,
    Ancestor,
    Miss,
}

impl Hit {
    #[must_use]
    pub fn of(start: usize, len: usize) -> Self {
        if start == len {
            Self::Exact
        } else if start == 0 {
            Self::Miss
        } else {
            Self::Ancestor
        }
    }
}

#[derive(Debug)]
pub struct Shared {
    pub index: Arc<dyn CacheIndex>,
    pub namespace: Namespace,
}

#[derive(Debug)]
struct Link {
    len: usize,
    point: Point,
    lease: Option<Lease>,
}

#[derive(Debug)]
pub struct Chain {
    key: Vec<FaultAction>,
    links: Vec<Link>,
    shared: Option<Shared>,
}

impl Chain {
    #[must_use]
    pub fn new(setup: Point, shared: Option<Shared>) -> Self {
        Self {
            key: Vec::new(),
            links: vec![Link {
                len: 0,
                point: setup,
                lease: None,
            }],
            shared,
        }
    }

    #[must_use]
    pub fn shared(&self) -> Option<&Shared> {
        self.shared.as_ref()
    }

    #[must_use]
    pub fn links(&self) -> usize {
        self.links.len()
    }

    pub fn points(&self) -> impl Iterator<Item = Point> + '_ {
        self.links.iter().map(|link| link.point)
    }

    #[must_use]
    pub fn local(&self, actions: &[FaultAction]) -> usize {
        let common = actions
            .iter()
            .zip(&self.key)
            .take_while(|(left, right)| left == right)
            .count();
        self.links
            .iter()
            .rposition(|link| link.len <= common)
            .unwrap_or(0)
    }

    #[must_use]
    pub fn depth(&self, at: usize) -> usize {
        self.links[at].len
    }

    #[must_use]
    pub fn point(&self, at: usize) -> Point {
        self.links[at].point
    }

    #[must_use]
    pub fn deeper_shared(&self, actions: &[FaultAction], local: usize) -> Option<Lease> {
        let shared = self.shared.as_ref()?;
        let lease = shared
            .index
            .lookup(shared.namespace, &actions_key(actions))?;
        if lease.key_len() % KEY_WIDTH == 0 && lease.key_len() / KEY_WIDTH > local {
            Some(lease)
        } else {
            shared.index.release(lease);
            None
        }
    }

    #[must_use]
    pub fn actions_in(lease: &Lease) -> usize {
        lease.key_len() / KEY_WIDTH
    }

    pub fn release(&self, lease: Lease) {
        if let Some(shared) = &self.shared {
            shared.index.release(lease);
        }
    }

    fn retire(&self, mut link: Link) -> Option<SnapId> {
        if let Some(lease) = link.lease.take() {
            self.release(lease);
        }
        (link.len != 0).then_some(link.point.snap)
    }

    pub fn truncate(&mut self, links: usize) -> Vec<SnapId> {
        let mut dropped = Vec::new();
        while self.links.len() > links.max(1) {
            if let Some(link) = self.links.pop()
                && let Some(snap) = self.retire(link)
            {
                dropped.push(snap);
            }
        }
        let len = self.links.last().map_or(0, |link| link.len);
        self.key.truncate(len);
        dropped
    }

    pub fn evict_oldest(&mut self) -> Option<SnapId> {
        if self.links.len() <= 2 {
            return None;
        }
        let link = self.links.remove(1);
        self.retire(link)
    }

    pub fn push(
        &mut self,
        actions: &[FaultAction],
        point: Point,
        lease: Option<Lease>,
    ) -> Option<SnapId> {
        self.key.clear();
        self.key.extend_from_slice(actions);
        self.links.push(Link {
            len: actions.len(),
            point,
            lease,
        });
        if self.links.len() > CHAIN_LIMIT.saturating_add(1) {
            let link = self.links.remove(1);
            return self.retire(link);
        }
        None
    }

    #[must_use]
    pub fn parent_for(&self, len: usize) -> Option<(SnapId, &Lease)> {
        let link = self.links.last()?;
        (link.len.saturating_add(1) == len)
            .then_some(link)
            .and_then(|link| link.lease.as_ref().map(|lease| (link.point.snap, lease)))
    }
}

impl Drop for Chain {
    fn drop(&mut self) {
        if let Some(shared) = &self.shared {
            for lease in self.links.iter_mut().filter_map(|link| link.lease.take()) {
                shared.index.release(lease);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU16;

    use consonance_client::cache::{
        LocalIndex,
        extent::{extent_len, write_extent},
    };
    use fault_policy::ParkTarget;

    use super::*;

    fn wait(ticks: u16) -> FaultAction {
        FaultAction::Wait(NonZeroU16::new(ticks).unwrap())
    }

    fn point(snap: u64) -> Point {
        Point {
            snap: SnapId(snap),
            moment: snap * 10,
        }
    }

    fn shared(index: &Arc<LocalIndex>) -> Shared {
        Shared {
            index: Arc::clone(index) as Arc<dyn CacheIndex>,
            namespace: Namespace::new(&[b"test"]),
        }
    }

    fn publish(index: &LocalIndex, actions: &[FaultAction], parent: Option<&Lease>) -> Lease {
        let mut extent = index.extent(extent_len(0, 0, 0).unwrap()).unwrap();
        write_extent(extent.bytes_mut(), &[], &[], &[]).unwrap();
        index
            .publish(
                Namespace::new(&[b"test"]),
                &actions_key(actions),
                parent,
                extent,
                1,
            )
            .unwrap()
    }

    #[test]
    fn action_keys_are_fixed_width_and_distinct() {
        let tick = NonZeroU16::new(9).unwrap();
        let park = FaultAction::EventPark {
            node: 3,
            edges: 1,
            hold_us: 9,
            ticks: tick,
            target: None,
        };
        let actions = [
            wait(3),
            FaultAction::Kill(3, tick),
            FaultAction::EventKill {
                node: 3,
                rarity: 1,
                ticks: tick,
            },
            park,
            FaultAction::EventPark {
                node: 3,
                edges: 1,
                hold_us: 9,
                ticks: tick,
                target: ParkTarget::new(u64::MAX - 1, u64::MAX),
            },
            FaultAction::Pause(3, tick),
            FaultAction::Restart(3, tick),
            FaultAction::Hook(3, tick),
        ];
        let keys: std::collections::BTreeSet<_> =
            actions.iter().map(FaultAction::key_bytes).collect();
        assert_eq!(keys.len(), actions.len());
        assert_eq!(actions_key(&actions).len(), actions.len() * KEY_WIDTH);
        assert!(actions_key(&actions).starts_with(&actions_key(&actions[..3])));
    }

    #[test]
    fn local_matches_classify_as_exact_ancestor_or_miss() {
        let mut chain = Chain::new(point(0), None);
        let path = [wait(1), wait(2), wait(3)];
        for len in 1..=3 {
            assert_eq!(chain.push(&path[..len], point(len as u64), None), None);
        }
        let at = chain.local(&path[..2]);
        assert_eq!(Hit::of(chain.depth(at), 2), Hit::Exact);
        assert_eq!(chain.point(at), point(2));
        let branch = [wait(1), wait(2), wait(9), wait(4)];
        let at = chain.local(&branch);
        assert_eq!(
            (Hit::of(chain.depth(at), 4), chain.depth(at)),
            (Hit::Ancestor, 2)
        );
        let at = chain.local(&[wait(7)]);
        assert_eq!(Hit::of(chain.depth(at), 1), Hit::Miss);
        assert_eq!(chain.point(at), point(0));
        assert_eq!(chain.truncate(3), vec![SnapId(3)]);
        assert_eq!(chain.local(&path), 2, "the key follows the last link");
        assert_eq!(chain.truncate(0), vec![SnapId(2), SnapId(1)]);
        assert_eq!(chain.links(), 1, "setup stays");
    }

    #[test]
    fn eviction_drops_the_oldest_link_and_keeps_setup_and_the_newest() {
        let mut chain = Chain::new(point(0), None);
        let path = [wait(1), wait(2), wait(3)];
        for len in 1..=3 {
            chain.push(&path[..len], point(len as u64), None);
        }
        assert_eq!(chain.evict_oldest(), Some(SnapId(1)));
        assert_eq!(chain.evict_oldest(), Some(SnapId(2)));
        assert_eq!(chain.evict_oldest(), None);
        assert_eq!(chain.links(), 2);
        let at = chain.local(&path);
        assert_eq!((chain.depth(at), chain.point(at)), (3, point(3)));
        assert_eq!(
            chain.local(&path[..2]),
            0,
            "an evicted prefix falls back to setup"
        );
    }

    #[test]
    fn a_deeper_shared_entry_is_leased_and_a_shallower_one_released() {
        let index = Arc::new(LocalIndex::new(1 << 20));
        let path = [wait(1), wait(2), wait(3)];
        let root = publish(&index, &path[..1], None);
        let deep = publish(&index, &path[..3], Some(&root));
        index.release(root);
        index.release(deep);
        let mut chain = Chain::new(point(0), Some(shared(&index)));
        let lease = chain.deeper_shared(&path, 0).unwrap();
        assert_eq!(Chain::actions_in(&lease), 3);
        chain.push(&path, point(3), Some(lease));
        assert!(chain.deeper_shared(&path, 3).is_none());
        assert!(chain.deeper_shared(&[wait(9)], 0).is_none());
        assert_eq!(index.stats().leased, 1);
        assert!(chain.parent_for(4).is_some());
        assert!(chain.parent_for(3).is_none());
    }

    #[test]
    fn the_chain_keeps_its_limit_and_releases_what_it_drops() {
        let index = Arc::new(LocalIndex::new(1 << 20));
        let mut chain = Chain::new(point(0), Some(shared(&index)));
        let actions: Vec<_> = (1..=40).map(wait).collect();
        let mut dropped = Vec::new();
        for len in 1..=actions.len() {
            let parent = chain.parent_for(len).map(|(_, lease)| lease);
            let lease = publish(&index, &actions[..len], parent);
            dropped.extend(chain.push(&actions[..len], point(len as u64), Some(lease)));
        }
        assert_eq!(chain.links(), CHAIN_LIMIT + 1);
        assert_eq!(dropped, (1..=8).map(SnapId).collect::<Vec<_>>());
        assert_eq!(index.stats().leased, CHAIN_LIMIT);
        assert_eq!(chain.point(1), point(9));
    }

    #[test]
    fn a_dropped_session_releases_its_leases_and_a_new_one_finds_the_entries() {
        let index = Arc::new(LocalIndex::new(1 << 20));
        let path = [wait(1), wait(2)];
        {
            let mut chain = Chain::new(point(0), Some(shared(&index)));
            let first = publish(&index, &path[..1], None);
            chain.push(&path[..1], point(1), Some(first));
            let parent = chain.parent_for(2).map(|(_, lease)| lease);
            let second = publish(&index, &path, parent);
            chain.push(&path, point(2), Some(second));
            assert_eq!(index.stats().leased, 2);
        }
        assert_eq!(index.stats().leased, 0);
        let chain = Chain::new(point(100), Some(shared(&index)));
        let at = chain.local(&path);
        assert_eq!(Hit::of(chain.depth(at), path.len()), Hit::Miss);
        let lease = chain.deeper_shared(&path, chain.depth(at)).unwrap();
        assert_eq!(Chain::actions_in(&lease), 2);
        chain.release(lease);
    }
}
