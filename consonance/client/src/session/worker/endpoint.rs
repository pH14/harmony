// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use crate::endpoint::*;

impl<T: SearchSession + ?Sized> EndpointTransport for T {
    fn endpoint_exchange(
        &mut self,
        request: EndpointRequest,
    ) -> Result<EndpointReply, EndpointError> {
        self.execution_endpoint(request)
    }

    fn endpoint_abandon(&mut self) {
        self.abandon_execution_endpoint();
    }
}

impl WorkerSession {
    pub(super) fn retire_endpoint_worker(&self) {
        self.failed.set(true);
        let _ = self.stream.shutdown(Shutdown::Both);
    }

    pub(super) fn call_endpoint(
        &self,
        request: EndpointRequest,
    ) -> Result<EndpointReply, EndpointError> {
        let commit = matches!(request, EndpointRequest::Commit { .. });
        let mut out = Out::measured(ENDPOINT, &self.copies);
        put_request(&mut out, &request)?;
        let _exchange = self.begin_exchange()?;
        if let Err(error) =
            self.check_loaded_image_at(crate::image_identity::GuardSite::ParentRpcBefore)
        {
            self.retire_endpoint_worker();
            return Err(error);
        }
        let received = send_measured(
            &self.stream,
            &out.0,
            &[],
            &self.transport,
            work::Group::Endpoint,
        )
        .and_then(|()| {
            receive_measured(
                &self.stream,
                ENDPOINT_FRAME_LIMIT,
                None,
                &self.transport,
                Some(work::Group::Endpoint),
            )
        });
        let (body, fds) = match received {
            Ok(frame) => frame,
            Err(error) => {
                self.retire_endpoint_worker();
                return Err(transport_error(error).into());
            }
        };
        let result = if fds.is_empty() {
            match self.reply_status(&body) {
                Ok(bytes) => {
                    let decoded = take_reply(&bytes);
                    if decoded.is_err() {
                        self.retire_endpoint_worker();
                    }
                    decoded
                }
                Err(error) => Err(error),
            }
        } else {
            self.retire_endpoint_worker();
            Err(malformed("execution endpoint reply carries descriptors").into())
        };
        let guard = self
            .loaded_image_guard
            .check_at(crate::image_identity::GuardSite::ParentRpcAfter);
        if let Err(error) = guard {
            self.retire_endpoint_worker();
            return match result {
                Err(primary) => Err(primary),
                Ok(_) => Err(error.into()),
            };
        }
        if self.failed.get()
            || (commit && result.is_err())
            || result
                .as_ref()
                .is_err_and(|error| error.downcast_ref::<io::Error>().is_some())
        {
            self.retire_endpoint_worker();
        }
        result
    }
}

fn put_stop(out: &mut Out, stop: &StopReason) -> Result<(), EndpointError> {
    let mut bytes = Vec::new();
    encode_reply(0, &Ok(Reply::Stop(stop.clone())), &mut bytes)?;
    out.bytes(&bytes);
    Ok(())
}

fn take_stop(input: &mut In<'_>) -> Result<StopReason, EndpointError> {
    let bytes = input.bytes()?;
    match decode_reply(bytes)? {
        Some((0, Ok(Reply::Stop(stop)), consumed)) if consumed == bytes.len() => Ok(stop),
        _ => Err(malformed("endpoint stop reason").into()),
    }
}

fn put_binding(out: &mut Out, binding: &EndpointBinding) -> Result<(), EndpointError> {
    out.u64(binding.token.0).bytes(&binding.request_digest);
    put_stop(out, &binding.stop)?;
    out.u64(u64::from(binding.source_count))
        .u64(u64::from(binding.start_cursor))
        .u64(u64::from(binding.end_cursor));
    Ok(())
}

fn u32_field(input: &mut In<'_>) -> io::Result<u32> {
    u32::try_from(input.u64()?).map_err(|_| malformed("endpoint cursor overflow"))
}

fn take_binding(input: &mut In<'_>) -> Result<EndpointBinding, EndpointError> {
    Ok(EndpointBinding {
        token: EndpointToken(input.u64()?),
        request_digest: input
            .bytes()?
            .try_into()
            .map_err(|_| malformed("endpoint request digest"))?,
        stop: take_stop(input)?,
        source_count: u32_field(input)?,
        start_cursor: u32_field(input)?,
        end_cursor: u32_field(input)?,
    })
}

