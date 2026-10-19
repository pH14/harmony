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
        "fault-replay-endpoint-v4",
        execution_contract::Assurance::ordinary().identity(),
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
    assurance_identity: [u8; 32],
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
            assurance_identity: execution_contract::Assurance::ordinary().identity(),
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
            assurance_identity: self.assurance_identity,
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
                self.assurance_identity,
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

    pub fn assurance_identity(&self) -> [u8; 32] {
        self.assurance_identity
    }

    pub fn require_qualified(
        &self,
        composition: execution_contract::Composition,
    ) -> Result<execution_contract::Qualified, String> {
        self.validate()?;
        execution_contract::Assurance::ordinary()
            .require_qualified(composition)
            .map_err(|error| error.to_string())
    }

    pub fn validate(&self) -> Result<(), String> {
        let genesis = self.depth == 0;
        if self.assurance_identity != execution_contract::Assurance::ordinary().identity()
            || genesis != self.parent.is_none()
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

impl Node {
    fn allocation_bytes() -> usize {
        size_of::<Self>() + 2 * size_of::<usize>()
    }
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
    fn nodes(&self) -> impl Iterator<Item = &Node> {
        std::iter::successors(Some(self.0.as_ref()), |node| node.parent.as_deref())
    }
    pub fn receipts(&self) -> Vec<&HistoryReceipt> {
        let mut receipts: Vec<_> = self.nodes().map(|node| &node.receipt).collect();
        receipts.reverse();
        receipts
    }
    pub fn receipts_after(
        &self,
        depth: usize,
    ) -> Option<impl DoubleEndedIterator<Item = &HistoryReceipt> + ExactSizeIterator> {
        if depth > self.len() {
            return None;
        }
        let receipts: Vec<_> = self
            .nodes()
            .take_while(|node| node.receipt.depth() > depth)
            .map(|node| &node.receipt)
            .collect();
        Some(receipts.into_iter().rev())
    }
    pub fn actions(&self) -> Vec<FaultAction> {
        let mut actions: Vec<_> = self
            .nodes()
            .filter_map(|node| node.receipt.action())
            .collect();
        actions.reverse();
        actions
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

    #[cfg(test)]
    pub fn certificates(&self) -> Vec<Option<[u8; 32]>> {
        let mut certificates: Vec<_> = self
            .nodes()
            .take(self.len())
            .map(|node| node.receipt.certificate())
            .collect();
        certificates.reverse();
        certificates
    }
    #[cfg(test)]
    fn allocations(&self) -> Vec<(usize, usize)> {
        self.nodes()
            .map(|node| (std::ptr::from_ref(node) as usize, Node::allocation_bytes()))
            .collect()
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

pub struct FaultSnapshotCodec;

impl searcher::search::snapshot_codec::CheckpointSnapshot for crate::target::FaultSnapshot {
    type Codec = FaultSnapshotCodec;
    fn checkpoint_image_charge(
        &self,
        budget: &mut searcher::search::checkpoint::image::ImageBudget,
    ) -> Result<(), Box<dyn std::error::Error>> {
        budget.reserve(std::mem::size_of::<Self>())?;
        budget.array::<crate::target::ParkLanding>(self.observation.parks.capacity())?;
        budget.array::<crate::target::ParkRead>(self.observation.park_reads.capacity())?;
        if let Some(check) = &self.observation.check {
            budget.array::<String>(check.points.capacity())?;
            for point in &check.points {
                budget.array::<u8>(point.capacity())?;
            }
        }
        for (name, outcome) in &self.observation.assertions.0 {
            budget.map_entry::<String, crate::assertion::AssertionOutcome>()?;
            budget.array::<u8>(name.capacity())?;
            budget.array::<u8>(outcome.message.capacity())?;
            budget.array::<u8>(outcome.location.capacity())?;
        }
        for node in self.history.nodes() {
            if !budget.shared(
                "fault-history-node",
                std::ptr::from_ref(node) as usize,
                Node::allocation_bytes(),
            )? {
                break;
            }
        }
        Ok(())
    }
    fn freeze_checkpoint(&self) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(self.clone())
    }
}

#[derive(Serialize, Deserialize)]
struct SnapshotEndpoint {
    observation: FaultObservations,
    failed: bool,
    endpoint: Option<FaultEndpointCertificate>,
    event_history: Option<(u64, [u8; 32])>,
}

impl searcher::search::snapshot_codec::SnapshotCodec<crate::target::FaultSnapshot>
    for FaultSnapshotCodec
{
    const FORMAT: &'static str = "fault-history-dag-v2";
    type DecodeState = std::collections::BTreeMap<[u8; 32], FaultHistory>;
    fn encode(
        snapshot: &crate::target::FaultSnapshot,
        sink: &mut dyn searcher::search::snapshot_codec::SharedRecordSink,
    ) -> searcher::search::snapshot_codec::CodecResult<
        searcher::search::snapshot_codec::EncodedSnapshot,
    > {
        use searcher::search::snapshot_codec::{EncodedSnapshot, SharedRecord};
        let mut tail = Vec::new();
        for node in snapshot.history.nodes() {
            node.receipt.validate()?;
            match &node.parent {
                Some(parent) if node.receipt.follows(&parent.receipt) => {}
                None if node.receipt.depth == 0 => {}
                _ => return Err("fault history node differs from its parent".into()),
            }
            let record = SharedRecord {
                key: node.receipt.root,
                dependencies: node.receipt.parent.into_iter().collect(),
                payload: postcard::to_allocvec(&node.receipt)?,
            };
            if sink.contains(&record)? {
                break;
            }
            tail.push(record);
        }
        for record in tail.into_iter().rev() {
            sink.insert(record)?;
        }
        Ok(EncodedSnapshot {
            roots: vec![snapshot.history.receipt().root()],
            payload: postcard::to_allocvec(&SnapshotEndpoint {
                observation: snapshot.observation.clone(),
                failed: snapshot.failed,
                endpoint: snapshot.endpoint,
                event_history: snapshot.event_history,
            })?,
        })
    }
    fn decode_record(
        state: &mut Self::DecodeState,
        record: searcher::search::snapshot_codec::SharedRecord,
    ) -> searcher::search::snapshot_codec::CodecResult<()> {
        let (receipt, tail): (HistoryReceipt, _) = postcard::take_from_bytes(&record.payload)?;
        if !tail.is_empty()
            || receipt.root != record.key
            || record.dependencies != receipt.parent.into_iter().collect::<Vec<_>>()
            || state.contains_key(&record.key)
        {
            return Err("fault history record key, parent or payload differs".into());
        }
        let history = match receipt.parent {
            Some(parent) => state
                .get(&parent)
                .ok_or("fault history parent is missing")?
                .push(receipt)?,
            None => FaultHistory::genesis(receipt)?,
        };
        state.insert(record.key, history);
        Ok(())
    }
    fn decode_snapshot(
        state: &Self::DecodeState,
        snapshot: searcher::search::snapshot_codec::EncodedSnapshot,
    ) -> searcher::search::snapshot_codec::CodecResult<crate::target::FaultSnapshot> {
        let [root] = snapshot.roots.as_slice() else {
            return Err("fault snapshot requires one history root".into());
        };
        let history = state
            .get(root)
            .ok_or("fault snapshot history root is missing")?
            .clone();
        let (endpoint, tail): (SnapshotEndpoint, _) = postcard::take_from_bytes(&snapshot.payload)?;
        if !tail.is_empty() {
            return Err("fault snapshot endpoint has trailing bytes".into());
        }
        Ok(crate::target::FaultSnapshot {
            history,
            observation: endpoint.observation,
            failed: endpoint.failed,
            endpoint: endpoint.endpoint,
            event_history: endpoint.event_history,
        })
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
            if !memory.retain(Arc::as_ptr(node) as usize, Node::allocation_bytes()) {
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
            if !memory.release(Arc::as_ptr(node) as usize, Node::allocation_bytes()) {
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
            searcher::search::archive::SharedSnapshotMemory::entry_charge(Node::allocation_bytes())
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
    use searcher::search::campaign::{SnapshotCheckpoint, SnapshotCheckpointEntry};
    use searcher::search::snapshot_codec::{
        CodecResult, EncodedSnapshot, SharedRecord, SharedRecordSink, SnapshotCodec,
    };
    use std::collections::BTreeMap;
    #[derive(Default)]
    struct Records {
        rows: BTreeMap<[u8; 32], SharedRecord>,
        order: Vec<SharedRecord>,
        checks: std::cell::Cell<usize>,
    }
    impl SharedRecordSink for Records {
        fn contains(&self, row: &SharedRecord) -> CodecResult<bool> {
            self.checks.set(self.checks.get() + 1);
            match self.rows.get(&row.key) {
                Some(old) if old != row => Err("conflict".into()),
                Some(_) => Ok(true),
                None => Ok(false),
            }
        }
        fn insert(&mut self, row: SharedRecord) -> CodecResult<()> {
            if !self.contains(&row)? {
                self.order.push(row.clone());
                self.rows.insert(row.key, row);
            }
            Ok(())
        }
    }
    fn dag_ladder(depth: usize, domain: u8) -> Vec<crate::target::FaultSnapshot> {
        let endpoint = FaultEndpointCertificate::KvmWholeState([17; 32]);
        let mut observation = FaultObservations::default();
        let mut history = FaultHistory::genesis(
            HistoryReceipt::genesis(
                [domain; 32],
                endpoint,
                fixture_event_history(endpoint),
                &observation,
            )
            .unwrap(),
        )
        .unwrap();
        let mut out = vec![];
        for step in 0..=depth {
            out.push(crate::target::FaultSnapshot {
                history: history.clone(),
                observation: observation.clone(),
                endpoint: Some(endpoint),
                event_history: fixture_event_history(endpoint),
                failed: false,
            });
            if step < depth {
                observation.moment += 1;
                let action = FaultAction::new(
                    FaultOperation::Wait(std::num::NonZeroU16::new(1).unwrap()),
                    std::num::NonZeroU16::new(1).unwrap(),
                );
                history = history
                    .push(
                        history
                            .receipt()
                            .extend(
                                action,
                                endpoint,
                                fixture_event_history(endpoint),
                                &observation,
                            )
                            .unwrap(),
                    )
                    .unwrap();
            }
        }
        out
    }
    #[test]
    fn checkpoint_image_charge_counts_shared_physical_histories_once_and_preserves_frozen_metadata()
    {
        use searcher::search::{
            checkpoint::image::ImageBudget, snapshot_codec::CheckpointSnapshot,
        };
        let mut snapshots = dag_ladder(4, 3);
        let mut budget = ImageBudget::new(usize::MAX);
        snapshots[4].checkpoint_image_charge(&mut budget).unwrap();
        let first = budget.bytes();
        assert!(first >= 5 * Node::allocation_bytes() + size_of::<crate::target::FaultSnapshot>());
        snapshots[3].checkpoint_image_charge(&mut budget).unwrap();
        assert_eq!(
            budget.bytes() - first,
            size_of::<crate::target::FaultSnapshot>()
        );
        let independent = dag_ladder(4, 3);
        let before = budget.bytes();
        independent[4].checkpoint_image_charge(&mut budget).unwrap();
        assert_eq!(budget.bytes() - before, first);
        let mut richer = independent[4].clone();
        richer.observation.parks = Vec::with_capacity(65);
        richer.observation.parks.resize(65, Default::default());
        let mut richer_budget = ImageBudget::new(usize::MAX);
        richer.checkpoint_image_charge(&mut richer_budget).unwrap();
        assert!(
            richer_budget.bytes() - first
                >= richer.observation.parks.capacity() * size_of::<crate::target::ParkLanding>()
        );
        snapshots[4].observation.check = Some(crate::target::CheckEvidence {
            points: vec!["old checkpoint evidence".into()],
            ..Default::default()
        });
        let frozen = snapshots[4].freeze_checkpoint().unwrap();
        assert_eq!(
            frozen.history.receipt().assurance_identity(),
            snapshots[4].history.receipt().assurance_identity()
        );
        let expected = frozen.clone();
        snapshots[4].observation.check.as_mut().unwrap().points[0] = "changed live evidence".into();
        snapshots[4].observation.moment += 1;
        assert_eq!(frozen, expected);
        assert_ne!(frozen.observation, snapshots[4].observation);
        let mut bounded = ImageBudget::new(first - 1);
        assert!(
            independent[4]
                .checkpoint_image_charge(&mut bounded)
                .is_err()
        );
    }

    #[test]
    fn checkpoint_image_charge_covers_every_owned_observation_capacity_and_exact_limit() {
        use searcher::search::{
            checkpoint::image::{ImageBudget, ImageLimit},
            snapshot_codec::CheckpointSnapshot,
        };
        let mut snapshot = dag_ladder(2, 7).pop().unwrap();
        let charge = |snapshot: &crate::target::FaultSnapshot| {
            let mut budget = ImageBudget::new(usize::MAX);
            snapshot.checkpoint_image_charge(&mut budget).unwrap();
            budget.bytes()
        };
        let base = charge(&snapshot);
        snapshot.observation.parks = Vec::with_capacity(31);
        snapshot.observation.park_reads = Vec::with_capacity(47);
        snapshot.observation.check = Some(crate::target::CheckEvidence {
            points: Vec::with_capacity(11),
            ..Default::default()
        });
        let points = &mut snapshot.observation.check.as_mut().unwrap().points;
        let mut point = String::with_capacity(8192);
        point.push_str("check point");
        points.push(point);
        let vectors = snapshot.observation.parks.capacity()
            * size_of::<crate::target::ParkLanding>()
            + snapshot.observation.park_reads.capacity() * size_of::<crate::target::ParkRead>()
            + points.capacity() * size_of::<String>()
            + points[0].capacity();
        assert_eq!(charge(&snapshot) - base, vectors);
        for index in 0..8 {
            let mut name = String::with_capacity(128 + index);
            name.push_str(&format!("assertion-{index}"));
            snapshot.observation.assertions.0.insert(
                name,
                crate::assertion::AssertionOutcome {
                    kind: crate::assertion::AssertionKind::Always,
                    message: String::new(),
                    location: String::new(),
                    passed: true,
                    failed: false,
                },
            );
        }
        let before_values = charge(&snapshot);
        let mut containers = ImageBudget::new(usize::MAX);
        for name in snapshot.observation.assertions.0.keys() {
            containers
                .map_entry::<String, crate::assertion::AssertionOutcome>()
                .unwrap();
            containers.array::<u8>(name.capacity()).unwrap();
        }
        assert_eq!(before_values - base - vectors, containers.bytes());
        let mut value_capacities = 0;
        for outcome in snapshot.observation.assertions.0.values_mut() {
            outcome.message = String::with_capacity(65536);
            outcome.message.push_str(&"message".repeat(4096));
            outcome.location = String::with_capacity(32768);
            outcome.location.push_str(&"location".repeat(1024));
            value_capacities += outcome.message.capacity() + outcome.location.capacity();
        }
        let exact = charge(&snapshot);
        assert_eq!(exact - before_values, value_capacities);
        let mut budget = ImageBudget::new(exact);
        snapshot.checkpoint_image_charge(&mut budget).unwrap();
        assert_eq!(budget.remaining(), 0);
        let frozen = snapshot.freeze_checkpoint().unwrap();
        assert_eq!(frozen, snapshot);
        let shared_history_only = {
            let mut budget = ImageBudget::new(usize::MAX);
            snapshot.checkpoint_image_charge(&mut budget).unwrap();
            let before = budget.bytes();
            snapshot.checkpoint_image_charge(&mut budget).unwrap();
            budget.bytes() - before
        };
        assert!(
            shared_history_only >= value_capacities + vectors + std::mem::size_of_val(&snapshot)
        );
        for limit in [exact - 1, before_values] {
            let mut budget = ImageBudget::new(limit);
            let error = snapshot.checkpoint_image_charge(&mut budget).unwrap_err();
            assert!(error.downcast_ref::<ImageLimit>().is_some());
        }
    }

    #[test]
    fn dag_checkpoint_writes_each_semantic_receipt_once_and_restores_shared_allocations() {
        let snapshots = dag_ladder(128, 3);
        let equivalent = dag_ladder(128, 3);
        let other = dag_ladder(2, 4);
        let mut records = Records::default();
        for snapshot in snapshots.iter().chain(&equivalent).chain(&other) {
            FaultSnapshotCodec::encode(snapshot, &mut records).unwrap();
        }
        assert_eq!(records.order.len(), 129 + 3);
        assert_eq!(records.checks.get(), 2 * (129 + 3) + 128 + 2 + 129);

        let mut state = BTreeMap::new();
        for row in records.order {
            FaultSnapshotCodec::decode_record(&mut state, row).unwrap();
        }
        let checkpoint = SnapshotCheckpoint {
            format: "dag-test".into(),
            entries: snapshots
                .iter()
                .chain(&other)
                .enumerate()
                .map(|(id, snapshot)| SnapshotCheckpointEntry {
                    id: id as u64,
                    snapshot: snapshot.clone(),
                })
                .collect(),
        };
        let bytes = checkpoint.to_bytes().unwrap();
        let legacy_bytes: usize = checkpoint
            .entries
            .iter()
            .map(|entry| postcard::to_allocvec(&entry.snapshot).unwrap().len())
            .sum();
        assert!(bytes.len() < legacy_bytes / 8);
        #[derive(Deserialize)]
        enum WireFrame {
            Shared(SharedRecord),
            Snapshot(u64, EncodedSnapshot),
        }
        fn frame(input: &mut &[u8]) -> Vec<u8> {
            use std::io::Read;
            let mut len = [0; 8];
            input.read_exact(&mut len).unwrap();
            let len = usize::try_from(u64::from_le_bytes(len)).unwrap();
            let payload = input[..len].to_vec();
            *input = &input[len..];
            let mut checksum = [0; 32];
            input.read_exact(&mut checksum).unwrap();
            assert_eq!(<[u8; 32]>::from(Sha256::digest(&payload)), checksum);
            payload
        }
        let mut input = &bytes[8..];
        let (_, codec, entries): (String, String, u64) =
            postcard::from_bytes(&frame(&mut input)).unwrap();
        assert_eq!(codec, "fault-history-dag-v2");
        assert_eq!(entries, 132);
        let mut rows = std::collections::BTreeSet::new();
        let mut endpoints = 0;
        while endpoints < entries {
            match postcard::from_bytes::<WireFrame>(&frame(&mut input)).unwrap() {
                WireFrame::Shared(row) => {
                    assert!(rows.insert(row.key));
                }
                WireFrame::Snapshot(id, encoded) => {
                    assert_eq!(id, endpoints);
                    assert_eq!(encoded.roots.len(), 1);
                    endpoints += 1;
                }
            }
        }
        assert_eq!(rows.len(), 132);
        let independently_allocated = SnapshotCheckpoint {
            format: "dag-test".into(),
            entries: equivalent
                .iter()
                .chain(&other)
                .enumerate()
                .map(|(id, snapshot)| SnapshotCheckpointEntry {
                    id: id as u64,
                    snapshot: snapshot.clone(),
                })
                .collect(),
        };
        assert_eq!(bytes, independently_allocated.to_bytes().unwrap());
        let decoded =
            SnapshotCheckpoint::<crate::target::FaultSnapshot>::from_bytes(&bytes, "dag-test")
                .unwrap();
        assert_eq!(decoded, checkpoint);
        for pair in decoded.entries[..129].windows(2) {
            assert!(Arc::ptr_eq(
                &pair[0].snapshot.history.0,
                pair[1].snapshot.history.0.parent.as_ref().unwrap()
            ));
        }
        assert!(!Arc::ptr_eq(
            &decoded.entries[0].snapshot.history.0,
            &decoded.entries[129].snapshot.history.0
        ));
        assert_eq!(
            decoded.entries[128].snapshot.history.allocations().len(),
            129
        );
        use searcher::search::archive::{SharedSnapshotMemory, SnapshotRetention};
        let mut retention = FaultHistoryRetention::default();
        let mut memory = SharedSnapshotMemory::default();
        for entry in &decoded.entries {
            retention.retain(&entry.snapshot, &mut memory);
        }
        assert_eq!(retention.nodes.len(), 132);
        let mut expected_memory = SharedSnapshotMemory::default();
        for identity in 0..132 {
            assert!(expected_memory.retain(identity, Node::allocation_bytes()));
        }
        assert_eq!(memory.bytes(), expected_memory.bytes());
        for entry in &decoded.entries {
            retention.release(&entry.snapshot, &mut memory);
        }
        assert_eq!(memory.bytes(), 0);
        assert!(retention.nodes.is_empty());
    }
    #[test]
    fn dag_codec_preserves_assurance_and_refuses_relabelled_receipts_and_old_codec() {
        #[derive(Serialize)]
        enum Frame {
            Shared(SharedRecord),
            Snapshot(u64, EncodedSnapshot),
        }
        fn frame(out: &mut Vec<u8>, value: &impl Serialize) {
            let bytes = postcard::to_allocvec(value).unwrap();
            out.extend_from_slice(&u64::try_from(bytes.len()).unwrap().to_le_bytes());
            out.extend_from_slice(&bytes);
            out.extend_from_slice(&Sha256::digest(&bytes));
        }
        let snapshot = dag_ladder(0, 9).pop().unwrap();
        let checkpoint = SnapshotCheckpoint {
            format: "assurance-dag-test".into(),
            entries: vec![SnapshotCheckpointEntry {
                id: 7,
                snapshot: snapshot.clone(),
            }],
        };
        let bytes = checkpoint.to_bytes().unwrap();
        let restored = SnapshotCheckpoint::<crate::target::FaultSnapshot>::from_bytes(
            &bytes,
            &checkpoint.format,
        )
        .unwrap();
        assert_eq!(restored, checkpoint);
        assert_eq!(
            restored.entries[0]
                .snapshot
                .history
                .receipt()
                .assurance_identity(),
            execution_contract::Assurance::ordinary().identity()
        );
        let mut records = Records::default();
        let endpoint = FaultSnapshotCodec::encode(&snapshot, &mut records).unwrap();
        let mut receipt = snapshot.history.receipt().clone();
        receipt.assurance_identity = [31; 32];
        receipt.root = receipt.root_digest().unwrap();
        let row = SharedRecord {
            key: receipt.root,
            dependencies: Vec::new(),
            payload: postcard::to_allocvec(&receipt).unwrap(),
        };
        let mut state = BTreeMap::new();
        assert!(FaultSnapshotCodec::decode_record(&mut state, row.clone()).is_err());
        assert!(state.is_empty());
        let forged = crate::target::FaultSnapshot {
            history: FaultHistory(Arc::new(Node {
                parent: None,
                receipt: receipt.clone(),
            })),
            ..snapshot
        };
        assert!(FaultSnapshotCodec::encode(&forged, &mut Records::default()).is_err());
        let mut changed = b"HMSNAP02".to_vec();
        frame(
            &mut changed,
            &("assurance-dag-test", FaultSnapshotCodec::FORMAT, 1_u64),
        );
        frame(&mut changed, &Frame::Shared(row));
        frame(
            &mut changed,
            &Frame::Snapshot(
                7,
                EncodedSnapshot {
                    roots: vec![receipt.root],
                    payload: endpoint.payload,
                },
            ),
        );
        frame(&mut changed, &"snapshot-checkpoint-end-v2");
        let error = SnapshotCheckpoint::<crate::target::FaultSnapshot>::from_bytes(
            &changed,
            &checkpoint.format,
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("malformed fault history receipt")
        );
        let mut old = b"HMSNAP02".to_vec();
        frame(
            &mut old,
            &("assurance-dag-test", "fault-history-dag-v1", 1_u64),
        );
        let error = SnapshotCheckpoint::<crate::target::FaultSnapshot>::from_bytes(
            &old,
            &checkpoint.format,
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "snapshot checkpoint format is not recognized"
        );
    }

    #[test]
    fn dag_codec_rejects_valid_receipts_that_do_not_continue_their_known_parent() {
        let snapshots = dag_ladder(1, 5);
        let mut records = Records::default();
        FaultSnapshotCodec::encode(&snapshots[1], &mut records).unwrap();
        for mutation in 0..5 {
            let mut receipt: HistoryReceipt =
                postcard::from_bytes(&records.order[1].payload).unwrap();
            match mutation {
                0 => receipt.domain = [8; 32],
                1 => receipt.depth += 1,
                2 => receipt.parent_path = Some([8; 32]),
                3 => receipt.previous_moment = receipt.moment,
                _ => {
                    receipt.endpoint = FaultEndpointCertificate::UmlWholeStateUnavailable;
                    receipt.certificate = None;
                    receipt.event_history = None;
                }
            }
            receipt.path = receipt.path_digest().unwrap();
            receipt.root = receipt.root_digest().unwrap();
            receipt.validate().unwrap();
            assert!(!receipt.follows(snapshots[0].history.receipt()));
            let row = SharedRecord {
                key: receipt.root(),
                dependencies: receipt.parent.into_iter().collect(),
                payload: postcard::to_allocvec(&receipt).unwrap(),
            };
            let mut state = BTreeMap::new();
            FaultSnapshotCodec::decode_record(&mut state, records.order[0].clone()).unwrap();
            assert!(
                FaultSnapshotCodec::decode_record(&mut state, row)
                    .unwrap_err()
                    .to_string()
                    .contains("differs from its parent"),
                "mutation {mutation}"
            );
            assert_eq!(state.len(), 1);
        }
    }

    #[test]
    fn dag_codec_rejects_intermediate_key_parent_domain_and_backend_corruption() {
        let snapshots = dag_ladder(3, 5);
        let mut records = Records::default();
        let encoded = FaultSnapshotCodec::encode(&snapshots[3], &mut records).unwrap();
        assert_eq!(records.order.len(), 4);
        for mutate in 0..7 {
            let mut rows = records.order.clone();
            let mut receipt: HistoryReceipt = postcard::from_bytes(&rows[1].payload).unwrap();
            match mutate {
                0 => receipt.certificate = Some([8; 32]),
                1 => receipt.domain = [8; 32],
                2 => receipt.previous_moment += 1,
                3 => receipt.depth += 1,
                4 => receipt.endpoint = FaultEndpointCertificate::UmlWholeStateUnavailable,
                5 => rows[1].key = [8; 32],
                _ => rows[1].dependencies = vec![[8; 32]],
            }
            rows[1].payload = postcard::to_allocvec(&receipt).unwrap();
            let mut state = BTreeMap::new();
            assert!(
                rows.into_iter()
                    .try_for_each(|row| FaultSnapshotCodec::decode_record(&mut state, row))
                    .is_err()
            );
            assert_eq!(
                snapshots[3].observation,
                snapshots.last().unwrap().observation
            );
        }
        let mut state = BTreeMap::new();
        for row in records.order {
            FaultSnapshotCodec::decode_record(&mut state, row).unwrap();
        }
        assert!(
            FaultSnapshotCodec::decode_snapshot(
                &state,
                EncodedSnapshot {
                    roots: vec![[0; 32]],
                    ..encoded
                }
            )
            .is_err()
        );
    }

    #[test]
    fn endpoint_certificates_bind_unqualified_assurance_and_cannot_claim_portability() {
        for endpoint in [
            FaultEndpointCertificate::KvmWholeState([17; 32]),
            FaultEndpointCertificate::UmlWholeStateUnavailable,
        ] {
            let observation = FaultObservations::default();
            let mut receipt = HistoryReceipt::genesis(
                [3; 32],
                endpoint,
                fixture_event_history(endpoint),
                &observation,
            )
            .unwrap();
            assert_eq!(
                receipt.assurance_identity(),
                execution_contract::Assurance::ordinary().identity()
            );
            assert_eq!(
                receipt.certificate().is_some(),
                matches!(endpoint, FaultEndpointCertificate::KvmWholeState(_))
            );
            for composition in [
                execution_contract::Composition::RetainedLinuxContinuation9673,
                execution_contract::Composition::UmlWholeState,
            ] {
                assert!(
                    receipt
                        .require_qualified(composition)
                        .unwrap_err()
                        .contains("qualification refused")
                );
            }
            receipt.assurance_identity = [31; 32];
            receipt.root = receipt.root_digest().unwrap();
            assert!(receipt.validate().is_err());
        }
        let observation = FaultObservations::default();
        let hash = [17; 32];
        let history = (0, [0; 32]);
        let expected = postcard::to_allocvec(&(
            "fault-replay-endpoint-v4",
            execution_contract::Assurance::ordinary().identity(),
            control_proto::EventHistory::VERSION,
            hash,
            history,
            &observation,
            false,
        ))
        .unwrap();
        assert_eq!(
            replay_certificate(hash, history, &observation, false).unwrap(),
            <[u8; 32]>::from(Sha256::digest(expected))
        );
        let previous = postcard::to_allocvec(&(
            "fault-replay-endpoint-v3",
            control_proto::EventHistory::VERSION,
            hash,
            history,
            &observation,
            false,
        ))
        .unwrap();
        assert_ne!(
            replay_certificate(hash, history, &observation, false).unwrap(),
            <[u8; 32]>::from(Sha256::digest(previous))
        );
    }

    #[test]
    fn borrowed_suffix_traversal_preserves_actions_receipts_and_wire_encoding() {
        let endpoint = FaultEndpointCertificate::KvmWholeState([17; 32]);
        let mut observation = FaultObservations::default();
        let mut history = FaultHistory::genesis(
            HistoryReceipt::genesis(
                [3; 32],
                endpoint,
                fixture_event_history(endpoint),
                &observation,
            )
            .unwrap(),
        )
        .unwrap();
        let mut actions = Vec::new();
        for depth in 0..=1024 {
            if [0, 1, 8, 128, 1024].contains(&depth) {
                assert_eq!(history.actions(), actions);
                assert!(history.matches_actions(&actions));
                let receipts = history.receipts();
                assert_eq!(receipts.len(), depth + 1);
                for start in [0, depth / 2, depth] {
                    let suffix = history.receipts_after(start).unwrap();
                    assert_eq!(suffix.len(), depth - start);
                    for (actual, expected) in suffix.zip(&receipts[start + 1..]) {
                        assert!(std::ptr::eq(actual, *expected));
                    }
                }
                assert!(history.receipts_after(depth + 1).is_none());
                let owned: Vec<HistoryReceipt> = receipts.into_iter().cloned().collect();
                assert_eq!(
                    postcard::to_allocvec(&history).unwrap(),
                    postcard::to_allocvec(&("fault-history-v1", owned)).unwrap()
                );
            }
            if depth == 1024 {
                break;
            }
            let action = FaultOperation::Wait(
                std::num::NonZeroU16::new(u16::try_from(depth + 1).unwrap()).unwrap(),
            )
            .into();
            observation.moment += 1;
            let receipt = history
                .receipt()
                .extend(
                    action,
                    endpoint,
                    fixture_event_history(endpoint),
                    &observation,
                )
                .unwrap();
            history = history.push(receipt).unwrap();
            actions.push(action);
        }
    }

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
            let snapshot = crate::target::FaultSnapshot {
                history: history.clone(),
                observation: observation.clone(),
                endpoint: Some(endpoint),
                event_history: fixture_event_history(endpoint),
                failed: false,
            };
            let checkpoint = SnapshotCheckpoint {
                format: "dag-deep-test".into(),
                entries: vec![
                    SnapshotCheckpointEntry {
                        id: 0,
                        snapshot: crate::target::FaultSnapshot {
                            history: root.clone(),
                            observation: FaultObservations::default(),
                            ..snapshot.clone()
                        },
                    },
                    SnapshotCheckpointEntry { id: 1, snapshot },
                ],
            };
            let encoded = checkpoint.to_bytes().unwrap();
            let restored = SnapshotCheckpoint::<crate::target::FaultSnapshot>::from_bytes(
                &encoded,
                "dag-deep-test",
            )
            .unwrap();
            assert_eq!(restored, checkpoint);
            assert!(Arc::ptr_eq(
                &restored.entries[0].snapshot.history.0,
                &restored.entries[1].snapshot.history.ancestor(0).unwrap().0
            ));
            drop(restored);
            drop(checkpoint);
            drop(history);
            drop(decoded);
        }
    }
}
