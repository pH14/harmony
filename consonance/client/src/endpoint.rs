// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use control_proto::{EventHistory, Resolution, SnapId, StopConditions, StopReason};
use environment::input_spec::InputSpec;
use sha2::{Digest, Sha256};

pub type EndpointError = Box<dyn Error>;
pub type EndpointEvent = (u64, u32, Vec<u8>);
pub const ENDPOINT_FRAME_LIMIT: usize = 2 * control_proto::MAX_FRAME_LEN + 4096;
pub const ENDPOINT_PAGE_LIMIT: usize = control_proto::MAX_FRAME_LEN;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EndpointToken(pub u64);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EndpointStops(u8);

impl EndpointStops {
    pub const NONE: Self = Self(0);
    pub const DEADLINE: Self = Self(1);
    pub const ALL: Self = Self(31);

    pub fn from_bits(bits: u8) -> Result<Self, EndpointError> {
        if bits & !Self::ALL.0 != 0 {
            return Err(EndpointProtocolError::Invalid("unknown snapshot stop mask").into());
        }
        Ok(Self(bits))
    }

    pub fn bits(self) -> u8 {
        self.0
    }

    pub fn contains(self, stop: &StopReason) -> bool {
        let bit = match stop {
            StopReason::Deadline { .. } => 1,
            StopReason::Quiescent { .. } => 2,
            StopReason::Crash { .. } => 4,
            StopReason::SnapshotPoint { .. } => 8,
            StopReason::Assertion { .. } => 16,
            StopReason::Decision { .. } => 0,
        };
        self.0 & bit != 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EndpointProofRequest {
    WholeStateAndHistory,
    Omit,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EndpointProof {
    WholeStateAndHistory {
        state_hash: [u8; 32],
        history: EventHistory,
    },
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EndpointBranch {
    pub parent: SnapId,
    pub input: Vec<u8>,
}

impl EndpointBranch {
    pub fn new(parent: SnapId, spec: &InputSpec) -> Result<Self, EndpointError> {
        Ok(Self {
            parent,
            input: spec.try_encode_reproducer()?,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionRequest {
    pub branch: Option<EndpointBranch>,
    pub until: StopConditions,
    pub resolve: Option<Resolution>,
    pub sdk_cursor: u32,
    pub snapshot_on: EndpointStops,
    pub proof: EndpointProofRequest,
}

impl ExecutionRequest {
    pub fn digest(&self) -> Result<[u8; 32], EndpointError> {
        if self.branch.is_some() && self.resolve.is_some() {
            return Err(EndpointProtocolError::Invalid(
                "branch and resolution are mutually exclusive",
            )
            .into());
        }
        EndpointStops::from_bits(self.snapshot_on.bits())?;
        let mut hash = Sha256::new();
        hash.update(b"consonance.execution-endpoint.request.v1\0");
        match &self.branch {
            Some(branch) => {
                if branch.input.len() > InputSpec::MAX_REPRODUCER_BYTES {
                    return Err(EndpointProtocolError::Invalid("branch input exceeds limit").into());
                }
                hash.update([1]);
                hash.update(branch.parent.0.to_le_bytes());
                hash.update((branch.input.len() as u64).to_le_bytes());
                hash.update(&branch.input);
            }
            None => hash.update([0]),
        }
        match self.until.deadline {
            Some(deadline) => {
                hash.update([1]);
                hash.update(deadline.0.to_le_bytes());
            }
            None => hash.update([0]),
        }
        hash.update(self.until.on.0.to_le_bytes());
        match &self.resolve {
            Some(resolve) => {
                if resolve.answer.0.len() > hypercall_proto::MAX_PAYLOAD {
                    return Err(EndpointProtocolError::Invalid("resolution exceeds limit").into());
                }
                hash.update([1]);
                hash.update(resolve.vtime.0.to_le_bytes());
                hash.update(resolve.service.to_le_bytes());
                hash.update(resolve.id.0.to_le_bytes());
                hash.update((resolve.answer.0.len() as u64).to_le_bytes());
                hash.update(&resolve.answer.0);
            }
            None => hash.update([0]),
        }
        hash.update(self.sdk_cursor.to_le_bytes());
        hash.update([
            self.snapshot_on.bits(),
            u8::from(self.proof == EndpointProofRequest::WholeStateAndHistory),
        ]);
        Ok(hash.finalize().into())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EndpointBinding {
    pub token: EndpointToken,
    pub request_digest: [u8; 32],
    pub stop: StopReason,
    pub source_count: u32,
    pub start_cursor: u32,
    pub end_cursor: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EndpointPage {
    pub binding: EndpointBinding,
    pub cursor: u32,
    pub next_cursor: u32,
    pub events: Vec<EndpointEvent>,
}

impl EndpointPage {
    pub fn validate(&self) -> Result<(), EndpointError> {
        let next = self
            .cursor
            .checked_add(u32::try_from(self.events.len())?)
            .ok_or(EndpointProtocolError::Invalid("SDK cursor overflow"))?;
        if self.binding.token.0 == 0
            || self.binding.start_cursor > self.binding.source_count
            || self.binding.source_count > self.binding.end_cursor
            || self.cursor < self.binding.start_cursor
            || next != self.next_cursor
            || next > self.binding.end_cursor
            || (self.events.is_empty() && self.cursor < self.binding.end_cursor)
            || matches!(self.binding.stop, StopReason::Decision { .. })
        {
            return Err(EndpointProtocolError::Invalid("endpoint page binding differs").into());
        }
        let bytes = self
            .events
            .iter()
            .try_fold(0usize, |bytes, event| {
                bytes
                    .checked_add(24)
                    .and_then(|bytes| bytes.checked_add(event.2.len()))
            })
            .ok_or(EndpointProtocolError::Invalid("SDK page length overflow"))?;
        if bytes > ENDPOINT_PAGE_LIMIT {
            return Err(EndpointProtocolError::Invalid("SDK page exceeds limit").into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EndpointReceipt {
    pub binding: EndpointBinding,
    pub snapshot: Option<(SnapId, u64)>,
    pub proof: EndpointProof,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EndpointRequest {
    Execute(ExecutionRequest),
    Page { token: EndpointToken, cursor: u32 },
    Commit { token: EndpointToken },
    Abort { token: EndpointToken },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EndpointReply {
    Decision(StopReason),
    Page(EndpointPage),
    Committed(EndpointReceipt),
    Aborted,
}

#[derive(Debug, thiserror::Error)]
pub enum EndpointProtocolError {
    #[error("execution endpoint: {0}")]
    Invalid(&'static str),
    #[error("execution endpoint is pending")]
    Pending,
    #[error("execution endpoint token or cursor is stale")]
    Stale,
    #[error("execution endpoint pages are incomplete")]
    Incomplete,
    #[error("whole-state and ordered-history endpoint evidence is unavailable")]
    Unavailable,
    #[error("execution endpoint token space is exhausted")]
    Exhausted,
}

pub trait EndpointDriver {
    fn ensure_active(&self) -> Result<(), EndpointError>;
    fn supports_proof(&self) -> bool;
    fn event_count(&self, parent: Option<SnapId>) -> Result<Option<u32>, EndpointError>;
    fn branch(&mut self, parent: SnapId, spec: &InputSpec) -> Result<(), EndpointError>;
    fn run(
        &mut self,
        until: StopConditions,
        resolve: Option<Resolution>,
    ) -> Result<StopReason, EndpointError>;
    fn snapshot(&mut self) -> Result<(SnapId, u64), EndpointError>;
    fn page(&mut self, cursor: u32, end: u32) -> Result<Vec<EndpointEvent>, EndpointError>;
    fn proof(&mut self) -> Result<EndpointProof, EndpointError>;
    fn drop_snapshot(&mut self, snapshot: SnapId) -> Result<(), EndpointError>;
    fn abandon(&mut self);
}

#[derive(Debug)]
struct PendingEndpoint {
    binding: EndpointBinding,
    next_cursor: u32,
    snapshot: Option<(SnapId, u64)>,
    proof: EndpointProofRequest,
}

#[derive(Debug)]
pub struct EndpointState {
    next_token: u64,
    pending: Option<PendingEndpoint>,
}

impl Default for EndpointState {
    fn default() -> Self {
        Self {
            next_token: 1,
            pending: None,
        }
    }
}

impl EndpointState {
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }

    pub fn exchange(
        &mut self,
        driver: &mut dyn EndpointDriver,
        request: EndpointRequest,
    ) -> Result<EndpointReply, EndpointError> {
        driver.ensure_active()?;
        match request {
            EndpointRequest::Execute(request) => self.execute(driver, request),
            EndpointRequest::Page { token, cursor } => {
                let pending = self.pending.as_ref().ok_or(EndpointProtocolError::Stale)?;
                if pending.binding.token != token || pending.next_cursor != cursor {
                    return Err(EndpointProtocolError::Stale.into());
                }
                let binding = pending.binding.clone();
                match Self::read_page(driver, binding, cursor) {
                    Ok(page) => {
                        self.pending
                            .as_mut()
                            .expect("pending endpoint checked")
                            .next_cursor = page.next_cursor;
                        Ok(EndpointReply::Page(page))
                    }
                    Err(error) => {
                        self.pending = None;
                        driver.abandon();
                        Err(error)
                    }
                }
            }
            EndpointRequest::Commit { token } => {
                let pending = self.pending.as_ref().ok_or(EndpointProtocolError::Stale)?;
                if pending.binding.token != token {
                    return Err(EndpointProtocolError::Stale.into());
                }
                if pending.next_cursor != pending.binding.end_cursor {
                    return Err(EndpointProtocolError::Incomplete.into());
                }
                let pending = self.pending.take().expect("pending endpoint checked");
                let proof = match pending.proof {
                    EndpointProofRequest::Omit => Ok(EndpointProof::Unavailable),
                    EndpointProofRequest::WholeStateAndHistory => driver.proof(),
                };
                match proof {
                    Ok(proof) => Ok(EndpointReply::Committed(EndpointReceipt {
                        binding: pending.binding,
                        snapshot: pending.snapshot,
                        proof,
                    })),
                    Err(error) => {
                        driver.abandon();
                        Err(error)
                    }
                }
            }
            EndpointRequest::Abort { token } => {
                let pending = self.pending.as_ref().ok_or(EndpointProtocolError::Stale)?;
                if pending.binding.token != token {
                    return Err(EndpointProtocolError::Stale.into());
                }
                let pending = self.pending.take().expect("pending endpoint checked");
                if let Some((snapshot, _)) = pending.snapshot {
                    if let Err(error) = driver.drop_snapshot(snapshot) {
                        driver.abandon();
                        return Err(error);
                    }
                }
                Ok(EndpointReply::Aborted)
            }
        }
    }

    fn execute(
        &mut self,
        driver: &mut dyn EndpointDriver,
        request: ExecutionRequest,
    ) -> Result<EndpointReply, EndpointError> {
        if self.pending() {
            return Err(EndpointProtocolError::Pending.into());
        }
        let digest = request.digest()?;
        let branch = request
            .branch
            .as_ref()
            .map(|branch| InputSpec::decode_reproducer(&branch.input))
            .transpose()?;
        if request.proof == EndpointProofRequest::WholeStateAndHistory && !driver.supports_proof() {
            return Err(EndpointProtocolError::Unavailable.into());
        }
        let known_source =
            driver.event_count(request.branch.as_ref().map(|branch| branch.parent))?;
        if known_source.is_some_and(|count| request.sdk_cursor > count) {
            return Err(
                EndpointProtocolError::Invalid("SDK cursor exceeds selected source").into(),
            );
        }
        let token = EndpointToken(self.next_token);
        self.next_token = self
            .next_token
            .checked_add(1)
            .ok_or(EndpointProtocolError::Exhausted)?;
        let result = (|| {
            if let (Some(parent), Some(spec)) = (&request.branch, branch) {
                driver.branch(parent.parent, &spec)?;
            }
            let source_count = driver
                .event_count(None)?
                .ok_or(EndpointProtocolError::Invalid(
                    "selected source count is unavailable",
                ))?;
            if request.sdk_cursor > source_count
                || known_source.is_some_and(|count| count != source_count)
            {
                return Err(
                    EndpointProtocolError::Invalid("selected source SDK history differs").into(),
                );
            }
            let stop = driver.run(request.until, request.resolve)?;
            let mut encoded_stop = Vec::new();
            control_proto::encode_reply(
                0,
                &Ok(control_proto::Reply::Stop(stop.clone())),
                &mut encoded_stop,
            )?;
            if matches!(stop, StopReason::Decision { .. }) {
                return Ok(EndpointReply::Decision(stop));
            }
            let snapshot = if request.snapshot_on.contains(&stop) {
                Some(driver.snapshot()?)
            } else {
                None
            };
            let end_cursor = driver
                .event_count(None)?
                .ok_or(EndpointProtocolError::Invalid(
                    "endpoint SDK count is unavailable",
                ))?;
            let binding = EndpointBinding {
                token,
                request_digest: digest,
                stop,
                source_count,
                start_cursor: request.sdk_cursor,
                end_cursor,
            };
            let page = Self::read_page(driver, binding.clone(), request.sdk_cursor)?;
            self.pending = Some(PendingEndpoint {
                binding,
                next_cursor: page.next_cursor,
                snapshot,
                proof: request.proof,
            });
            Ok(EndpointReply::Page(page))
        })();
        if result.is_err() {
            self.pending = None;
            driver.abandon();
        }
        result
    }

    fn read_page(
        driver: &mut dyn EndpointDriver,
        binding: EndpointBinding,
        cursor: u32,
    ) -> Result<EndpointPage, EndpointError> {
        let events = driver.page(cursor, binding.end_cursor)?;
        let next_cursor = cursor
            .checked_add(u32::try_from(events.len())?)
            .ok_or(EndpointProtocolError::Invalid("SDK cursor overflow"))?;
        let page = EndpointPage {
            binding,
            cursor,
            next_cursor,
            events,
        };
        page.validate()?;
        if next_cursor == page.binding.end_cursor
            && cursor != next_cursor
            && !driver.page(next_cursor, next_cursor)?.is_empty()
        {
            return Err(EndpointProtocolError::Invalid("endpoint SDK end differs").into());
        }
        Ok(page)
    }
}

pub trait EndpointTransport {
    fn endpoint_exchange(
        &mut self,
        request: EndpointRequest,
    ) -> Result<EndpointReply, EndpointError>;
    fn endpoint_abandon(&mut self);
}

pub enum ExecutionStart<'a, T: EndpointTransport + ?Sized> {
    Decision(StopReason),
    Endpoint(ExecutionEndpoint<'a, T>),
}

pub struct ExecutionEndpoint<'a, T: EndpointTransport + ?Sized> {
    transport: &'a mut T,
    page: EndpointPage,
    capture: EndpointStops,
    proof: EndpointProofRequest,
    finished: bool,
}

impl<'a, T: EndpointTransport + ?Sized> ExecutionEndpoint<'a, T> {
    pub fn execute(
        transport: &'a mut T,
        request: ExecutionRequest,
    ) -> Result<ExecutionStart<'a, T>, EndpointError> {
        let digest = request.digest()?;
        let start_cursor = request.sdk_cursor;
        let capture = request.snapshot_on;
        let proof = request.proof;
        let result = transport.endpoint_exchange(EndpointRequest::Execute(request));
        match result {
            Ok(EndpointReply::Decision(stop @ StopReason::Decision { .. })) => {
                Ok(ExecutionStart::Decision(stop))
            }
            Ok(EndpointReply::Page(page)) => {
                if let Err(error) = page.validate() {
                    transport.endpoint_abandon();
                    return Err(error);
                }
                if page.binding.request_digest != digest
                    || page.binding.start_cursor != start_cursor
                    || page.cursor != start_cursor
                {
                    transport.endpoint_abandon();
                    return Err(
                        EndpointProtocolError::Invalid("execution reply binding differs").into(),
                    );
                }
                Ok(ExecutionStart::Endpoint(Self {
                    transport,
                    page,
                    capture,
                    proof,
                    finished: false,
                }))
            }
            Ok(_) => {
                transport.endpoint_abandon();
                Err(EndpointProtocolError::Invalid("unexpected execution reply").into())
            }
            Err(error) => Err(error),
        }
    }

    pub fn page(&self) -> &EndpointPage {
        &self.page
    }

    pub fn next_page(&mut self) -> Result<bool, EndpointError> {
        if self.page.next_cursor == self.page.binding.end_cursor {
            return Ok(false);
        }
        let result = self.transport.endpoint_exchange(EndpointRequest::Page {
            token: self.page.binding.token,
            cursor: self.page.next_cursor,
        });
        let checked = (|| {
            let EndpointReply::Page(page) = result? else {
                return Err(EndpointProtocolError::Invalid("unexpected page reply").into());
            };
            page.validate()?;
            if page.binding != self.page.binding || page.cursor != self.page.next_cursor {
                return Err(
                    EndpointProtocolError::Invalid("page belongs to another endpoint").into(),
                );
            }
            Ok(page)
        })();
        match checked {
            Ok(page) => {
                self.page = page;
                Ok(true)
            }
            Err(error) => {
                self.finished = true;
                self.transport.endpoint_abandon();
                Err(error)
            }
        }
    }

    pub fn commit(mut self) -> Result<EndpointReceipt, EndpointError> {
        if self.page.next_cursor != self.page.binding.end_cursor {
            return Err(EndpointProtocolError::Incomplete.into());
        }
        let result = self.transport.endpoint_exchange(EndpointRequest::Commit {
            token: self.page.binding.token,
        });
        self.finished = true;
        let checked = (|| {
            let EndpointReply::Committed(receipt) = result? else {
                return Err(EndpointProtocolError::Invalid("unexpected commit reply").into());
            };
            if receipt.binding != self.page.binding
                || receipt.snapshot.is_some() != self.capture.contains(&receipt.binding.stop)
                || matches!(receipt.proof, EndpointProof::WholeStateAndHistory { .. })
                    != (self.proof == EndpointProofRequest::WholeStateAndHistory)
            {
                return Err(EndpointProtocolError::Invalid("commit binding differs").into());
            }
            Ok(receipt)
        })();
        if checked.is_err() {
            self.transport.endpoint_abandon();
        }
        checked
    }

    pub fn abort(mut self) -> Result<(), EndpointError> {
        let result = self.transport.endpoint_exchange(EndpointRequest::Abort {
            token: self.page.binding.token,
        });
        self.finished = true;
        match result {
            Ok(EndpointReply::Aborted) => Ok(()),
            Ok(_) => {
                self.transport.endpoint_abandon();
                Err(EndpointProtocolError::Invalid("unexpected abort reply").into())
            }
            Err(error) => {
                self.transport.endpoint_abandon();
                Err(error)
            }
        }
    }
}

impl<T: EndpointTransport + ?Sized> Drop for ExecutionEndpoint<'_, T> {
    fn drop(&mut self) {
        if !self.finished {
            if !matches!(
                self.transport.endpoint_exchange(EndpointRequest::Abort {
                    token: self.page.binding.token
                }),
                Ok(EndpointReply::Aborted)
            ) {
                self.transport.endpoint_abandon();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use control_proto::{Answer, DecisionId, Moment, StopMask};
    use std::collections::BTreeMap;

    #[derive(Debug, thiserror::Error)]
    #[error("injected {0}")]
    struct Injected(&'static str);

    struct Fixture {
        endpoint: EndpointState,
        events: Vec<EndpointEvent>,
        parents: BTreeMap<SnapId, Vec<EndpointEvent>>,
        snapshots: BTreeMap<SnapId, u32>,
        next_snapshot: u64,
        next_stop: Option<StopReason>,
        calls: Vec<String>,
        exchanges: usize,
        page_events: usize,
        fail: Option<&'static str>,
        abandoned: bool,
        proof_available: bool,
        parent_count_available: bool,
        corrupt_page: bool,
        lose_commit: bool,
    }

    fn events(count: u32) -> Vec<EndpointEvent> {
        (0..count)
            .map(|n| (u64::from(n), n, vec![n as u8]))
            .collect()
    }

    impl Default for Fixture {
        fn default() -> Self {
            Self {
                endpoint: EndpointState::default(),
                events: events(2),
                parents: [(SnapId(10), events(5)), (SnapId(20), events(1))].into(),
                snapshots: [(SnapId(1), 2)].into(),
                next_snapshot: 100,
                next_stop: None,
                calls: Vec::new(),
                exchanges: 0,
                page_events: 100,
                fail: None,
                abandoned: false,
                proof_available: true,
                parent_count_available: true,
                corrupt_page: false,
                lose_commit: false,
            }
        }
    }

    impl Fixture {
        fn check(&mut self, stage: &'static str) -> Result<(), EndpointError> {
            self.calls.push(stage.to_owned());
            if self.fail == Some(stage) {
                Err(Injected(stage).into())
            } else {
                Ok(())
            }
        }
    }

    impl EndpointDriver for Fixture {
        fn ensure_active(&self) -> Result<(), EndpointError> {
            if self.abandoned {
                Err(Injected("abandoned").into())
            } else {
                Ok(())
            }
        }
        fn supports_proof(&self) -> bool {
            self.proof_available
        }
        fn event_count(&self, parent: Option<SnapId>) -> Result<Option<u32>, EndpointError> {
            match parent {
                Some(_) if !self.parent_count_available => Ok(None),
                Some(parent) => self
                    .parents
                    .get(&parent)
                    .map(|events| Some(events.len() as u32))
                    .ok_or_else(|| Injected("unknown parent").into()),
                None => Ok(Some(self.events.len() as u32)),
            }
        }
        fn branch(&mut self, parent: SnapId, _: &InputSpec) -> Result<(), EndpointError> {
            self.check("branch")?;
            self.events = self
                .parents
                .get(&parent)
                .ok_or(Injected("unknown parent"))?
                .clone();
            Ok(())
        }
        fn run(
            &mut self,
            _: StopConditions,
            resolve: Option<Resolution>,
        ) -> Result<StopReason, EndpointError> {
            self.check(if resolve.is_some() { "resolve" } else { "run" })?;
            for _ in 0..2 {
                let n = self.events.len() as u32;
                self.events.push((u64::from(n), n, vec![n as u8]));
            }
            Ok(self
                .next_stop
                .take()
                .unwrap_or(StopReason::Deadline { vtime: Moment(100) }))
        }
        fn snapshot(&mut self) -> Result<(SnapId, u64), EndpointError> {
            self.check("snapshot-before")?;
            let id = SnapId(self.next_snapshot);
            self.next_snapshot += 1;
            self.snapshots.insert(id, self.events.len() as u32);
            self.check("snapshot-after")?;
            Ok((id, 100))
        }
        fn page(&mut self, cursor: u32, end: u32) -> Result<Vec<EndpointEvent>, EndpointError> {
            self.check("page")?;
            if end != self.events.len() as u32 {
                return Err(Injected("end differs").into());
            }
            Ok(self
                .events
                .iter()
                .skip(cursor as usize)
                .take(self.page_events)
                .cloned()
                .collect())
        }
        fn proof(&mut self) -> Result<EndpointProof, EndpointError> {
            self.check("hash")?;
            self.check("history")?;
            Ok(EndpointProof::WholeStateAndHistory {
                state_hash: [self.events.len() as u8; 32],
                history: EventHistory {
                    count: self.events.len() as u64,
                    digest: [9; 32],
                },
            })
        }
        fn drop_snapshot(&mut self, snapshot: SnapId) -> Result<(), EndpointError> {
            self.check("drop")?;
            self.snapshots
                .remove(&snapshot)
                .ok_or(Injected("unknown temporary"))?;
            Ok(())
        }
        fn abandon(&mut self) {
            self.abandoned = true;
            self.calls.push("abandon".to_owned());
            self.snapshots.clear();
            self.calls.push("clear-snapshots".to_owned());
        }
    }

    impl EndpointTransport for Fixture {
        fn endpoint_exchange(
            &mut self,
            request: EndpointRequest,
        ) -> Result<EndpointReply, EndpointError> {
            self.exchanges += 1;
            let commit = matches!(request, EndpointRequest::Commit { .. });
            let mut state = std::mem::take(&mut self.endpoint);
            let mut result = state.exchange(self, request);
            self.endpoint = state;
            if self.corrupt_page {
                if let Ok(EndpointReply::Page(page)) = &mut result {
                    page.binding.request_digest[0] ^= 1;
                }
            }
            if commit && self.lose_commit {
                return Err(Injected("lost commit reply").into());
            }
            result
        }
        fn endpoint_abandon(&mut self) {
            self.abandon();
            self.endpoint.pending = None;
        }
    }

    fn request() -> ExecutionRequest {
        ExecutionRequest {
            branch: None,
            until: StopConditions {
                deadline: Some(Moment(100)),
                on: StopMask::NONE,
            },
            resolve: None,
            sdk_cursor: 2,
            snapshot_on: EndpointStops::DEADLINE,
            proof: EndpointProofRequest::WholeStateAndHistory,
        }
    }

    fn captured(
        fixture: &mut Fixture,
        request: ExecutionRequest,
    ) -> ExecutionEndpoint<'_, Fixture> {
        match ExecutionEndpoint::execute(fixture, request).unwrap() {
            ExecutionStart::Endpoint(endpoint) => endpoint,
            ExecutionStart::Decision(_) => panic!("expected captured endpoint"),
        }
    }

    #[test]
    fn transaction_matches_legacy_order_and_facts_with_two_common_exchanges() {
        let mut legacy = Fixture::default();
        let mut spec = request();
        spec.branch = Some(EndpointBranch::new(SnapId(10), &InputSpec::seeded(7)).unwrap());
        spec.sdk_cursor = 5;
        legacy.branch(SnapId(10), &InputSpec::seeded(7)).unwrap();
        let stop = legacy.run(spec.until, None).unwrap();
        legacy.snapshot().unwrap();
        let expected_events = legacy.page(5, 7).unwrap();
        assert!(legacy.page(7, 7).unwrap().is_empty());
        let proof = legacy.proof().unwrap();
        let mut batched = Fixture::default();
        let endpoint = captured(&mut batched, spec);
        assert_eq!(endpoint.page().events, expected_events);
        let receipt = endpoint.commit().unwrap();
        assert_eq!(receipt.binding.stop, stop);
        assert_eq!(receipt.proof, proof);
        assert_eq!(receipt.snapshot, Some((SnapId(100), 100)));
        assert_eq!(batched.calls, legacy.calls);
        assert_eq!(batched.exchanges, 2);
        assert!(batched.snapshots.contains_key(&SnapId(100)));
        assert!(!batched.endpoint.pending());
    }

    #[test]
    fn selected_parent_cursor_works_on_both_sides_of_the_current_cut() {
        for (current, parent, cursor) in [(1, SnapId(10), 5), (8, SnapId(20), 1)] {
            for known in [false, true] {
                let mut fixture = Fixture {
                    events: events(current),
                    parent_count_available: known,
                    ..Default::default()
                };
                let mut spec = request();
                spec.branch = Some(EndpointBranch::new(parent, &InputSpec::seeded(7)).unwrap());
                spec.sdk_cursor = cursor;
                let endpoint = captured(&mut fixture, spec);
                assert_eq!(endpoint.page().binding.source_count, cursor);
                assert_eq!(endpoint.page().events.len(), 2);
                endpoint.commit().unwrap();
                assert!(!fixture.abandoned);
            }
        }
    }

    #[test]
    fn unknown_parent_count_checks_cursor_after_selection_and_abandons_failure() {
        let mut fixture = Fixture {
            events: events(8),
            parent_count_available: false,
            ..Default::default()
        };
        let mut spec = request();
        spec.branch = Some(EndpointBranch::new(SnapId(20), &InputSpec::seeded(7)).unwrap());
        spec.sdk_cursor = 4;
        assert!(
            fixture
                .endpoint_exchange(EndpointRequest::Execute(spec))
                .is_err()
        );
        assert_eq!(fixture.calls, ["branch", "abandon", "clear-snapshots"]);
        assert!(fixture.abandoned);
    }

    #[test]
    fn decision_is_exact_handoff_and_resolution_is_an_explicit_next_request() {
        let decision = StopReason::Decision {
            vtime: Moment(9),
            id: DecisionId(23),
            ctx: vec![7, 0, 1, 2],
        };
        let mut fixture = Fixture {
            next_stop: Some(decision.clone()),
            ..Default::default()
        };
        match ExecutionEndpoint::execute(&mut fixture, request()).unwrap() {
            ExecutionStart::Decision(stop) => assert_eq!(stop, decision),
            ExecutionStart::Endpoint(_) => panic!("decision must not capture"),
        }
        assert_eq!(fixture.calls, ["run"]);
        assert_eq!(fixture.snapshots.len(), 1);
        assert!(!fixture.endpoint.pending());
        let mut resume = request();
        resume.resolve = Some(Resolution {
            vtime: Moment(9),
            service: 7,
            id: DecisionId(23),
            answer: Answer(vec![3]),
        });
        resume.sdk_cursor = 4;
        captured(&mut fixture, resume).commit().unwrap();
        assert_eq!(fixture.calls[1], "resolve");
    }

    #[test]
    fn invalid_requests_and_unavailable_proof_refuse_before_mutation() {
        let mut invalid = request();
        invalid.snapshot_on = EndpointStops(128);
        let mut branch_and_resolve = request();
        branch_and_resolve.branch =
            Some(EndpointBranch::new(SnapId(10), &InputSpec::seeded(7)).unwrap());
        branch_and_resolve.resolve = Some(Resolution {
            vtime: Moment(1),
            service: 7,
            id: DecisionId(1),
            answer: Answer(vec![]),
        });
        let mut malformed_branch = request();
        malformed_branch.branch = Some(EndpointBranch {
            parent: SnapId(10),
            input: vec![0],
        });
        let mut ahead = request();
        ahead.sdk_cursor = 3;
        for spec in [invalid, branch_and_resolve, malformed_branch, ahead] {
            let mut fixture = Fixture::default();
            assert!(
                fixture
                    .endpoint_exchange(EndpointRequest::Execute(spec))
                    .is_err()
            );
            assert!(fixture.calls.is_empty());
            assert!(!fixture.abandoned);
            assert_eq!(fixture.snapshots.len(), 1);
        }
        let mut fixture = Fixture {
            proof_available: false,
            ..Default::default()
        };
        assert!(
            fixture
                .endpoint_exchange(EndpointRequest::Execute(request()))
                .is_err()
        );
        assert!(fixture.calls.is_empty());
    }

    #[test]
    fn terminal_capture_policy_and_unavailable_proof_are_explicit() {
        let stop = StopReason::Quiescent { vtime: Moment(100) };
        let mut fixture = Fixture {
            next_stop: Some(stop.clone()),
            proof_available: false,
            ..Default::default()
        };
        let mut spec = request();
        spec.proof = EndpointProofRequest::Omit;
        let receipt = captured(&mut fixture, spec).commit().unwrap();
        assert_eq!(receipt.binding.stop, stop);
        assert_eq!(receipt.snapshot, None);
        assert_eq!(receipt.proof, EndpointProof::Unavailable);
        assert!(
            !fixture
                .calls
                .iter()
                .any(|call| call.starts_with("snapshot") || call == "hash" || call == "history")
        );
    }

    #[test]
    fn bounded_pages_keep_exact_binding_and_commit_rejects_incomplete_reads() {
        let mut fixture = Fixture {
            page_events: 1,
            ..Default::default()
        };
        let mut endpoint = captured(&mut fixture, request());
        let binding = endpoint.page().binding.clone();
        assert_eq!(endpoint.page().events.len(), 1);
        assert!(endpoint.next_page().unwrap());
        assert_eq!(endpoint.page().binding, binding);
        assert_eq!(endpoint.page().cursor, 3);
        assert_eq!(endpoint.page().next_cursor, 4);
        assert!(!endpoint.next_page().unwrap());
        endpoint.commit().unwrap();
        assert_eq!(fixture.exchanges, 3);
        let mut incomplete = Fixture {
            page_events: 1,
            ..Default::default()
        };
        assert!(captured(&mut incomplete, request()).commit().is_err());
        assert_eq!(incomplete.snapshots.len(), 1);
        assert!(!incomplete.endpoint.pending());
    }

    #[test]
    fn stale_cursor_token_and_token_exhaustion_never_resume_or_reuse() {
        let mut fixture = Fixture {
            page_events: 1,
            ..Default::default()
        };
        let EndpointReply::Page(page) = fixture
            .endpoint_exchange(EndpointRequest::Execute(request()))
            .unwrap()
        else {
            panic!("page")
        };
        let before = fixture.calls.clone();
        assert!(
            fixture
                .endpoint_exchange(EndpointRequest::Page {
                    token: page.binding.token,
                    cursor: 2
                })
                .is_err()
        );
        assert!(
            fixture
                .endpoint_exchange(EndpointRequest::Commit {
                    token: EndpointToken(900)
                })
                .is_err()
        );
        assert!(
            fixture
                .endpoint_exchange(EndpointRequest::Execute(request()))
                .is_err()
        );
        assert_eq!(fixture.calls, before);
        fixture
            .endpoint_exchange(EndpointRequest::Abort {
                token: page.binding.token,
            })
            .unwrap();
        let next = captured(
            &mut fixture,
            ExecutionRequest {
                sdk_cursor: 4,
                ..request()
            },
        );
        assert_ne!(next.page().binding.token, page.binding.token);
        next.abort().unwrap();
        fixture.endpoint.next_token = u64::MAX;
        let before = fixture.calls.clone();
        assert!(
            fixture
                .endpoint_exchange(EndpointRequest::Execute(ExecutionRequest {
                    sdk_cursor: 6,
                    ..request()
                }))
                .is_err()
        );
        assert_eq!(fixture.calls, before);
    }

    #[test]
    fn post_mutation_failures_preserve_primary_errors_and_release_unknown_handles() {
        for stage in [
            "branch",
            "run",
            "snapshot-before",
            "snapshot-after",
            "page",
            "hash",
            "history",
        ] {
            let mut fixture = Fixture {
                fail: Some(stage),
                ..Default::default()
            };
            let mut spec = request();
            spec.branch = Some(EndpointBranch::new(SnapId(10), &InputSpec::seeded(7)).unwrap());
            spec.sdk_cursor = 5;
            let result = match ExecutionEndpoint::execute(&mut fixture, spec) {
                Ok(ExecutionStart::Endpoint(endpoint)) => endpoint.commit().map(drop),
                Ok(ExecutionStart::Decision(_)) => panic!("unexpected decision"),
                Err(error) => Err(error),
            };
            assert_eq!(
                result.unwrap_err().downcast_ref::<Injected>().unwrap().0,
                stage
            );
            assert!(fixture.abandoned);
            assert!(fixture.snapshots.is_empty());
            assert_eq!(
                &fixture.calls[fixture.calls.len() - 2..],
                ["abandon", "clear-snapshots"]
            );
        }
    }

    #[test]
    fn raii_aborts_provisional_snapshot_and_commit_reply_loss_abandons_all_ownership() {
        let mut fixture = Fixture::default();
        drop(captured(&mut fixture, request()));
        assert_eq!(fixture.snapshots.len(), 1);
        assert!(!fixture.endpoint.pending());
        assert!(!fixture.abandoned);
        let mut lost = Fixture {
            lose_commit: true,
            ..Default::default()
        };
        assert!(captured(&mut lost, request()).commit().is_err());
        assert!(lost.abandoned);
        assert!(lost.snapshots.is_empty());
        let mut corrupt = Fixture {
            corrupt_page: true,
            ..Default::default()
        };
        assert!(ExecutionEndpoint::execute(&mut corrupt, request()).is_err());
        assert!(corrupt.abandoned);
        assert!(corrupt.snapshots.is_empty());
    }
}