pub(super) fn put_request(out: &mut Out, request: &EndpointRequest) -> Result<(), EndpointError> {
    match request {
        EndpointRequest::Execute(request) => {
            request.digest()?;
            out.u8(0)
                .option(request.branch.as_ref().map(|branch| branch.parent.0));
            if let Some(branch) = &request.branch {
                out.bytes(&branch.input);
            }
            let mut bytes = Vec::new();
            control_proto::encode_request(
                0,
                &control_proto::Request::Run {
                    until: request.until,
                    resolve: request.resolve.clone(),
                },
                &mut bytes,
            )?;
            out.bytes(&bytes)
                .u64(u64::from(request.sdk_cursor))
                .u8(request.snapshot_on.bits())
                .u8(match request.proof {
                    EndpointProofRequest::Omit => 0,
                    EndpointProofRequest::WholeStateAndHistory => 1,
                    EndpointProofRequest::CapturedWholeStateAndHistory => 2,
                });
        }
        EndpointRequest::Resume { token, resolution } => {
            if resolution.answer.0.len() > hypercall_proto::MAX_PAYLOAD {
                return Err(malformed("endpoint resolution exceeds limit").into());
            }
            out.u8(4)
                .u64(token.0)
                .u64(resolution.vtime.0)
                .u64(u64::from(resolution.service))
                .u64(resolution.id.0)
                .bytes(&resolution.answer.0);
        }
        EndpointRequest::FinalizeDecision { token, capture } => {
            out.u8(5).u64(token.0).u8(u8::from(*capture));
        }
        EndpointRequest::Page { token, cursor } => {
            out.u8(1).u64(token.0).u64(u64::from(*cursor));
        }
        EndpointRequest::Commit { token } => {
            out.u8(2).u64(token.0);
        }
        EndpointRequest::Abort { token } => {
            out.u8(3).u64(token.0);
        }
    }
    if out.0.len() > ENDPOINT_FRAME_LIMIT {
        return Err(malformed("endpoint request exceeds limit").into());
    }
    Ok(())
}

pub(super) fn take_request(input: &mut In<'_>) -> Result<EndpointRequest, EndpointError> {
    Ok(match input.u8()? {
        0 => {
            let branch = match input.option()? {
                Some(parent) => {
                    let bytes = input.bytes()?;
                    if bytes.len() > InputSpec::MAX_REPRODUCER_BYTES {
                        return Err(malformed("endpoint input exceeds limit").into());
                    }
                    Some(EndpointBranch {
                        parent: SnapId(parent),
                        input: bytes.to_vec(),
                    })
                }
                None => None,
            };
            let bytes = input.bytes()?;
            let (until, resolve) = match control_proto::decode_request(bytes)? {
                Some((0, control_proto::Request::Run { until, resolve }, consumed))
                    if consumed == bytes.len() =>
                {
                    (until, resolve)
                }
                _ => return Err(malformed("endpoint run request").into()),
            };
            let sdk_cursor = u32_field(input)?;
            let snapshot_on = EndpointStops::from_bits(input.u8()?)?;
            let proof = match input.u8()? {
                0 => EndpointProofRequest::Omit,
                1 => EndpointProofRequest::WholeStateAndHistory,
                2 => EndpointProofRequest::CapturedWholeStateAndHistory,
                _ => return Err(malformed("endpoint proof tag").into()),
            };
            let request = ExecutionRequest {
                branch,
                until,
                resolve,
                sdk_cursor,
                snapshot_on,
                proof,
            };
            request.digest()?;
            EndpointRequest::Execute(request)
        }
        4 => {
            let token = EndpointToken(input.u64()?);
            let vtime = Moment(input.u64()?);
            let service =
                u16::try_from(input.u64()?).map_err(|_| malformed("endpoint service overflow"))?;
            let id = DecisionId(input.u64()?);
            let bytes = input.bytes()?;
            if bytes.len() > hypercall_proto::MAX_PAYLOAD {
                return Err(malformed("endpoint resolution exceeds limit").into());
            }
            EndpointRequest::Resume {
                token,
                resolution: Resolution {
                    vtime,
                    service,
                    id,
                    answer: Answer(bytes.to_vec()),
                },
            }
        }
        5 => {
            let token = EndpointToken(input.u64()?);
            let capture = match input.u8()? {
                0 => false,
                1 => true,
                _ => return Err(malformed("endpoint capture tag").into()),
            };
            EndpointRequest::FinalizeDecision { token, capture }
        }
        1 => EndpointRequest::Page {
            token: EndpointToken(input.u64()?),
            cursor: u32_field(input)?,
        },
        2 => EndpointRequest::Commit {
            token: EndpointToken(input.u64()?),
        },
        3 => EndpointRequest::Abort {
            token: EndpointToken(input.u64()?),
        },
        _ => return Err(malformed("endpoint request tag").into()),
    })
}

