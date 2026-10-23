// SPDX-License-Identifier: AGPL-3.0-or-later

use std::sync::Arc;

use consonance_client::cache::{CacheIndex, Lease, Namespace};
use control_proto::SnapId;

use crate::history::HistoryReceipt;
use crate::target::ObservationCheckpoint;

pub const CHAIN_LIMIT: usize = 32;

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
struct RetainedObservationCheckpoint {
    checkpoint: ObservationCheckpoint,
    allocation: (usize, usize),
    verified: bool,
}

#[derive(Debug)]
struct Link {
    len: usize,
    point: Point,
    lease: Option<Lease>,
    provenance: Option<HistoryReceipt>,
    observations: Option<Box<RetainedObservationCheckpoint>>,
}

#[derive(Debug)]
pub struct Chain {
    links: Vec<Link>,
    shared: Option<Shared>,
}

impl Chain {
    #[must_use]
    pub fn new(setup: Point, shared: Option<Shared>) -> Self {
        Self {
            links: vec![Link {
                len: 0,
                point: setup,
                lease: None,
                provenance: None,
                observations: None,
            }],
            shared,
        }
    }

    pub fn by_path(&self, path: [u8; 32]) -> Option<usize> {
        self.links.iter().position(|link| {
            link.provenance
                .as_ref()
                .is_some_and(|receipt| receipt.path() == path)
        })
    }

    pub fn push_history(
        &mut self,
        point: Point,
        lease: Option<Lease>,
        receipt: HistoryReceipt,
    ) -> Result<Vec<SnapId>, String> {
        receipt.validate()?;
        self.links.push(Link {
            len: receipt.depth(),
            point,
            lease,
            provenance: Some(receipt),
            observations: None,
        });
        let mut dropped = Vec::new();
        if self.links.len() > CHAIN_LIMIT + 1 {
            let link = self.links.remove(1);
            dropped.extend(self.retire(link));
        }
        Ok(dropped)
    }

    pub fn certified(&self, receipt: &HistoryReceipt) -> Option<usize> {
        self.links
            .iter()
            .position(|link| link.provenance.as_ref() == Some(receipt))
    }

    pub fn at_snapshot(&self, snap: SnapId) -> Option<usize> {
        self.links.iter().position(|link| link.point.snap == snap)
    }

    pub fn provenance(&self, at: usize) -> Result<&HistoryReceipt, String> {
        self.links[at]
            .provenance
            .as_ref()
            .ok_or_else(|| "fault snapshot lacks captured history provenance".to_owned())
    }

    pub fn certify(&mut self, at: usize, provenance: HistoryReceipt) -> Result<(), String> {
        provenance.validate()?;
        if provenance.depth() != self.links[at].len {
            return Err("captured history depth differs from snapshot".to_owned());
        }
        if self.links[at]
            .provenance
            .as_ref()
            .is_some_and(|existing| existing != &provenance)
        {
            return Err("captured fault history cannot be replaced".to_owned());
        }
        self.links[at].provenance = Some(provenance);
        Ok(())
    }

    pub(crate) fn observation_checkpoint(&self, at: usize) -> Option<&ObservationCheckpoint> {
        self.links[at]
            .observations
            .as_deref()
            .filter(|retained| retained.verified)
            .map(|retained| &retained.checkpoint)
    }

    pub(crate) fn pending_observation_checkpoint(
        &self,
        at: usize,
    ) -> Option<&ObservationCheckpoint> {
        self.links[at]
            .observations
            .as_deref()
            .filter(|retained| !retained.verified)
            .map(|retained| &retained.checkpoint)
    }

    pub(crate) fn set_pending_observation_checkpoint(
        &mut self,
        at: usize,
        checkpoint: ObservationCheckpoint,
    ) -> Result<(), String> {
        self.store_observation_checkpoint(at, checkpoint, false)
    }

    pub(crate) fn set_observation_checkpoint(
        &mut self,
        at: usize,
        checkpoint: ObservationCheckpoint,
    ) -> Result<(), String> {
        self.store_observation_checkpoint(at, checkpoint, true)
    }

