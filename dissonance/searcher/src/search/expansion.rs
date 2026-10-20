// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{collections::BTreeMap, fmt, num::NonZeroU8, sync::Arc};

pub const WIDTH: usize = 2;
pub const FANOUT: usize = 2;
pub const DEPTH: u8 = 4;
pub const MAX_EDGES: usize = 14;
pub const MAX_LIVE_SNAPSHOTS: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    pub snapshot_bytes: usize,
    pub evidence_bytes: usize,
    pub path_bytes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Runnable,
    Terminal,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error<E> {
    Cancelled,
    Adapter(E),
    InvalidReply(&'static str),
    CostOverflow,
    ChargeOverflow,
    BudgetExceeded(&'static str),
}

impl<E: fmt::Debug> fmt::Display for Error<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl<E: fmt::Debug> std::error::Error for Error<E> {}

#[derive(Debug)]
pub struct Root<I, A, F> {
    pub parent_id: I,
    pub facts: Arc<F>,
    pub cost_in_group: u64,
    pub last_action: Option<A>,
    pub task_seed: u64,
    pub edge_budget: NonZeroU8,
}

pub struct Parent<'a, I, A, F> {
    pub ordinal: u32,
    pub depth: u8,
    pub root: &'a Root<I, A, F>,
    pub suffix: &'a [A],
    pub facts: &'a F,
    pub path_cost: u64,
    pub cost_in_group: u64,
}

#[derive(Debug)]
pub struct Request<A, S, F> {
    pub ordinal: u32,
    pub parent_ordinal: u32,
    pub depth: u8,
    pub branch: u8,
    pub entropy: u64,
    pub action: A,
    pub suffix: Vec<A>,
    pub parent_snapshot: Arc<S>,
    pub parent_facts: Arc<F>,
}

#[derive(Debug)]
pub struct Completion<A, S, F, E> {
    pub ordinal: u32,
    pub recorded_action: A,
    pub status: Status,
    pub objective: bool,
    pub facts: Option<F>,
    pub edge_cost: u64,
    pub evidence: E,
    pub snapshot: Option<Arc<S>>,
}

pub type RootOf<T> = Root<<T as Adapter>::RootId, <T as Adapter>::Action, <T as Adapter>::Facts>;
pub type ParentOf<'a, T> =
    Parent<'a, <T as Adapter>::RootId, <T as Adapter>::Action, <T as Adapter>::Facts>;
pub type RequestOf<T> =
    Request<<T as Adapter>::Action, <T as Adapter>::Snapshot, <T as Adapter>::Facts>;
pub type CompletionOf<T> = Completion<
    <T as Adapter>::Action,
    <T as Adapter>::Snapshot,
    <T as Adapter>::Facts,
    <T as Adapter>::Evidence,
>;

pub trait Adapter: Sized {
    type Error;
    type RootId: Copy;
    type Action: Ord + Clone;
    type Snapshot;
    type Facts;
    type Progress: Eq;
    type Quality: Ord;
    type Bucket: Eq;
    type Evidence;

    fn progress(facts: &Self::Facts) -> &Self::Progress;
    fn quality(facts: &Self::Facts) -> &Self::Quality;
    fn bucket(facts: &Self::Facts) -> &Self::Bucket;
    fn actions(
        &mut self,
        parent: ParentOf<'_, Self>,
        entropy: [u64; FANOUT],
    ) -> Result<[Option<Self::Action>; FANOUT], Self::Error>;
    fn execute(
        &mut self,
        requests: &[RequestOf<Self>],
    ) -> Result<Vec<CompletionOf<Self>>, Self::Error>;
    fn validate_recorded_action(
        &self,
        requested: &Self::Action,
        recorded: &Self::Action,
        status: Status,
    ) -> bool;
    fn snapshot_bytes(&self, snapshot: &Self::Snapshot) -> Result<usize, Self::Error>;
    fn evidence_bytes(&self, evidence: &Self::Evidence) -> Result<usize, Self::Error>;
    fn facts_bytes(&self, facts: &Self::Facts) -> Result<usize, Self::Error>;
    fn action_heap_bytes(&self, action: &Self::Action) -> Result<usize, Self::Error>;
    fn cancelled(&self) -> bool {
        false
    }
}

#[derive(Debug)]
pub struct Node<A, S, F, E> {
    pub ordinal: u32,
    pub parent_ordinal: Option<u32>,
    pub depth: u8,
    pub requested_action: Option<A>,
    pub suffix: Vec<A>,
    pub facts: Option<Arc<F>>,
    pub path_cost: u64,
    pub cost_in_group: Option<u64>,
    pub edge_cost: u64,
    pub status: Status,
    pub objective: bool,
    pub evidence: Option<E>,
    pub snapshot: Option<Arc<S>>,
    pub charged_snapshot_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Duplicate<A> {
    pub ordinal: u32,
    pub existing_ordinal: u32,
    pub suffix: Vec<A>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Accounting {
    pub proposed_edges: usize,
    pub task_unique_edge_attempts: usize,
    pub duplicates: usize,
    pub peak_live_snapshots: usize,
    pub peak_snapshot_bytes: usize,
    pub retained_evidence_bytes: usize,
    pub peak_action_path_bytes: usize,
}

#[derive(Debug)]
pub struct Trace<I, A, S, F, E> {
    pub root: Root<I, A, F>,
    pub nodes: Vec<Node<A, S, F, E>>,
    pub duplicates: Vec<Duplicate<A>>,
    pub frontier: Vec<u32>,
    pub objectives: Vec<u32>,
    pub failures: Vec<u32>,
    pub accounting: Accounting,
}

pub type TraceOf<T> = Trace<
    <T as Adapter>::RootId,
    <T as Adapter>::Action,
    <T as Adapter>::Snapshot,
    <T as Adapter>::Facts,
    <T as Adapter>::Evidence,
>;

pub fn branch_entropy(task_seed: u64, parent_ordinal: u32, depth: u8, branch: u8) -> u64 {
    let mut value =
        task_seed ^ (u64::from(parent_ordinal) << 16) ^ (u64::from(depth) << 8) ^ u64::from(branch);
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

pub fn root_relative_cost<P: Eq>(
    root_progress: &P,
    root_cost: u64,
    endpoint_progress: &P,
    path_cost: u64,
) -> u64 {
    if root_progress == endpoint_progress {
        root_cost.saturating_add(path_cost)
    } else {
        path_cost
    }
}

fn add<E>(a: usize, b: usize) -> Result<usize, Error<E>> {
    a.checked_add(b).ok_or(Error::ChargeOverflow)
}

fn check_cancelled<T: Adapter>(adapter: &T) -> Result<(), Error<T::Error>> {
    if adapter.cancelled() {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

pub fn run<T: Adapter>(
    adapter: &mut T,
    root: RootOf<T>,
    snapshot: Arc<T::Snapshot>,
    budget: Budget,
) -> Result<TraceOf<T>, Error<T::Error>> {
    check_cancelled(adapter)?;
    if usize::from(root.edge_budget.get()) > MAX_EDGES {
        return Err(Error::InvalidReply("edge budget exceeds policy bound"));
    }
    let root_bytes = adapter.snapshot_bytes(&snapshot).map_err(Error::Adapter)?;
    if root_bytes > budget.snapshot_bytes {
        return Err(Error::BudgetExceeded("root snapshot"));
    }
    let root_facts_bytes = adapter.facts_bytes(&root.facts).map_err(Error::Adapter)?;
    if root_facts_bytes > budget.evidence_bytes {
        return Err(Error::BudgetExceeded("root facts"));
    }
    let mut trace = Trace {
        nodes: vec![Node {
            ordinal: 0,
            parent_ordinal: None,
            depth: 0,
            requested_action: None,
            suffix: Vec::new(),
            facts: Some(Arc::clone(&root.facts)),
            path_cost: 0,
            cost_in_group: Some(root.cost_in_group),
            edge_cost: 0,
            status: Status::Runnable,
            objective: false,
            evidence: None,
            snapshot: Some(snapshot),
            charged_snapshot_bytes: root_bytes,
        }],
        root,
        duplicates: Vec::new(),
        frontier: vec![0],
        objectives: Vec::new(),
        failures: Vec::new(),
        accounting: Accounting {
            peak_live_snapshots: 1,
            peak_snapshot_bytes: root_bytes,
            retained_evidence_bytes: root_facts_bytes,
            ..Accounting::default()
        },
    };
    let mut paths = BTreeMap::from([(Vec::new(), 0_u32)]);
    account_paths(adapter, &mut trace, &paths, &[], &[], budget.path_bytes)?;
    for depth in 1..=DEPTH {
        check_cancelled(adapter)?;
        let mut requests = Vec::with_capacity(WIDTH * FANOUT);
        let mut parents = trace.frontier.clone();
        parents.sort_unstable();
        for ordinal in parents {
            if trace.accounting.task_unique_edge_attempts + requests.len()
                >= usize::from(trace.root.edge_budget.get())
            {
                break;
            }
            let node = trace
                .nodes
                .iter()
                .find(|node| node.ordinal == ordinal)
                .ok_or(Error::InvalidReply("missing parent"))?;
            let entropy = std::array::from_fn(|branch| {
                branch_entropy(trace.root.task_seed, ordinal, depth, branch as u8)
            });
            let actions = adapter
                .actions(
                    Parent {
                        ordinal,
                        depth: node.depth,
                        root: &trace.root,
                        suffix: &node.suffix,
                        facts: node
                            .facts
                            .as_ref()
                            .ok_or(Error::InvalidReply("missing facts"))?,
                        path_cost: node.path_cost,
                        cost_in_group: node
                            .cost_in_group
                            .ok_or(Error::InvalidReply("missing cost"))?,
                    },
                    entropy,
                )
                .map_err(Error::Adapter)?;
            for (branch, action) in actions.into_iter().enumerate() {
                if trace.accounting.task_unique_edge_attempts + requests.len()
                    >= usize::from(trace.root.edge_budget.get())
                {
                    break;
                }
                let Some(action) = action else { continue };
                trace.accounting.proposed_edges += 1;
                let child_ordinal = ordinal * FANOUT as u32 + 1 + branch as u32;
                let mut suffix = node.suffix.clone();
                suffix.push(action.clone());
                if let Some(existing) = paths.get(&suffix) {
                    trace.duplicates.push(Duplicate {
                        ordinal: child_ordinal,
                        existing_ordinal: *existing,
                        suffix,
                    });
                    trace.accounting.duplicates += 1;
                    continue;
                }
                paths.insert(suffix.clone(), child_ordinal);
                requests.push(Request {
                    ordinal: child_ordinal,
                    parent_ordinal: ordinal,
                    depth,
                    branch: branch as u8,
                    entropy: entropy[branch],
                    action,
                    suffix,
                    parent_snapshot: Arc::clone(
                        node.snapshot
                            .as_ref()
                            .ok_or(Error::InvalidReply("missing snapshot"))?,
                    ),
                    parent_facts: Arc::clone(
                        node.facts
                            .as_ref()
                            .ok_or(Error::InvalidReply("missing facts"))?,
                    ),
                });
            }
        }
        account_paths(
            adapter,
            &mut trace,
            &paths,
            &requests,
            &[],
            budget.path_bytes,
        )?;
        if requests.is_empty() {
            break;
        }
        check_cancelled(adapter)?;
        let completions = adapter.execute(&requests).map_err(Error::Adapter)?;
        check_cancelled(adapter)?;
        if completions.len() != requests.len() {
            return Err(Error::InvalidReply("completion count"));
        }
        account_paths(
            adapter,
            &mut trace,
            &paths,
            &requests,
            &completions,
            budget.path_bytes,
        )?;
        let mut ordered = BTreeMap::new();
        let mut live_bytes = trace
            .nodes
            .iter()
            .filter(|node| node.snapshot.is_some())
            .try_fold(0, |sum, node| add(sum, node.charged_snapshot_bytes))?;
        let mut live_count = trace
            .nodes
            .iter()
            .filter(|node| node.snapshot.is_some())
            .count();
        for completion in completions {
            let request = requests
                .iter()
                .find(|request| request.ordinal == completion.ordinal)
                .ok_or(Error::InvalidReply("unknown completion"))?;
            if completion.status == Status::Runnable
                && (completion.snapshot.is_none()
                    || completion.facts.is_none()
                    || completion.recorded_action != request.action)
                || completion.status != Status::Runnable && completion.snapshot.is_some()
                || completion.status == Status::Failed
                    && (completion.facts.is_some() || completion.objective)
                || !adapter.validate_recorded_action(
                    &request.action,
                    &completion.recorded_action,
                    completion.status,
                )
            {
                return Err(Error::InvalidReply("status, action or snapshot"));
            }
            if let Some(snapshot) = &completion.snapshot {
                live_count += 1;
                live_bytes = add(
                    live_bytes,
                    adapter.snapshot_bytes(snapshot).map_err(Error::Adapter)?,
                )?;
            }
            let charge = adapter
                .evidence_bytes(&completion.evidence)
                .map_err(Error::Adapter)?;
            let facts_charge = completion
                .facts
                .as_ref()
                .map(|facts| adapter.facts_bytes(facts))
                .transpose()
                .map_err(Error::Adapter)?
                .unwrap_or(0);
            trace.accounting.retained_evidence_bytes = add(
                trace.accounting.retained_evidence_bytes,
                add(charge, facts_charge)?,
            )?;
            if ordered.insert(completion.ordinal, completion).is_some() {
                return Err(Error::InvalidReply("duplicate completion"));
            }
        }
        if live_count > MAX_LIVE_SNAPSHOTS || live_bytes > budget.snapshot_bytes {
            return Err(Error::BudgetExceeded("live snapshots"));
        }
        if trace.accounting.retained_evidence_bytes > budget.evidence_bytes {
            return Err(Error::BudgetExceeded("evidence and facts"));
        }
        trace.accounting.peak_live_snapshots = trace.accounting.peak_live_snapshots.max(live_count);
        trace.accounting.peak_snapshot_bytes = trace.accounting.peak_snapshot_bytes.max(live_bytes);
        let mut children = Vec::with_capacity(WIDTH * FANOUT);
        for request in &requests {
            check_cancelled(adapter)?;
            let completion = ordered
                .remove(&request.ordinal)
                .ok_or(Error::InvalidReply("missing completion"))?;
            let parent = trace
                .nodes
                .iter()
                .find(|node| node.ordinal == request.parent_ordinal)
                .ok_or(Error::InvalidReply("missing completion parent"))?;
            let path_cost = parent
                .path_cost
                .checked_add(completion.edge_cost)
                .ok_or(Error::CostOverflow)?;
            let cost = completion.facts.as_ref().map(|facts| {
                root_relative_cost(
                    T::progress(&trace.root.facts),
                    trace.root.cost_in_group,
                    T::progress(facts),
                    path_cost,
                )
            });
            let mut suffix = request.suffix.clone();
            *suffix
                .last_mut()
                .ok_or(Error::InvalidReply("empty child suffix"))? = completion.recorded_action;
            let snapshot_bytes = completion
                .snapshot
                .as_ref()
                .map(|snapshot| adapter.snapshot_bytes(snapshot))
                .transpose()
                .map_err(Error::Adapter)?
                .unwrap_or(0);
            if completion.objective {
                trace.objectives.push(request.ordinal);
            }
            if completion.status == Status::Failed {
                trace.failures.push(request.ordinal);
            }
            if completion.status == Status::Runnable && !completion.objective {
                children.push(trace.nodes.len());
            }
            trace.accounting.task_unique_edge_attempts += 1;
            trace.nodes.push(Node {
                ordinal: request.ordinal,
                parent_ordinal: Some(request.parent_ordinal),
                depth,
                requested_action: Some(request.action.clone()),
                suffix,
                facts: completion.facts.map(Arc::new),
                path_cost,
                cost_in_group: cost,
                edge_cost: completion.edge_cost,
                status: completion.status,
                objective: completion.objective,
                evidence: Some(completion.evidence),
                snapshot: completion.snapshot,
                charged_snapshot_bytes: snapshot_bytes,
            });
        }
        children.sort_by(|a, b| {
            let a = &trace.nodes[*a];
            let b = &trace.nodes[*b];
            T::quality(b.facts.as_ref().expect("validated facts"))
                .cmp(T::quality(a.facts.as_ref().expect("validated facts")))
                .then_with(|| a.cost_in_group.cmp(&b.cost_in_group))
                .then_with(|| a.ordinal.cmp(&b.ordinal))
        });
        let second = children.first().and_then(|first| {
            let first = &trace.nodes[*first];
            children
                .iter()
                .skip(1)
                .copied()
                .find(|index| {
                    T::bucket(trace.nodes[*index].facts.as_ref().expect("validated facts"))
                        != T::bucket(first.facts.as_ref().expect("validated facts"))
                })
                .or_else(|| {
                    children
                        .iter()
                        .skip(1)
                        .copied()
                        .find(|index| trace.nodes[*index].suffix != first.suffix)
                })
        });
        trace.frontier = children
            .first()
            .map(|index| trace.nodes[*index].ordinal)
            .into_iter()
            .chain(second.map(|index| trace.nodes[index].ordinal))
            .collect();
        account_paths(
            adapter,
            &mut trace,
            &paths,
            &requests,
            &[],
            budget.path_bytes,
        )?;
        drop(requests);
        for node in &mut trace.nodes {
            if !trace.frontier.contains(&node.ordinal) {
                node.snapshot = None;
            }
        }
        account_paths(adapter, &mut trace, &paths, &[], &[], budget.path_bytes)?;
        if trace.frontier.is_empty() {
            break;
        }
    }
    if trace.frontier == [0] {
        trace.frontier.clear();
        trace.nodes[0].snapshot = None;
    }
    Ok(trace)
}

fn action_storage<T: Adapter>(
    adapter: &T,
    actions: &[T::Action],
    capacity: usize,
) -> Result<usize, Error<T::Error>> {
    let mut bytes = capacity
        .checked_mul(std::mem::size_of::<T::Action>())
        .ok_or(Error::ChargeOverflow)?;
    for action in actions {
        bytes = add(
            bytes,
            adapter.action_heap_bytes(action).map_err(Error::Adapter)?,
        )?;
    }
    Ok(bytes)
}

fn account_paths<T: Adapter>(
    adapter: &T,
    trace: &mut TraceOf<T>,
    paths: &BTreeMap<Vec<T::Action>, u32>,
    requests: &[RequestOf<T>],
    completions: &[CompletionOf<T>],
    budget: usize,
) -> Result<(), Error<T::Error>> {
    let mut bytes = 0;
    if let Some(action) = &trace.root.last_action {
        bytes = action_storage(adapter, std::slice::from_ref(action), 1)?;
    }
    for node in &trace.nodes {
        bytes = add(
            bytes,
            action_storage(adapter, &node.suffix, node.suffix.capacity())?,
        )?;
        if let Some(action) = &node.requested_action {
            bytes = add(
                bytes,
                action_storage(adapter, std::slice::from_ref(action), 1)?,
            )?;
        }
    }
    for duplicate in &trace.duplicates {
        bytes = add(
            bytes,
            action_storage(adapter, &duplicate.suffix, duplicate.suffix.capacity())?,
        )?;
    }
    for suffix in paths.keys() {
        bytes = add(bytes, action_storage(adapter, suffix, suffix.capacity())?)?;
    }
    for request in requests {
        bytes = add(
            bytes,
            action_storage(adapter, &request.suffix, request.suffix.capacity())?,
        )?;
        bytes = add(
            bytes,
            action_storage(adapter, std::slice::from_ref(&request.action), 1)?,
        )?;
    }
    for completion in completions {
        bytes = add(
            bytes,
            action_storage(
                adapter,
                std::slice::from_ref(&completion.recorded_action),
                1,
            )?,
        )?;
    }
    trace.accounting.peak_action_path_bytes = trace.accounting.peak_action_path_bytes.max(bytes);
    if bytes > budget {
        return Err(Error::BudgetExceeded("action paths"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    type TestError = Box<dyn std::error::Error>;
    #[derive(Debug)]
    struct Sentinel(&'static str);
    impl std::fmt::Display for Sentinel {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.0)
        }
    }
    impl std::error::Error for Sentinel {}

    #[derive(Debug, PartialEq, Eq)]
    struct Facts {
        progress: u8,
        quality: u32,
        bucket: u8,
        milestones: u64,
    }

    #[derive(Debug, Default)]
    struct Owners {
        live: AtomicUsize,
        peak: AtomicUsize,
        evidence: AtomicUsize,
    }

    #[derive(Debug)]
    struct Snapshot(Arc<Owners>);
    impl Snapshot {
        fn new(owners: &Arc<Owners>) -> Arc<Self> {
            let count = owners.live.fetch_add(1, Ordering::SeqCst) + 1;
            owners.peak.fetch_max(count, Ordering::SeqCst);
            Arc::new(Self(Arc::clone(owners)))
        }
    }
    impl Drop for Snapshot {
        fn drop(&mut self) {
            self.0.live.fetch_sub(1, Ordering::SeqCst);
        }
    }
    #[derive(Debug)]
    struct Evidence {
        ordinal: u32,
        owners: Arc<Owners>,
    }
    impl Drop for Evidence {
        fn drop(&mut self) {
            self.owners.evidence.fetch_sub(1, Ordering::SeqCst);
        }
    }
    #[derive(Default)]
    struct Toy {
        owners: Arc<Owners>,
        reverse: bool,
        policy: u8,
        malformed: u8,
        cancelled: bool,
        oversized: bool,
        calls: Vec<(u32, Vec<u8>, u64)>,
        request_path_bytes: usize,
    }
    impl Toy {
        fn facts(ordinal: u32, depth: u8, milestones: u64, policy: u8) -> Facts {
            let quality = if policy == 9 {
                7
            } else if policy == 1 && depth == 2 {
                match ordinal {
                    3 => 38,
                    4 => 44,
                    5 => 143,
                    _ => 38,
                }
            } else {
                ordinal
            };
            let bucket = if policy == 2 {
                u8::from(ordinal == 3)
            } else if policy == 3 || policy == 9 {
                0
            } else {
                (ordinal % 3) as u8
            };
            Facts {
                progress: depth % 2,
                quality,
                bucket,
                milestones,
            }
        }
        fn root(&self) -> RootOf<Self> {
            Root {
                parent_id: 91,
                facts: Arc::new(Facts {
                    progress: 0,
                    quality: 0,
                    bucket: 0,
                    milestones: 7,
                }),
                cost_in_group: 100,
                last_action: Some(9),
                task_seed: 19,
                edge_budget: NonZeroU8::new(14).unwrap(),
            }
        }
        fn run(&mut self) -> Result<TraceOf<Self>, Error<TestError>> {
            run(self, self.root(), Snapshot::new(&self.owners), generous())
        }
    }
    impl Adapter for Toy {
        type Error = TestError;
        type RootId = usize;
        type Action = u8;
        type Snapshot = Snapshot;
        type Facts = Facts;
        type Progress = u8;
        type Quality = u32;
        type Bucket = u8;
        type Evidence = Evidence;
        fn progress(facts: &Facts) -> &u8 {
            &facts.progress
        }
        fn quality(facts: &Facts) -> &u32 {
            &facts.quality
        }
        fn bucket(facts: &Facts) -> &u8 {
            &facts.bucket
        }
        fn actions(
            &mut self,
            parent: ParentOf<'_, Self>,
            _: [u64; FANOUT],
        ) -> Result<[Option<u8>; FANOUT], TestError> {
            assert_eq!(parent.root.parent_id, 91);
            if self.policy == 10 {
                return Err(Box::new(Sentinel("action sampling")));
            }
            if self.policy == 4 || self.policy == 1 && parent.depth == 2 {
                Ok([None, None])
            } else if self.policy == 5 {
                Ok([Some(1), Some(1)])
            } else {
                Ok([Some(1), Some(2)])
            }
        }
        fn execute(
            &mut self,
            requests: &[RequestOf<Self>],
        ) -> Result<Vec<CompletionOf<Self>>, TestError> {
            self.request_path_bytes = requests
                .iter()
                .map(|request| request.suffix.capacity() + 1)
                .sum();
            let mut replies = Vec::new();
            let mut indices = (0..requests.len()).collect::<Vec<_>>();
            if self.reverse {
                indices.reverse();
            }
            for index in indices {
                let request = &requests[index];
                self.calls
                    .push((request.ordinal, request.suffix.clone(), request.entropy));
                self.owners.evidence.fetch_add(1, Ordering::SeqCst);
                let status = if self.policy == 6 && request.ordinal == 3 {
                    Status::Terminal
                } else if self.policy == 6 && request.ordinal == 4 {
                    Status::Failed
                } else {
                    Status::Runnable
                };
                let objective = self.policy == 6 && request.ordinal == 6;
                replies.push(Completion {
                    ordinal: request.ordinal,
                    recorded_action: if status == Status::Terminal {
                        0
                    } else {
                        request.action
                    },
                    status,
                    objective,
                    facts: (status == Status::Runnable).then(|| {
                        Self::facts(
                            request.ordinal,
                            request.depth,
                            request.parent_facts.milestones + u64::from(request.action),
                            self.policy,
                        )
                    }),
                    edge_cost: if self.policy == 7 {
                        u64::MAX
                    } else {
                        u64::from(request.action)
                    },
                    evidence: Evidence {
                        ordinal: request.ordinal,
                        owners: Arc::clone(&self.owners),
                    },
                    snapshot: (status == Status::Runnable).then(|| Snapshot::new(&self.owners)),
                });
            }
            if self.policy == 11 {
                return Err(Box::new(Sentinel("execution")));
            }
            match self.malformed {
                1 => {
                    replies.pop();
                }
                2 => replies[0].ordinal = 99,
                3 => {
                    if replies.len() > 1 {
                        replies[1].ordinal = replies[0].ordinal
                    }
                }
                4 => replies[0].recorded_action = 99,
                5 => replies[0].snapshot = None,
                6 => {
                    replies[0].status = Status::Terminal;
                    replies[0].snapshot = None;
                    replies[0].recorded_action = 99;
                }
                7 => replies[0].facts = None,
                8 => {
                    replies[0].status = Status::Failed;
                    replies[0].snapshot = None;
                }
                9 => {
                    replies[0].status = Status::Failed;
                    replies[0].snapshot = None;
                    replies[0].facts = None;
                    replies[0].objective = true;
                }
                _ => {}
            }
            if self.policy == 8 {
                self.cancelled = true;
            }
            Ok(replies)
        }
        fn validate_recorded_action(&self, requested: &u8, recorded: &u8, status: Status) -> bool {
            *recorded == *requested || status == Status::Terminal && *recorded < *requested
        }
        fn snapshot_bytes(&self, _: &Snapshot) -> Result<usize, TestError> {
            Ok(1)
        }
        fn evidence_bytes(&self, _: &Evidence) -> Result<usize, TestError> {
            Ok(if self.oversized { usize::MAX } else { 1 })
        }
        fn facts_bytes(&self, _: &Facts) -> Result<usize, TestError> {
            Ok(std::mem::size_of::<Facts>())
        }
        fn action_heap_bytes(&self, _: &u8) -> Result<usize, TestError> {
            Ok(0)
        }
        fn cancelled(&self) -> bool {
            self.cancelled
        }
    }
    fn generous() -> Budget {
        Budget {
            snapshot_bytes: 6,
            evidence_bytes: 10000,
            path_bytes: 10000,
        }
    }

    #[test]
    fn reordered_completions_match_independent_scalar_tree() {
        let mut toy = Toy::default();
        let actual = toy.run().unwrap();
        let mut reverse = Toy {
            reverse: true,
            ..Toy::default()
        };
        let reordered = reverse.run().unwrap();
        let mut parents = vec![(0u32, Vec::<u8>::new())];
        let mut expected = Vec::new();
        for depth in 1..=4 {
            parents.sort_by_key(|parent| parent.0);
            let mut children = Vec::new();
            for (parent, suffix) in parents {
                for branch in 0..2 {
                    let ordinal = parent * 2 + 1 + branch;
                    let mut path = suffix.clone();
                    path.push(branch as u8 + 1);
                    let sum = path.iter().map(|action| u64::from(*action)).sum::<u64>();
                    let cost = if depth % 2 == 0 { 100 + sum } else { sum };
                    expected.push((ordinal, path.clone(), cost));
                    children.push((ordinal, path));
                }
            }
            children.sort_by_key(|node| std::cmp::Reverse(node.0));
            let first = children[0].clone();
            let second = children
                .iter()
                .skip(1)
                .find(|node| node.0 % 3 != first.0 % 3)
                .or_else(|| children.get(1))
                .unwrap()
                .clone();
            parents = vec![first, second];
        }
        let projection = |trace: &TraceOf<Toy>| {
            trace
                .nodes
                .iter()
                .skip(1)
                .map(|node| {
                    (
                        node.ordinal,
                        node.suffix.clone(),
                        node.cost_in_group.unwrap(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(projection(&actual), expected);
        assert_eq!(projection(&reordered), expected);
        assert_eq!(
            actual.frontier,
            parents.iter().map(|node| node.0).collect::<Vec<_>>()
        );
        toy.calls.sort();
        reverse.calls.sort();
        assert_eq!(toy.calls, reverse.calls);
        for node in actual.nodes.iter().skip(1) {
            assert_eq!(node.evidence.as_ref().unwrap().ordinal, node.ordinal);
            assert_eq!(node.facts.as_ref().unwrap().milestones, 7 + node.path_cost);
        }
    }

    #[test]
    fn quality_overrides_legacy_earliest_position_and_diversifies_buckets() {
        let mut toy = Toy {
            policy: 1,
            ..Toy::default()
        };
        let trace = toy.run().unwrap();
        assert_eq!(trace.frontier, [5, 4]);
        let positions = trace
            .frontier
            .iter()
            .map(|ordinal| {
                trace
                    .nodes
                    .iter()
                    .find(|node| node.ordinal == *ordinal)
                    .unwrap()
                    .facts
                    .as_ref()
                    .unwrap()
                    .quality
            })
            .collect::<Vec<_>>();
        assert_eq!(positions, [143, 44]);
        let mut diverse = Toy {
            policy: 2,
            ..Toy::default()
        };
        let mut root = diverse.root();
        root.edge_budget = NonZeroU8::new(6).unwrap();
        let trace = run(&mut diverse, root, Snapshot::new(&toy.owners), generous()).unwrap();
        assert_eq!(trace.frontier, [6, 3]);
        let mut fallback = Toy {
            policy: 3,
            ..Toy::default()
        };
        let trace = fallback.run().unwrap();
        assert_eq!(trace.frontier.len(), 2);
        let nodes = trace
            .frontier
            .iter()
            .map(|id| trace.nodes.iter().find(|node| node.ordinal == *id).unwrap())
            .collect::<Vec<_>>();
        assert_ne!(nodes[0].suffix, nodes[1].suffix);
    }

    #[test]
    fn objectives_and_filtered_failures_keep_witness_suffix_without_expansion() {
        let mut toy = Toy {
            policy: 6,
            ..Toy::default()
        };
        let trace = toy.run().unwrap();
        assert_eq!(trace.objectives, [6]);
        assert_eq!(trace.failures, [4]);
        assert!(!toy.calls.iter().any(|call| call.0 == 13 || call.0 == 14));
        let objective = trace.nodes.iter().find(|node| node.ordinal == 6).unwrap();
        assert_eq!(objective.suffix, [2, 2]);
        assert!(objective.snapshot.is_none());
        assert!(objective.evidence.is_some());
        let terminal = trace.nodes.iter().find(|node| node.ordinal == 3).unwrap();
        assert_eq!(terminal.requested_action, Some(1));
        assert_eq!(terminal.suffix, [1, 0]);
        assert!(terminal.facts.is_none());
        assert_eq!(terminal.cost_in_group, None);
        assert_eq!(terminal.path_cost, 2);
        assert_eq!(trace.root.parent_id, 91);
        assert!(
            trace
                .nodes
                .iter()
                .find(|node| node.ordinal == 4)
                .unwrap()
                .evidence
                .is_some()
        );
    }

    #[test]
    fn empty_root_is_not_an_attempted_proposal() {
        let mut toy = Toy {
            policy: 4,
            ..Toy::default()
        };
        let trace = toy.run().unwrap();
        assert!(trace.frontier.is_empty());
        assert!(toy.calls.is_empty());
        assert_eq!(trace.accounting.task_unique_edge_attempts, 0);
        assert!(trace.nodes[0].snapshot.is_none());
        assert_eq!(toy.owners.live.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn duplicate_requested_paths_are_attempted_once_per_task() {
        let mut toy = Toy {
            policy: 5,
            ..Toy::default()
        };
        let trace = toy.run().unwrap();
        assert_eq!(trace.accounting.task_unique_edge_attempts, 4);
        assert_eq!(trace.accounting.duplicates, 4);
        let paths = toy
            .calls
            .iter()
            .map(|call| call.1.clone())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(paths.len(), toy.calls.len());
        assert_eq!(trace.frontier.len(), 1);
    }

    #[test]
    fn edge_budget_is_canonical_and_never_exceeds_fourteen() {
        for budget in 1..=14 {
            let mut toy = Toy::default();
            let mut root = toy.root();
            root.edge_budget = NonZeroU8::new(budget).unwrap();
            let snapshot = Snapshot::new(&toy.owners);
            let trace = run(&mut toy, root, snapshot, generous()).unwrap();
            assert_eq!(
                trace.accounting.task_unique_edge_attempts,
                usize::from(budget)
            );
            assert!(trace.nodes.iter().all(|node| node.suffix.len() <= 4));
        }
        let mut toy = Toy::default();
        let mut root = toy.root();
        root.edge_budget = NonZeroU8::new(15).unwrap();
        let snapshot = Snapshot::new(&toy.owners);
        assert!(run(&mut toy, root, snapshot, generous()).is_err());
        assert!(toy.calls.is_empty());
    }

    #[test]
    fn invalid_replies_and_cancellation_release_all_local_owners() {
        for malformed in 1..=9 {
            let mut toy = Toy {
                malformed,
                ..Toy::default()
            };
            assert!(matches!(toy.run(), Err(Error::InvalidReply(_))));
            assert_eq!(toy.owners.live.load(Ordering::SeqCst), 0);
            assert_eq!(toy.owners.evidence.load(Ordering::SeqCst), 0);
        }
        for during in [false, true] {
            let mut toy = Toy {
                cancelled: !during,
                policy: if during { 8 } else { 0 },
                ..Toy::default()
            };
            assert!(matches!(toy.run(), Err(Error::Cancelled)));
            assert_eq!(toy.owners.live.load(Ordering::SeqCst), 0);
            assert_eq!(toy.owners.evidence.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn returned_payload_limits_and_overflow_release_oversized_layer() {
        for kind in 0..4 {
            let mut toy = Toy {
                oversized: kind == 3,
                ..Toy::default()
            };
            let root = toy.root();
            let snapshot = Snapshot::new(&toy.owners);
            let mut budget = generous();
            match kind {
                0 => budget.snapshot_bytes = 2,
                1 => budget.evidence_bytes = 40,
                2 => budget.path_bytes = 1,
                _ => {}
            }
            assert!(run(&mut toy, root, snapshot, budget).is_err());
            assert_eq!(toy.owners.live.load(Ordering::SeqCst), 0);
            assert_eq!(toy.owners.evidence.load(Ordering::SeqCst), 0);
        }
        let mut toy = Toy {
            policy: 7,
            ..Toy::default()
        };
        assert!(matches!(toy.run(), Err(Error::CostOverflow)));
        assert_eq!(toy.owners.live.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn snapshot_payload_is_shared_and_local_tree_owns_at_most_six() {
        let mut toy = Toy::default();
        let snapshot = Snapshot::new(&toy.owners);
        let external = Arc::clone(&snapshot);
        let root = toy.root();
        let trace = run(&mut toy, root, snapshot, generous()).unwrap();
        assert_eq!(Arc::strong_count(&external), 1);
        assert_eq!(trace.accounting.peak_live_snapshots, 6);
        assert_eq!(toy.owners.peak.load(Ordering::SeqCst), 7);
        assert_eq!(toy.owners.live.load(Ordering::SeqCst), 3);
        drop(trace);
        assert_eq!(toy.owners.live.load(Ordering::SeqCst), 1);
        drop(external);
        assert_eq!(toy.owners.live.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn leave_and_return_uses_root_relative_cost_with_checked_local_sum() {
        let trace = Toy::default().run().unwrap();
        let node = trace.nodes.iter().find(|node| node.depth == 2).unwrap();
        assert_eq!(node.cost_in_group, Some(100 + node.path_cost));
        assert_eq!(root_relative_cost(&0, u64::MAX, &0, 9), u64::MAX);
        assert_eq!(root_relative_cost(&0, u64::MAX, &1, 9), 9);
    }

    #[derive(
        Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
    )]
    struct Key {
        progress: u8,
        place: u32,
    }
    impl crate::search::archive::ArchiveKey for Key {
        type Progress = u8;
        type Place = u32;
        type Identity = ();
        type Lineage = ();
        fn progress(self) -> u8 {
            self.progress
        }
        fn place(self) -> u32 {
            self.place
        }
        fn identity(self) {}
        fn complete(self, _: Option<(Self, &())>) -> Self {
            self
        }
        fn record(_: &mut (), _: Self) {}
    }

    #[test]
    fn every_endpoint_cost_matches_independent_archive_admission_at_same_root() {
        use crate::search::archive::{Archive, ArchiveCandidate};
        let mut archive = Archive::<u8, Key, (), ()>::new(|action| u64::from(*action));
        let root = archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    suffix: vec![7],
                    key: Key {
                        progress: 0,
                        place: 0,
                    },
                    milestones: (),
                },
                (),
            )
            .unwrap()
            .unwrap();
        assert_eq!(archive.entry_cost_in_group(root), 7);
        let mut toy = Toy::default();
        let mut context = toy.root();
        context.cost_in_group = 7;
        let snapshot = Snapshot::new(&toy.owners);
        let trace = run(&mut toy, context, snapshot, generous()).unwrap();
        for node in trace.nodes.iter().skip(1) {
            let facts = node.facts.as_ref().unwrap();
            let (retained, _) = archive
                .insert_after(
                    Some(root),
                    None,
                    u64::from(node.ordinal),
                    ArchiveCandidate {
                        suffix: node.suffix.clone(),
                        key: Key {
                            progress: facts.progress,
                            place: node.ordinal,
                        },
                        milestones: (),
                    },
                    (),
                )
                .unwrap();
            assert_eq!(
                Some(archive.entry_cost_in_group(retained.unwrap())),
                node.cost_in_group
            );
        }
    }

    #[test]
    fn equal_quality_and_bucket_use_cost_then_canonical_ordinal() {
        for reverse in [false, true] {
            let mut toy = Toy {
                policy: 9,
                reverse,
                ..Toy::default()
            };
            let mut root = toy.root();
            root.edge_budget = NonZeroU8::new(6).unwrap();
            let snapshot = Snapshot::new(&toy.owners);
            let trace = run(&mut toy, root, snapshot, generous()).unwrap();
            assert_eq!(trace.frontier, [3, 4]);
            assert_eq!(
                trace
                    .nodes
                    .iter()
                    .find(|node| node.ordinal == 4)
                    .unwrap()
                    .cost_in_group,
                trace
                    .nodes
                    .iter()
                    .find(|node| node.ordinal == 5)
                    .unwrap()
                    .cost_in_group
            );
        }
    }

    #[test]
    fn adapter_errors_release_both_planned_and_returned_payloads() {
        for policy in [10, 11] {
            let mut toy = Toy {
                policy,
                ..Toy::default()
            };
            let error = toy.run().unwrap_err();
            let Error::Adapter(error) = error else {
                panic!("expected exact adapter error")
            };
            assert_eq!(
                error.downcast_ref::<Sentinel>().unwrap().0,
                if policy == 10 {
                    "action sampling"
                } else {
                    "execution"
                }
            );
            assert_eq!(toy.owners.live.load(Ordering::SeqCst), 0);
            assert_eq!(toy.owners.evidence.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn path_charge_includes_request_and_retained_result_overlap() {
        let mut toy = Toy::default();
        let mut root = toy.root();
        root.edge_budget = NonZeroU8::new(2).unwrap();
        let snapshot = Snapshot::new(&toy.owners);
        let trace = run(&mut toy, root, snapshot, generous()).unwrap();
        let retained = 1 + trace
            .nodes
            .iter()
            .map(|node| node.suffix.capacity() + usize::from(node.requested_action.is_some()))
            .sum::<usize>();
        let map_paths = toy
            .calls
            .iter()
            .map(|call| call.1.capacity())
            .sum::<usize>();
        let expected = retained + map_paths + toy.request_path_bytes;
        assert_eq!(trace.accounting.peak_action_path_bytes, expected);
        let mut budget = generous();
        budget.path_bytes = expected - 1;
        drop(trace);
        let mut root = toy.root();
        root.edge_budget = NonZeroU8::new(2).unwrap();
        let snapshot = Snapshot::new(&toy.owners);
        assert!(matches!(
            run(&mut toy, root, snapshot, budget),
            Err(Error::BudgetExceeded("action paths"))
        ));
        assert_eq!(toy.owners.live.load(Ordering::SeqCst), 0);
        assert_eq!(toy.owners.evidence.load(Ordering::SeqCst), 0);
    }
}
