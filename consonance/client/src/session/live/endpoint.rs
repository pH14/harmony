// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use crate::endpoint::{
    EndpointDriver, EndpointError, EndpointEvent, EndpointProof, EndpointReply, EndpointRequest,
    EndpointState,
};

impl Session {
    pub fn execution_endpoint(
        &mut self,
        request: EndpointRequest,
    ) -> Result<EndpointReply, EndpointError> {
        if let Err(error) = self.loaded_image_guard.check() {
            self.abandon_execution_endpoint();
            return Err(error.into());
        }
        let mut state = std::mem::take(&mut self.endpoint_state);
        let result = state.exchange(self, request);
        self.endpoint_state = state;
        if let Err(error) = self.loaded_image_guard.check() {
            self.abandon_execution_endpoint();
            return match result {
                Err(primary) => Err(primary),
                Ok(_) => Err(error.into()),
            };
        }
        result
    }

    pub(super) fn ensure_mutation_available(&self) -> Result<(), EndpointError> {
        if self.abandoned {
            return Err(SessionError::Abandoned.into());
        }
        if self.endpoint_state.pending() {
            return Err(crate::endpoint::EndpointProtocolError::Pending.into());
        }
        Ok(())
    }

    pub(in crate::session) fn abandon_execution_endpoint(&mut self) {
        self.abandoned = true;
        self.client.transport_mut().abandon_owned_session();
        self.endpoint_state = EndpointState::default();
        self.snapshot_times.clear();
    }
}

impl EndpointDriver for Session {
    fn ensure_active(&self) -> Result<(), EndpointError> {
        if self.abandoned {
            Err(SessionError::Abandoned.into())
        } else {
            Ok(())
        }
    }

    fn supports_proof(&self) -> bool {
        true
    }

    fn event_count(&self, parent: Option<SnapId>) -> Result<Option<u32>, EndpointError> {
        let count = match parent {
            Some(parent) => self
                .client
                .transport()
                .snapshot_sdk_event_count(parent)
                .ok_or_else(|| {
                    SessionError::Control(format!("unknown endpoint parent {parent:?}"))
                })?,
            None => u64::try_from(
                self.client
                    .transport()
                    .vmm()
                    .ok_or(SessionError::Abandoned)?
                    .sdk_events()
                    .len(),
            )?,
        };
        Ok(Some(u32::try_from(count).map_err(|_| {
            SessionError::Control("SDK event count overflow".into())
        })?))
    }

    fn branch(&mut self, parent: SnapId, spec: &InputSpec) -> Result<(), EndpointError> {
        self.branch_input(parent, spec)
    }

    fn run(
        &mut self,
        until: StopConditions,
        resolve: Option<control_proto::Resolution>,
    ) -> Result<StopReason, EndpointError> {
        Session::run(self, until, resolve)
    }

    fn snapshot(&mut self) -> Result<(SnapId, u64), EndpointError> {
        Session::snapshot(self)
    }

    fn page(&mut self, cursor: u32, end: u32) -> Result<Vec<EndpointEvent>, EndpointError> {
        let reply = self
            .client
            .request(&Request::SdkEvents { offset: cursor })
            .map_err(|error| SessionError::Control(error.to_string()))?;
        let Reply::SdkEvents(events) = reply else {
            return Err(SessionError::Reply {
                operation: "endpoint SDK page",
                reply,
            }
            .into());
        };
        if cursor
            .checked_add(u32::try_from(events.len())?)
            .is_none_or(|next| next > end)
        {
            return Err(
                SessionError::Control("endpoint SDK page exceeds captured end".into()).into(),
            );
        }
        Ok(events)
    }

    fn proof(&mut self) -> Result<EndpointProof, EndpointError> {
        let state_hash = self.state_hash()?;
        let history = self.event_history()?;
        Ok(EndpointProof::WholeStateAndHistory {
            state_hash,
            history,
        })
    }

    fn drop_snapshot(&mut self, snapshot: SnapId) -> Result<(), EndpointError> {
        Session::drop_snapshot(self, snapshot)
    }

    fn abandon(&mut self) {
        self.abandon_execution_endpoint();
    }
}
