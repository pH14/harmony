// SPDX-License-Identifier: AGPL-3.0-or-later

use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

use crate::target::{FaultAction, FaultEndpointCertificate, FaultObservations};

fn digest<T: Serialize>(domain: &str, value: &T) -> Result<[u8; 32], String> {
    postcard::to_allocvec(&(domain, value))
        .map(|bytes| Sha256::digest(bytes).into())
        .map_err(|error| format!("fault history encoding: {error}"))
}

pub fn replay_certificate(
    hash: [u8; 32],
    history: (u64, [u8; 32]),
    observation: &FaultObservations,
    failed: bool,
) -> Result<[u8; 32], String> {
    postcard::to_allocvec(&(
        "fault-replay-endpoint-v2",
        control_proto::EventHistory::VERSION,
        hash,
        history,
        observation,
        failed,
    ))
    .map(|bytes| Sha256::digest(bytes).into())
    .map_err(|error| format!("encode fault replay endpoint: {error}"))
}

pub fn endpoint_certificate(
    endpoint: FaultEndpointCertificate,
    event_history: Option<(u64, [u8; 32])>,
    observation: &FaultObservations,
) -> Result<Option<[u8; 32]>, String> {
    match endpoint {
        FaultEndpointCertificate::KvmWholeState(hash) => replay_certificate(
            hash,
            event_history.ok_or("KVM endpoint lacks ordered event history")?,
            observation,
            false,
        )
        .map(Some),
        FaultEndpointCertificate::UmlWholeStateUnavailable if event_history.is_none() => Ok(None),
        FaultEndpointCertificate::UmlWholeStateUnavailable => {
            Err("UML endpoint unexpectedly carries ordered event history".to_owned())
        }
        FaultEndpointCertificate::KvmUnavailableAfterFailure => {
            Err("failed endpoints cannot certify fault history".to_owned())
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HistoryReceipt {
    domain: [u8; 32],
    depth: usize,
    parent: Option<[u8; 32]>,
    parent_path: Option<[u8; 32]>,
    action: Option<FaultAction>,
    endpoint: FaultEndpointCertificate,
    certificate: Option<[u8; 32]>,
    event_history: Option<(u64, [u8; 32])>,
    observation_commitment: [u8; 32],
    previous_moment: u64,
    moment: u64,
    path: [u8; 32],
    root: [u8; 32],
}

impl HistoryReceipt {
    pub fn genesis(
        domain: [u8; 32],
        endpoint: FaultEndpointCertificate,
        event_history: Option<(u64, [u8; 32])>,
        observation: &FaultObservations,
    ) -> Result<Self, String> {
        let mut out = Self {
            domain,
            depth: 0,
            parent: None,
            parent_path: None,
            action: None,
            endpoint,
            certificate: endpoint_certificate(endpoint, event_history, observation)?,
            event_history,
            observation_commitment: digest("fault-history-observation-v1", observation)?,
            previous_moment: observation.moment,
            moment: observation.moment,
            path: [0; 32],
            root: [0; 32],
        };
        out.path = out.path_digest()?;
        out.root = out.root_digest()?;
        Ok(out)
    }

    pub fn extend(
        &self,
        action: FaultAction,
        endpoint: FaultEndpointCertificate,
        event_history: Option<(u64, [u8; 32])>,
        observation: &FaultObservations,
    ) -> Result<Self, String> {
        if std::mem::discriminant(&self.endpoint) != std::mem::discriminant(&endpoint) {
            return Err("fault history backend changed".to_owned());
        }
        let mut out = Self {
            domain: self.domain,
            depth: self
                .depth
                .checked_add(1)
                .ok_or("fault history depth overflow")?,
            parent: Some(self.root),
            parent_path: Some(self.path),
            action: Some(action),
            endpoint,
            certificate: endpoint_certificate(endpoint, event_history, observation)?,
            event_history,
            observation_commitment: digest("fault-history-observation-v1", observation)?,
            previous_moment: self.moment,
            moment: observation.moment,
            path: [0; 32],
            root: [0; 32],
        };
        out.path = out.path_digest()?;
        out.root = out.root_digest()?;
        out.validate()?;
        Ok(out)
    }

    fn path_digest(&self) -> Result<[u8; 32], String> {
        digest(
            "fault-history-path-v1",
            &(self.domain, self.parent_path, self.action),
        )
    }

    fn root_digest(&self) -> Result<[u8; 32], String> {
        digest(
            "fault-history-node-v1",
            &(
                self.domain,
                self.depth,
                self.parent,
                self.path,
                self.endpoint,
                self.certificate,
                self.event_history,
                self.observation_commitment,
                self.previous_moment,
                self.moment,
            ),
        )
    }

    pub fn validate(&self) -> Result<(), String> {
        let genesis = self.depth == 0;
        if genesis != self.parent.is_none()
            || genesis != self.parent_path.is_none()
            || genesis != self.action.is_none()
            || self.previous_moment > self.moment
            || self.path != self.path_digest()?
            || self.root != self.root_digest()?
            || !matches!(
                (self.endpoint, self.certificate, self.event_history),
                (FaultEndpointCertificate::KvmWholeState(_), Some(_), Some(_))
                    | (
                        FaultEndpointCertificate::UmlWholeStateUnavailable,
                        None,
                        None
                    )
            )
        {
            return Err("malformed fault history receipt".to_owned());
        }
        Ok(())
    }

    pub fn matches_observation(&self, observation: &FaultObservations) -> Result<bool, String> {
        Ok(self.observation_commitment == digest("fault-history-observation-v1", observation)?)
    }

    pub fn follows(&self, parent: &Self) -> bool {
        self.domain == parent.domain
            && parent.depth.checked_add(1) == Some(self.depth)
            && self.parent == Some(parent.root)
            && self.parent_path == Some(parent.path)
            && self.previous_moment == parent.moment
            && std::mem::discriminant(&self.endpoint) == std::mem::discriminant(&parent.endpoint)
    }

    pub fn path(&self) -> [u8; 32] {
        self.path
    }
    pub fn next_path(&self, action: FaultAction) -> Result<[u8; 32], String> {
        digest(
            "fault-history-path-v1",
            &(self.domain, Some(self.path), Some(action)),
        )
    }
    pub fn depth(&self) -> usize {
        self.depth
    }
    pub fn root(&self) -> [u8; 32] {
        self.root
    }
    pub fn domain(&self) -> [u8; 32] {
        self.domain
    }
    pub fn action(&self) -> Option<FaultAction> {
        self.action
    }
    pub fn endpoint(&self) -> FaultEndpointCertificate {
        self.endpoint
    }
    pub fn event_history(&self) -> Option<(u64, [u8; 32])> {
        self.event_history
    }
    pub fn certificate(&self) -> Option<[u8; 32]> {
        self.certificate
    }
    pub fn previous_moment(&self) -> u64 {
        self.previous_moment
    }
    pub fn moment(&self) -> u64 {
        self.moment
    }
}

#[derive(Debug)]
struct Node {
    parent: Option<Arc<Node>>,
    receipt: HistoryReceipt,
}

impl Drop for Node {
    fn drop(&mut self) {
        let mut parent = self.parent.take();
        while let Some(node) = parent {
            match Arc::try_unwrap(node) {
                Ok(mut node) => parent = node.parent.take(),
                Err(_) => break,
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct FaultHistory(Arc<Node>);

impl PartialEq for FaultHistory {
    fn eq(&self, other: &Self) -> bool {
        self.receipt() == other.receipt()
    }
}
impl Eq for FaultHistory {}

impl FaultHistory {
    pub fn genesis(receipt: HistoryReceipt) -> Result<Self, String> {
        receipt.validate()?;
        if receipt.depth != 0 {
            return Err("fault history root is not genesis".to_owned());
        }
        Ok(Self(Arc::new(Node {
            parent: None,
            receipt,
        })))
    }

    pub fn push(&self, receipt: HistoryReceipt) -> Result<Self, String> {
        receipt.validate()?;
        if !receipt.follows(self.receipt()) {
            return Err("fault history receipt differs from its parent".to_owned());
        }
        Ok(Self(Arc::new(Node {
            parent: Some(Arc::clone(&self.0)),
            receipt,
        })))
    }

    pub fn receipt(&self) -> &HistoryReceipt {
        &self.0.receipt
    }
    pub fn len(&self) -> usize {
        self.receipt().depth()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn ancestor(&self, depth: usize) -> Option<Self> {
        if depth > self.len() {
            return None;
        }
        let mut node = Arc::clone(&self.0);
        while node.receipt.depth > depth {
            node = Arc::clone(node.parent.as_ref()?);
        }
        Some(Self(node))
    }
    pub fn receipts(&self) -> Vec<HistoryReceipt> {
        let mut out = Vec::with_capacity(self.len() + 1);
        let mut node = Some(&self.0);
        while let Some(current) = node {
            out.push(current.receipt.clone());
            node = current.parent.as_ref();
        }
        out.reverse();
        out
    }
    pub fn actions(&self) -> Vec<FaultAction> {
        self.receipts()
            .into_iter()
            .filter_map(|r| r.action)
            .collect()
    }
    pub fn matches_actions(&self, actions: &[FaultAction]) -> bool {
        if actions.len() != self.len() {
            return false;
        }
        let mut node = Some(&self.0);
        for action in actions.iter().rev() {
            let Some(current) = node else {
                return false;
            };
            if current.receipt.action != Some(*action) {
                return false;
            }
            node = current.parent.as_ref();
        }
        true
    }

    pub fn certificates(&self) -> Vec<Option<[u8; 32]>> {
        self.receipts()
            .into_iter()
            .skip(1)
            .map(|r| r.certificate)
            .collect()
    }
    pub fn visit_allocations(&self, mut visit: impl FnMut(usize, usize) -> bool) {
        let mut node = Some(&self.0);
        while let Some(current) = node {
            if !visit(
                Arc::as_ptr(current) as usize,
                size_of::<Node>() + 2 * size_of::<usize>(),
            ) {
                break;
            }
            node = current.parent.as_ref();
        }
    }
    pub fn allocations(&self) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut node = Some(&self.0);
        while let Some(current) = node {
            out.push((
                Arc::as_ptr(current) as usize,
                size_of::<Node>() + 2 * size_of::<usize>(),
            ));
            node = current.parent.as_ref();
        }
        out
    }
}

impl Serialize for FaultHistory {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        ("fault-history-v1", self.receipts()).serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for FaultHistory {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let (format, rows): (String, Vec<HistoryReceipt>) = Deserialize::deserialize(deserializer)?;
        if format != "fault-history-v1" {
            return Err(serde::de::Error::custom("unsupported fault history format"));
        }
        let mut rows = rows.into_iter();
        let first = rows
            .next()
            .ok_or_else(|| serde::de::Error::custom("empty fault history"))?;
        let mut history = Self::genesis(first).map_err(serde::de::Error::custom)?;
        for row in rows {
            history = history.push(row).map_err(serde::de::Error::custom)?;
        }
        Ok(history)
    }
}

#[derive(Default)]
pub struct FaultHistoryRetention {
    nodes: std::collections::BTreeMap<[u8; 32], FaultHistory>,
}

impl FaultHistoryRetention {
    fn index_charge() -> usize {
        size_of::<([u8; 32], FaultHistory)>() + 64
    }

    fn node_charge() -> usize {
        size_of::<Node>() + 2 * size_of::<usize>()
    }

    fn normalize(&self, history: &mut FaultHistory) -> Result<usize, &'static str> {
        let mut tail = Vec::new();
        let mut current = Some(Arc::clone(&history.0));
        let mut base = None;
        let mut visits = 0;
        while let Some(node) = current {
            visits += 1;
            node.receipt
                .validate()
                .map_err(|_| "invalid fault history receipt")?;
            if let Some(parent) = &node.parent {
                if !node.receipt.follows(&parent.receipt) {
                    return Err("fault history parent differs");
                }
            } else if node.receipt.depth() != 0 {
                return Err("fault history lacks genesis");
            }
            if let Some(existing) = self.nodes.get(&node.receipt.root()) {
                if existing.receipt() != &node.receipt {
                    return Err("conflicting fault history root");
                }
                base = Some(existing.clone());
                break;
            }
            current = node.parent.as_ref().map(Arc::clone);
            tail.push(node);
        }
        for original in tail.into_iter().rev() {
            let same_parent = match (&original.parent, &base) {
                (None, None) => true,
                (Some(parent), Some(base)) => Arc::ptr_eq(parent, &base.0),
                _ => false,
            };
            base = Some(if same_parent {
                FaultHistory(original)
            } else {
                base.as_ref()
                    .ok_or("fault history parent missing")?
                    .push(original.receipt.clone())
                    .map_err(|_| "invalid fault history extension")?
            });
        }
        *history = base.ok_or("fault history missing")?;
        Ok(visits)
    }
}

impl searcher::search::archive::SnapshotRetention<crate::target::FaultSnapshot>
    for FaultHistoryRetention
{
    fn canonicalize(
        &mut self,
        snapshot: &mut crate::target::FaultSnapshot,
    ) -> Result<(), &'static str> {
        self.normalize(&mut snapshot.history).map(|_| ())
    }

    fn retain(
        &mut self,
        snapshot: &crate::target::FaultSnapshot,
        memory: &mut searcher::search::archive::SharedSnapshotMemory,
    ) {
        let mut current = Some(&snapshot.history.0);
        while let Some(node) = current {
            if !memory.retain(Arc::as_ptr(node) as usize, Self::node_charge()) {
                break;
            }
            let previous = self
                .nodes
                .insert(node.receipt.root(), FaultHistory(Arc::clone(node)));
            assert!(previous.is_none(), "uncanonicalized fault history retained");
            current = node.parent.as_ref();
        }
    }

    fn release(
        &mut self,
        snapshot: &crate::target::FaultSnapshot,
        memory: &mut searcher::search::archive::SharedSnapshotMemory,
    ) {
        let mut current = Some(&snapshot.history.0);
        while let Some(node) = current {
            if !memory.release(Arc::as_ptr(node) as usize, Self::node_charge()) {
                break;
            }
            let registered = self
                .nodes
                .remove(&node.receipt.root())
                .expect("released unregistered fault history");
            assert!(
                Arc::ptr_eq(&registered.0, node),
                "released noncanonical fault history"
            );
            current = node.parent.as_ref();
        }
    }

    fn clear(&mut self) {
        self.nodes.clear();
    }

    fn standalone_bytes(&self, snapshot: &crate::target::FaultSnapshot) -> usize {
        let per_node =
            searcher::search::archive::SharedSnapshotMemory::entry_charge(Self::node_charge())
                .checked_add(Self::index_charge())
                .expect("fault history charge overflow");
        snapshot
            .history
            .len()
            .checked_add(1)
            .and_then(|nodes| nodes.checked_mul(per_node))
            .and_then(|bytes| bytes.checked_add(size_of::<Self>()))
            .expect("fault history standalone charge overflow")
    }

    fn metadata_bytes(&self) -> usize {
        self.nodes
            .len()
            .checked_mul(Self::index_charge())
            .and_then(|bytes| bytes.checked_add(size_of::<Self>()))
            .expect("fault history index charge overflow")
    }
}

impl Default for FaultHistory {
    fn default() -> Self {
        Self::genesis(
            HistoryReceipt::genesis(
                [0; 32],
                FaultEndpointCertificate::KvmWholeState([0; 32]),
                Some((0, [0; 32])),
                &FaultObservations::default(),
            )
            .expect("default history receipt"),
        )
        .expect("default genesis")
    }
}

#[cfg(test)]
pub fn fixture_event_history(endpoint: FaultEndpointCertificate) -> Option<(u64, [u8; 32])> {
    match endpoint {
        FaultEndpointCertificate::KvmWholeState(_) => Some((0, [0; 32])),
        _ => None,
    }
}

#[cfg(test)]
impl FaultHistory {
    pub fn fixture(
        actions: &[FaultAction],
        certificates: &[Option<[u8; 32]>],
        endpoint: Option<FaultEndpointCertificate>,
        observation: &FaultObservations,
    ) -> Self {
        let endpoint = endpoint.unwrap_or(FaultEndpointCertificate::KvmWholeState([17; 32]));
        let genesis_observation = if actions.is_empty() {
            observation.clone()
        } else {
            FaultObservations::default()
        };
        let mut history = Self::genesis(
            HistoryReceipt::genesis(
                [0; 32],
                endpoint,
                crate::history::fixture_event_history(endpoint),
                &genesis_observation,
            )
            .unwrap(),
        )
        .unwrap();
        for (index, action) in actions.iter().enumerate() {
            let mut receipt = history
                .receipt()
                .extend(
                    *action,
                    endpoint,
                    crate::history::fixture_event_history(endpoint),
                    observation,
                )
                .unwrap();
            if let Some(certificate) = certificates.get(index) {
                receipt.certificate = *certificate;
                receipt.root = receipt.root_digest().unwrap();
            }
            history = history.push(receipt).unwrap();
        }
        history
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::FaultOperation;
    #[test]
    fn retained_history_normalization_matches_checkpoint_physical_charges() {
        use searcher::search::archive::{SharedSnapshotMemory, SnapshotRetention};
        let endpoint = FaultEndpointCertificate::KvmWholeState([17; 32]);
        let observation = FaultObservations::default();
        let action = FaultOperation::Wait(std::num::NonZeroU16::MIN).into();
        let other = FaultOperation::Wait(std::num::NonZeroU16::new(2).unwrap()).into();
        let root = FaultHistory::genesis(
            HistoryReceipt::genesis(
                [3; 32],
                endpoint,
                crate::history::fixture_event_history(endpoint),
                &observation,
            )
            .unwrap(),
        )
        .unwrap();
        let prefix = root
            .push(
                root.receipt()
                    .extend(
                        action,
                        endpoint,
                        crate::history::fixture_event_history(endpoint),
                        &observation,
                    )
                    .unwrap(),
            )
            .unwrap();
        let left = prefix
            .push(
                prefix
                    .receipt()
                    .extend(
                        action,
                        endpoint,
                        crate::history::fixture_event_history(endpoint),
                        &observation,
                    )
                    .unwrap(),
            )
            .unwrap();
        let right = prefix
            .push(
                prefix
                    .receipt()
                    .extend(
                        other,
                        endpoint,
                        crate::history::fixture_event_history(endpoint),
                        &observation,
                    )
                    .unwrap(),
            )
            .unwrap();
        let mut snapshots = [left, right].map(|history| crate::target::FaultSnapshot {
            event_history: crate::history::fixture_event_history(endpoint),

            history: postcard::from_bytes(&postcard::to_allocvec(&history).unwrap()).unwrap(),
            observation: observation.clone(),
            failed: false,
            endpoint: Some(endpoint),
        });
        assert_ne!(
            snapshots[0].history.ancestor(1).unwrap().allocations(),
            snapshots[1].history.ancestor(1).unwrap().allocations()
        );
        let mut retention = FaultHistoryRetention::default();
        let mut memory = SharedSnapshotMemory::default();
        retention.canonicalize(&mut snapshots[0]).unwrap();
        retention.retain(&snapshots[0], &mut memory);
        assert_eq!(retention.normalize(&mut snapshots[1].history).unwrap(), 2);
        retention.retain(&snapshots[1], &mut memory);
        assert_eq!(retention.nodes.len(), 4);
        assert_eq!(
            snapshots[0].history.ancestor(1).unwrap().allocations(),
            snapshots[1].history.ancestor(1).unwrap().allocations()
        );
        let live_bytes = memory.bytes() + retention.metadata_bytes();
        let mut resumed: [crate::target::FaultSnapshot; 2] = snapshots.clone().map(|snapshot| {
            postcard::from_bytes(&postcard::to_allocvec(&snapshot).unwrap()).unwrap()
        });
        let mut resumed_retention = FaultHistoryRetention::default();
        let mut resumed_memory = SharedSnapshotMemory::default();
        for snapshot in &mut resumed {
            resumed_retention.canonicalize(snapshot).unwrap();
            resumed_retention.retain(snapshot, &mut resumed_memory);
        }
        assert_eq!(
            live_bytes,
            resumed_memory.bytes() + resumed_retention.metadata_bytes()
        );
        let before = retention.nodes.len();
        let mut rejected = snapshots[1].clone();
        retention.canonicalize(&mut rejected).unwrap();
        assert_eq!(retention.nodes.len(), before);
        retention.release(&snapshots[0], &mut memory);
        assert_eq!(retention.nodes.len(), 3);
        retention.release(&snapshots[1], &mut memory);
        assert!(retention.nodes.is_empty());
        assert_eq!(memory.bytes(), 0);
        for snapshot in &resumed {
            resumed_retention.release(snapshot, &mut resumed_memory);
        }
        assert!(resumed_retention.nodes.is_empty());
        assert_eq!(resumed_memory.bytes(), 0);
    }

    #[test]
    fn canonicalization_preserves_the_live_bootstrap_genesis_allocation() {
        use searcher::search::archive::{SharedSnapshotMemory, SnapshotRetention};
        let endpoint = FaultEndpointCertificate::KvmWholeState([17; 32]);
        let observation = FaultObservations::default();
        let root = FaultHistory::genesis(
            HistoryReceipt::genesis(
                [3; 32],
                endpoint,
                crate::history::fixture_event_history(endpoint),
                &observation,
            )
            .unwrap(),
        )
        .unwrap();
        let child = root
            .push(
                root.receipt()
                    .extend(
                        FaultOperation::Wait(std::num::NonZeroU16::MIN).into(),
                        endpoint,
                        crate::history::fixture_event_history(endpoint),
                        &observation,
                    )
                    .unwrap(),
            )
            .unwrap();
        let mut decoded: FaultHistory =
            postcard::from_bytes(&postcard::to_allocvec(&child).unwrap()).unwrap();
        assert_ne!(decoded.allocations().last(), root.allocations().last());
        let mut retention = FaultHistoryRetention::default();
        let mut memory = SharedSnapshotMemory::default();
        let mut seed = crate::target::FaultSnapshot {
            event_history: crate::history::fixture_event_history(endpoint),

            history: root.clone(),
            observation,
            failed: false,
            endpoint: Some(endpoint),
        };
        retention.canonicalize(&mut seed).unwrap();
        retention.retain(&seed, &mut memory);
        assert_eq!(seed.history.allocations(), root.allocations());
        assert_eq!(retention.normalize(&mut decoded).unwrap(), 2);
        assert_eq!(decoded.allocations().last(), root.allocations().last());
        retention.release(&seed, &mut memory);
        assert_eq!(
            decoded.ancestor(0).unwrap().allocations(),
            root.allocations()
        );
    }

    #[test]
    fn unavailable_machine_evidence_still_binds_intermediate_observations() {
        let endpoint = FaultEndpointCertificate::UmlWholeStateUnavailable;
        let observation = FaultObservations::default();
        let root = HistoryReceipt::genesis(
            [3; 32],
            endpoint,
            crate::history::fixture_event_history(endpoint),
            &observation,
        )
        .unwrap();
        let action = FaultOperation::Wait(std::num::NonZeroU16::MIN).into();
        let first = root
            .extend(
                action,
                endpoint,
                crate::history::fixture_event_history(endpoint),
                &observation,
            )
            .unwrap();
        let mut different = observation.clone();
        different.alive = 7;
        let divergent = root
            .extend(
                action,
                endpoint,
                crate::history::fixture_event_history(endpoint),
                &different,
            )
            .unwrap();
        assert_ne!(first.root(), divergent.root());
        assert_eq!(first.certificate(), None);
        assert_eq!(divergent.certificate(), None);
        let converged = first
            .extend(
                action,
                endpoint,
                crate::history::fixture_event_history(endpoint),
                &observation,
            )
            .unwrap();
        let divergent_converged = divergent
            .extend(
                action,
                endpoint,
                crate::history::fixture_event_history(endpoint),
                &observation,
            )
            .unwrap();
        assert_ne!(converged.root(), divergent_converged.root());
        assert_eq!(converged.endpoint(), endpoint);
        assert_eq!(divergent_converged.endpoint(), endpoint);
    }

    #[test]
    fn persistent_histories_share_nodes_round_trip_and_drop_iteratively() {
        for depth in [1, 8, 32, 128, 1024] {
            let endpoint = FaultEndpointCertificate::KvmWholeState([17; 32]);
            let mut observation = FaultObservations::default();
            let mut history = FaultHistory::genesis(
                HistoryReceipt::genesis(
                    [3; 32],
                    endpoint,
                    crate::history::fixture_event_history(endpoint),
                    &observation,
                )
                .unwrap(),
            )
            .unwrap();
            let root = history.clone();
            for _ in 0..depth {
                observation.moment += 1;
                history = history
                    .push(
                        history
                            .receipt()
                            .extend(
                                FaultOperation::Wait(std::num::NonZeroU16::MIN).into(),
                                endpoint,
                                crate::history::fixture_event_history(endpoint),
                                &observation,
                            )
                            .unwrap(),
                    )
                    .unwrap();
            }
            assert_eq!(history.len(), depth);
            assert_eq!(history.ancestor(0).unwrap(), root);
            assert_eq!(history.allocations().last(), root.allocations().last());
            let encoded = postcard::to_allocvec(&history).unwrap();
            let decoded: FaultHistory = postcard::from_bytes(&encoded).unwrap();
            assert_eq!(history, decoded);
            assert_eq!(history.actions(), decoded.actions());
            drop(history);
            drop(decoded);
        }
    }
}