pub(super) fn put_reply(out: &mut Out, reply: &EndpointReply) -> Result<(), EndpointError> {
    match reply {
        EndpointReply::Decision(binding) => {
            if !matches!(binding.stop, StopReason::Decision { .. }) {
                return Err(malformed("endpoint decision tag differs").into());
            }
            out.u8(0);
            put_binding(out, binding)?;
        }
        EndpointReply::Page(page) => {
            page.validate()?;
            out.u8(1);
            put_binding(out, &page.binding)?;
            out.u64(u64::from(page.cursor))
                .u64(u64::from(page.next_cursor))
                .len(page.events.len());
            for (moment, id, bytes) in &page.events {
                out.u64(*moment);
                out.0.extend_from_slice(&id.to_le_bytes());
                out.0
                    .extend_from_slice(&u32::try_from(bytes.len())?.to_le_bytes());
                out.0.extend_from_slice(bytes);
                if let Some(copies) = &out.1 {
                    copies.copied(CopySite::Encode, bytes.len());
                }
            }
        }
        EndpointReply::Committed(receipt) => {
            out.u8(2);
            put_binding(out, &receipt.binding)?;
            out.option(receipt.snapshot.map(|(snapshot, _)| snapshot.0));
            if let Some((_, at)) = receipt.snapshot {
                out.u64(at);
            }
            match &receipt.proof {
                EndpointProof::Unavailable => {
                    out.u8(0);
                }
                EndpointProof::WholeStateAndHistory {
                    state_hash,
                    history,
                } => {
                    out.u8(1)
                        .bytes(state_hash)
                        .u64(history.count)
                        .bytes(&history.digest);
                }
            }
        }
        EndpointReply::Aborted => {
            out.u8(3);
        }
    }
    if out.0.len() >= ENDPOINT_FRAME_LIMIT {
        return Err(malformed("endpoint reply exceeds limit").into());
    }
    Ok(())
}

fn take_reply(bytes: &[u8]) -> Result<EndpointReply, EndpointError> {
    let mut input = In(bytes);
    let reply = match input.u8()? {
        0 => {
            let binding = take_binding(&mut input)?;
            if !matches!(binding.stop, StopReason::Decision { .. }) {
                return Err(malformed("endpoint decision reply").into());
            }
            EndpointReply::Decision(binding)
        }
        1 => {
            let binding = take_binding(&mut input)?;
            let cursor = u32_field(&mut input)?;
            let next_cursor = u32_field(&mut input)?;
            let count = input.len()?;
            if count > input.0.len() / 16 || count > ENDPOINT_PAGE_LIMIT / 16 {
                return Err(malformed("endpoint event count exceeds bytes").into());
            }
            let mut events = Vec::with_capacity(count);
            for _ in 0..count {
                let moment = input.u64()?;
                let id = u32::from_le_bytes(input.take(4)?.try_into().expect("four bytes"));
                let len = u32::from_le_bytes(input.take(4)?.try_into().expect("four bytes"));
                let bytes = input.take(usize::try_from(len)?)?.to_vec();
                events.push((moment, id, bytes));
            }
            let page = EndpointPage {
                binding,
                cursor,
                next_cursor,
                events,
            };
            page.validate()?;
            EndpointReply::Page(page)
        }
        2 => {
            let binding = take_binding(&mut input)?;
            let snapshot = match input.option()? {
                Some(id) => Some((SnapId(id), input.u64()?)),
                None => None,
            };
            let proof = match input.u8()? {
                0 => EndpointProof::Unavailable,
                1 => EndpointProof::WholeStateAndHistory {
                    state_hash: input
                        .bytes()?
                        .try_into()
                        .map_err(|_| malformed("endpoint state hash"))?,
                    history: control_proto::EventHistory {
                        count: input.u64()?,
                        digest: input
                            .bytes()?
                            .try_into()
                            .map_err(|_| malformed("endpoint history digest"))?,
                    },
                },
                _ => return Err(malformed("endpoint proof reply tag").into()),
            };
            EndpointReply::Committed(EndpointReceipt {
                binding,
                snapshot,
                proof,
            })
        }
        3 => EndpointReply::Aborted,
        _ => return Err(malformed("endpoint reply tag").into()),
    };
    input.finish()?;
    Ok(reply)
}