    fn store_observation_checkpoint(
        &mut self,
        at: usize,
        checkpoint: ObservationCheckpoint,
        verified: bool,
    ) -> Result<(), String> {
        checkpoint.validate_binding(self.provenance(at)?)?;
        let allocation = checkpoint.decoder().retained_allocation();
        self.links[at].observations = Some(Box::new(RetainedObservationCheckpoint {
            checkpoint,
            allocation,
            verified,
        }));
        Ok(())
    }

    pub(crate) fn observation_checkpoint_bytes(&self) -> usize {
        let mut seen = std::collections::BTreeSet::new();
        self.links
            .iter()
            .filter_map(|link| link.observations.as_deref())
            .map(|checkpoint| {
                let (identity, bytes) = checkpoint.allocation;
                std::mem::size_of::<RetainedObservationCheckpoint>()
                    + if seen.insert(identity) { bytes } else { 0 }
            })
            .sum::<usize>()
            + self.links.capacity() * std::mem::size_of::<Option<Box<ObservationCheckpoint>>>()
    }

    pub(crate) fn discard_observation_checkpoints(&mut self, limit: usize) -> usize {
        let mut discarded = 0;
        for at in 0..self.links.len() {
            if self.observation_checkpoint_bytes() <= limit {
                break;
            }
            if self.links[at].observations.take().is_some() {
                discarded += 1;
            }
        }
        discarded
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
    pub fn depth(&self, at: usize) -> usize {
        self.links[at].len
    }

    #[must_use]
    pub fn point(&self, at: usize) -> Point {
        self.links[at].point
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
        dropped
    }

    pub(crate) fn remove_snapshot(&mut self, snapshot: SnapId) -> Option<SnapId> {
        let at = self.at_snapshot(snapshot)?;
        if at == 0 {
            return None;
        }
        let link = self.links.remove(at);
        self.retire(link)
    }

    pub fn evict_oldest(&mut self) -> Option<SnapId> {
        if self.links.len() <= 2 {
            return None;
        }
        let link = self.links.remove(1);
        self.retire(link)
    }

    pub fn shed(&mut self, keep: usize) -> Vec<SnapId> {
        match self.evict_oldest() {
            Some(snap) => vec![snap],
            None => self.truncate(keep),
        }
    }

    #[must_use]
    pub fn parent_for(&self, parent: Point) -> Option<(Point, &Lease)> {
        let at = self.links.iter().position(|link| link.point == parent)?;
        self.links[..=at]
            .iter()
            .rev()
            .find_map(|link| link.lease.as_ref().map(|lease| (link.point, lease)))
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
    use super::*;
    use crate::target::{FaultEndpointCertificate, FaultObservations, FaultOperation};
    use consonance_client::cache::{LocalIndex, extent::encode};

    fn point(snap: u64) -> Point {
        Point {
            snap: SnapId(snap),
            moment: snap * 10,
        }
    }
    fn root() -> HistoryReceipt {
        HistoryReceipt::genesis(
            [3; 32],
            FaultEndpointCertificate::KvmWholeState([17; 32]),
            crate::history::fixture_event_history(FaultEndpointCertificate::KvmWholeState(
                [17; 32],
            )),
            &FaultObservations::default(),
        )
        .unwrap()
    }
    fn child(parent: &HistoryReceipt, ticks: u16) -> HistoryReceipt {
        let observation = FaultObservations {
            moment: parent.moment() + 10,
            ..FaultObservations::default()
        };
        parent
            .extend(
                FaultOperation::Wait(std::num::NonZeroU16::new(ticks).unwrap()).into(),
                parent.endpoint(),
                crate::history::fixture_event_history(parent.endpoint()),
                &observation,
            )
            .unwrap()
    }
    fn chain(shared: Option<Shared>) -> Chain {
        let mut chain = Chain::new(point(0), shared);
        chain.certify(0, root()).unwrap();
        chain
    }

    #[test]
    fn exact_paths_preserve_independently_captured_history_identity() {
        let mut chain = chain(None);
        let first = child(&root(), 1);
        let branch = child(&root(), 2);
        chain.push_history(point(1), None, first.clone()).unwrap();
        chain.push_history(point(2), None, branch.clone()).unwrap();
        assert_eq!(chain.by_path(first.path()), Some(1));
        assert_eq!(chain.by_path(branch.path()), Some(2));
        assert_eq!(chain.certified(&first), Some(1));
        let observation = FaultObservations {
            moment: 10,
            alive: 7,
            ..FaultObservations::default()
        };
        let divergent = root()
            .extend(
                first.action().unwrap(),
                first.endpoint(),
                crate::history::fixture_event_history(first.endpoint()),
                &observation,
            )
            .unwrap();
        assert_eq!(divergent.path(), first.path());
        assert_eq!(chain.certified(&divergent), None);
        assert_eq!(chain.provenance(1).unwrap(), &first);
    }

    #[test]
    fn eviction_preserves_setup_and_the_latest_certified_endpoint() {
        let mut chain = chain(None);
        let mut receipt = root();
        let mut dropped = Vec::new();
        for depth in 1..=40 {
            receipt = child(&receipt, 1);
            dropped.extend(
                chain
                    .push_history(point(depth), None, receipt.clone())
                    .unwrap(),
            );
        }
        assert_eq!(chain.links(), CHAIN_LIMIT + 1);
        assert_eq!(dropped, (1..=8).map(SnapId).collect::<Vec<_>>());
        assert_eq!(chain.provenance(0).unwrap(), &root());
        assert_eq!(chain.certified(&receipt), Some(CHAIN_LIMIT));
        while chain.evict_oldest().is_some() {}
        assert_eq!(chain.links(), 2);
        assert_eq!(chain.certified(&receipt), Some(1));
        assert!(chain.shed(2).is_empty());
        assert_eq!(chain.truncate(1), vec![SnapId(40)]);
        assert_eq!(chain.links(), 1);
    }

    #[test]
    fn parent_lease_uses_the_selected_point_instead_of_a_later_sibling() {
        let index = Arc::new(LocalIndex::new(1 << 20));
        let namespace = Namespace::exact(&[b"sibling-parent"]);
        let mut chain = chain(Some(Shared {
            index: index.clone(),
            namespace,
        }));
        let mut leases = Vec::new();
        for (snap, ticks) in [(1, 1), (2, 2)] {
            let receipt = child(&root(), ticks);
            let encoded = encode(&[], &[], &[]).unwrap();
            let mut extent = index.extent(encoded.len()).unwrap();
            encoded.write(extent.bytes_mut()).unwrap();
            let lease = index
                .publish(
                    namespace,
                    &receipt.path(),
                    None,
                    extent,
                    [ticks as u8; 32],
                    1,
                )
                .unwrap();
            leases.push(index.lookup(namespace, &receipt.path()).unwrap());
            chain
                .push_history(point(snap), Some(lease), receipt)
                .unwrap();
        }
        let (snapshot, parent) = chain.parent_for(point(1)).unwrap();
        assert_eq!(snapshot.snap, SnapId(1));
        assert_eq!(parent, &leases[0]);
        assert_ne!(parent, &leases[1]);
        for lease in leases {
            index.release(lease);
        }
        assert!(
            chain
                .parent_for(Point {
                    snap: SnapId(1),
                    moment: 999
                })
                .is_none()
        );
    }

    #[test]
    fn exact_cache_leases_follow_certified_depth_and_release_on_drop() {
        let index = Arc::new(LocalIndex::new(1 << 20));
        let namespace = Namespace::exact(&[b"certified-chain"]);
        let shared = Shared {
            index: index.clone(),
            namespace,
        };
        let mut chain = chain(Some(shared));
        let mut receipt = root();
        for depth in 1..=40 {
            receipt = child(&receipt, 1);
            let encoded = encode(&[], &[], &[]).unwrap();
            let mut extent = index.extent(encoded.len()).unwrap();
            encoded.write(extent.bytes_mut()).unwrap();
            let lease = index
                .publish(
                    namespace,
                    &receipt.path(),
                    chain
                        .parent_for(point(depth as u64 - 1))
                        .map(|(_, lease)| lease),
                    extent,
                    [17; 32],
                    1,
                )
                .unwrap();
            chain
                .push_history(point(depth as u64), Some(lease), receipt.clone())
                .unwrap();
        }
        assert_eq!(index.stats().leased, CHAIN_LIMIT);
        assert!(chain.parent_for(point(40)).is_some());
        assert!(chain.parent_for(point(41)).is_none());
        drop(chain);
        assert_eq!(index.stats().leased, 0);
        let lease = index.lookup(namespace, &receipt.path()).unwrap();
        index.release(lease);
    }
}
