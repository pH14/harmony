// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use crate::endpoint::*;
use environment::input_spec::InputSpec;

impl UmlSession {
    pub(super) fn ensure_endpoint_mutation(&self) -> Result<(), EndpointError> {
        if self.abandoned {
            return Err(SessionError::Abandoned.into());
        }
        if self.endpoint_state.pending() {
            return Err(EndpointProtocolError::Pending.into());
        }
        Ok(())
    }

    pub(super) fn retire_endpoint(&mut self) {
        self.abandoned = true;
        if let Some(session) = self.session.take() {
            let _ = session.kill();
        }
        self.snapshots.clear();
        self.endpoint_state = EndpointState::default();
    }

    pub(super) fn exchange_endpoint(
        &mut self,
        request: EndpointRequest,
    ) -> Result<EndpointReply, EndpointError> {
        if let Err(error) = self.loaded_image_guard.check() {
            self.retire_endpoint();
            return Err(error.into());
        }
        let mut state = std::mem::take(&mut self.endpoint_state);
        let result = state.exchange(self, request);
        self.endpoint_state = state;
        if let Err(error) = self.loaded_image_guard.check() {
            self.retire_endpoint();
            return match result {
                Err(primary) => Err(primary),
                Ok(_) => Err(error.into()),
            };
        }
        result
    }
}

impl EndpointDriver for UmlSession {
    fn ensure_active(&self) -> Result<(), EndpointError> {
        if self.abandoned {
            Err(SessionError::Abandoned.into())
        } else {
            Ok(())
        }
    }
    fn supports_proof(&self) -> bool {
        false
    }
    fn validate_request(
        &self,
        request: &ExecutionRequest,
        branch: Option<&InputSpec>,
    ) -> Result<(), EndpointError> {
        if request.until.deadline.is_none() || request.until.on != control_proto::StopMask::NONE {
            return Err(EndpointProtocolError::Invalid(
                "UML endpoint requires deadline-only stop conditions",
            )
            .into());
        }
        if branch.is_some_and(|spec| {
            spec.seed() != self.seed
                || !spec.effects().is_empty()
                || !spec.reseeds().is_empty()
                || !spec.answers().is_empty()
                || !spec.request_bindings().is_empty()
        }) {
            return Err(EndpointProtocolError::Invalid(
                "UML endpoint branch inputs are unsupported",
            )
            .into());
        }
        Ok(())
    }
    fn event_count(&self, parent: Option<SnapId>) -> Result<Option<u32>, EndpointError> {
        if let Some(parent) = parent {
            if !self.snapshots.contains_key(&parent) {
                return Err(SessionError::Control(format!("unknown snapshot {parent:?}")).into());
            }
            return Ok(None);
        }
        let session = self.session.as_ref().ok_or(SessionError::Abandoned)?;
        Ok(Some(u32::try_from(session.events().len())?))
    }
    fn branch(&mut self, parent: SnapId, spec: &InputSpec) -> Result<(), EndpointError> {
        self.branch_with_service(
            parent,
            spec.config().clone(),
            spec.payloads().unwrap_or_default().to_vec(),
            Vec::new(),
            spec.coverage_schedule().cloned(),
        )
    }
    fn run(
        &mut self,
        until: control_proto::StopConditions,
        resolve: Option<Resolution>,
    ) -> Result<StopReason, EndpointError> {
        let deadline = until
            .deadline
            .ok_or(EndpointProtocolError::Invalid("missing UML deadline"))?
            .0;
        match resolve {
            Some(resolution) => self.run_resolving(deadline, resolution),
            None => self.run_until(deadline),
        }
    }
    fn snapshot(&mut self) -> Result<(SnapId, u64), EndpointError> {
        SearchSession::snapshot(self)
    }
    fn page(&mut self, cursor: u32, end: u32) -> Result<Vec<EndpointEvent>, EndpointError> {
        self.loaded_image_guard.check()?;
        let session = self.session.as_ref().ok_or(SessionError::Abandoned)?;
        if u32::try_from(session.events().len())? != end {
            return Err(EndpointProtocolError::Invalid("UML endpoint cut differs").into());
        }
        let mut bytes = 0usize;
        let mut events = Vec::new();
        for event in session.events().iter().skip(usize::try_from(cursor)?) {
            let size = 16usize
                .checked_add(event.data.len())
                .ok_or(EndpointProtocolError::Invalid("UML event length overflow"))?;
            if size > ENDPOINT_PAGE_LIMIT {
                return Err(EndpointProtocolError::Invalid("UML event exceeds page limit").into());
            }
            if bytes + size > ENDPOINT_PAGE_LIMIT {
                break;
            }
            bytes += size;
            events.push((event.moment, event.id, event.data.to_vec()));
        }
        Ok(events)
    }
    fn proof(&mut self) -> Result<EndpointProof, EndpointError> {
        Err(EndpointProtocolError::Unavailable.into())
    }
    fn drop_snapshot(&mut self, snapshot: SnapId) -> Result<(), EndpointError> {
        SearchSession::drop_snapshot(self, snapshot)
    }
    fn abandon(&mut self) {
        self.retire_endpoint();
    }
}