pub(super) fn status(result: Result<Out, Box<dyn Error>>, copies: &Arc<CopyWork>) -> Vec<u8> {
    let error = match result {
        Ok(out) => return status_measured(Ok(out), copies),
        Err(error) => error,
    };
    let mut out = Out::measured(FAILED, copies);
    match error.downcast_ref::<SessionError>() {
        Some(SessionError::Hung(limit)) => {
            out.u8(HUNG)
                .u64(u64::try_from(limit.as_nanos()).unwrap_or(u64::MAX));
        }
        Some(SessionError::Abandoned) => {
            out.u8(ABANDONED);
        }
        Some(SessionError::Infrastructure(detail)) => {
            out.u8(INFRASTRUCTURE)
                .bytes(bounded_detail(detail).as_bytes());
        }
        _ => {
            out.u8(OTHER)
                .bytes(bounded_detail(error.as_ref()).as_bytes());
        }
    }
    out.0
}

fn bounded_detail(detail: impl std::fmt::Display) -> String {
    use std::fmt::Write;
    struct Detail(String);
    impl std::fmt::Write for Detail {
        fn write_str(&mut self, value: &str) -> std::fmt::Result {
            let mut len = value.len().min((64 << 10) - self.0.len());
            while !value.is_char_boundary(len) {
                len -= 1;
            }
            self.0.push_str(&value[..len]);
            Ok(())
        }
    }
    let mut result = Detail(String::new());
    let _ = write!(&mut result, "{detail}");
    result.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_maximum_legal_control_sdk_page_fits_the_endpoint_wire_without_widening_rows() {
        let events = vec![(7, 19, vec![3; ENDPOINT_PAGE_LIMIT - 22])];
        let mut control = Vec::new();
        encode_reply(0, &Ok(Reply::SdkEvents(events.clone())), &mut control).unwrap();
        let page = EndpointPage {
            binding: EndpointBinding {
                token: EndpointToken(1),
                request_digest: [7; 32],
                stop: StopReason::Deadline { vtime: Moment(20) },
                source_count: 0,
                start_cursor: 0,
                end_cursor: 1,
            },
            cursor: 0,
            next_cursor: 1,
            events,
        };
        page.validate().unwrap();
        let reply = EndpointReply::Page(page);
        let mut out = Out::default();
        put_reply(&mut out, &reply).unwrap();
        assert!(out.0.len() < ENDPOINT_FRAME_LIMIT);
        assert_eq!(take_reply(&out.0).unwrap(), reply);
    }

    #[test]
    fn endpoint_diagnostics_are_bounded_utf8_and_preserve_typed_status() {
        let detail = "é".repeat(40_000);
        let bytes = status(
            Err(SessionError::Infrastructure(detail).into()),
            &Arc::default(),
        );
        let mut input = In(&bytes);
        assert_eq!(input.u8().unwrap(), FAILED);
        assert_eq!(input.u8().unwrap(), INFRASTRUCTURE);
        let detail = input.string().unwrap();
        assert_eq!(detail.len(), 64 << 10);
        assert!(detail.chars().all(|character| character == 'é'));
        input.finish().unwrap();
        let bytes = status(
            Err(SessionError::Hung(Duration::from_secs(3)).into()),
            &Arc::default(),
        );
        let mut input = In(&bytes);
        assert_eq!(input.u8().unwrap(), FAILED);
        assert_eq!(input.u8().unwrap(), HUNG);
        assert_eq!(input.u64().unwrap(), 3_000_000_000);
        input.finish().unwrap();
    }
}
