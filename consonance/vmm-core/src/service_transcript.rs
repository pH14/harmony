// SPDX-License-Identifier: AGPL-3.0-or-later

use hypercall_proto::{ServiceId, decode, encode_request};
use sha2::{Digest, Sha256};

use crate::vmm::SdkStop;

pub(crate) fn exchange_digest(requested_len: u32, request: &[u8], response: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"consonance.service-exchange.v1\0");
    frame_digest(&mut digest, requested_len, request);
    frame_digest(&mut digest, response.len() as u32, response);
    digest.finalize().into()
}

fn frame_digest(digest: &mut Sha256, advertised_len: u32, bytes: &[u8]) {
    if let Ok((header, payload)) = decode(bytes) {
        digest.update([0]);
        digest.update(header.kind.to_le_bytes());
        digest.update(header.service.to_le_bytes());
        digest.update(header.opcode.to_le_bytes());
        digest.update(header.status.to_le_bytes());
        digest.update(header.seq.to_le_bytes());
        digest.update((payload.len() as u32).to_le_bytes());
        digest.update(payload);
    } else {
        digest.update([1]);
        digest.update(advertised_len.to_le_bytes());
        digest.update((bytes.len() as u32).to_le_bytes());
        digest.update(bytes);
    }
}

pub(crate) fn pending_request(stop: &SdkStop) -> Option<Vec<u8>> {
    let (opcode, seq, payload) = match stop {
        SdkStop::Decision { seq, question, .. } => {
            let mut payload = question.service().to_le_bytes().to_vec();
            payload.extend_from_slice(&question.request_id().to_le_bytes());
            payload.extend_from_slice(question.payload());
            (3, *seq, payload)
        }
        SdkStop::CoverageDecision {
            seq,
            question,
            ready,
            ..
        } => {
            let thread = u32::try_from(question.request_id()).ok()?;
            if question.payload().len() != 8 {
                return None;
            }
            let mut payload = thread.to_le_bytes().to_vec();
            payload.extend_from_slice(question.payload());
            payload.extend_from_slice(&ready.to_le_bytes());
            (2, *seq, payload)
        }
        _ => return None,
    };
    let mut request = vec![0; hypercall_proto::HEADER_LEN + payload.len()];
    let len = encode_request(ServiceId::Sdk, opcode, seq, &payload, &mut request).ok()?;
    request.truncate(len);
    Some(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use environment::channel;
    use hypercall_proto::{Status, encode_response};

    fn request(service: ServiceId, opcode: u16, seq: u32, payload: &[u8]) -> Vec<u8> {
        let mut frame = vec![0; hypercall_proto::HEADER_LEN + payload.len()];
        encode_request(service, opcode, seq, payload, &mut frame).unwrap();
        frame
    }

    fn response(status: Status, payload: &[u8]) -> Vec<u8> {
        let mut frame = vec![0; hypercall_proto::HEADER_LEN + payload.len()];
        encode_response(ServiceId::Sdk, 3, 7, status, payload, &mut frame).unwrap();
        frame
    }

    #[test]
    fn canonical_exchange_binds_identity_payload_status_and_response() {
        let baseline = request(ServiceId::Sdk, 3, 7, b"request");
        let answer = response(Status::Ok, b"answer");
        let digest = exchange_digest(baseline.len() as u32, &baseline, &answer);
        for changed in [
            request(ServiceId::Entropy, 3, 7, b"request"),
            request(ServiceId::Sdk, 2, 7, b"request"),
            request(ServiceId::Sdk, 3, 8, b"request"),
            request(ServiceId::Sdk, 3, 7, b"changed"),
        ] {
            assert_eq!(changed.len(), baseline.len());
            assert_ne!(
                exchange_digest(changed.len() as u32, &changed, &answer),
                digest
            );
        }
        for changed in [
            response(Status::BadRequest, b"answer"),
            response(Status::Ok, b"change"),
        ] {
            assert_eq!(changed.len(), answer.len());
            assert_ne!(
                exchange_digest(baseline.len() as u32, &baseline, &changed),
                digest
            );
        }
        let mut padded = baseline.clone();
        padded.extend_from_slice(b"ignored transport padding");
        assert_eq!(
            exchange_digest(padded.len() as u32, &padded, &answer),
            digest
        );
        assert_ne!(
            exchange_digest(4097, &[], &answer),
            exchange_digest(4098, &[], &answer)
        );
        assert_ne!(
            exchange_digest(3, b"bad", &answer),
            exchange_digest(3, b"BAD", &answer)
        );
    }

    #[test]
    fn saved_decisions_reconstruct_all_canonical_request_fields() {
        let question = channel::Question::with_request_id(19, 77, b"body".to_vec()).unwrap();
        let stop = SdkStop::Decision {
            moment: 1,
            seq: 9,
            question,
        };
        let mut payload = 77_u16.to_le_bytes().to_vec();
        payload.extend_from_slice(&19_u64.to_le_bytes());
        payload.extend_from_slice(b"body");
        assert_eq!(
            pending_request(&stop).unwrap(),
            request(ServiceId::Sdk, 3, 9, &payload)
        );
        let question = channel::Question::with_request_id(
            12,
            channel::SERVICE_COVERAGE_QUANTUM,
            34_u64.to_le_bytes().to_vec(),
        )
        .unwrap();
        let stop = SdkStop::CoverageDecision {
            moment: 1,
            seq: 56,
            question,
            ready: 78,
        };
        let mut payload = 12_u32.to_le_bytes().to_vec();
        payload.extend_from_slice(&34_u64.to_le_bytes());
        payload.extend_from_slice(&78_u32.to_le_bytes());
        assert_eq!(
            pending_request(&stop).unwrap(),
            request(ServiceId::Sdk, 2, 56, &payload)
        );
    }
}
