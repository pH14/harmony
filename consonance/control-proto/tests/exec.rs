// SPDX-License-Identifier: AGPL-3.0-or-later
//! Codec coverage for the durable command control added in application
//! protocol 13. These tests pin the new optional state shape and make every
//! enum tag/truncation boundary fail closed without changing legacy `Exec`.

use control_proto::{
    ExecCompletion, ExecStatus, Moment, PROTO_VERSION, Reply, Request, StopReason, decode_reply,
    decode_request, encode_reply, encode_request,
};

const MAGIC: [u8; 4] = *b"CTL1";
const HEADER_LEN: usize = 14;

fn frame(body: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(HEADER_LEN + body.len());
    bytes.extend_from_slice(&MAGIC);
    bytes.extend_from_slice(&PROTO_VERSION.to_le_bytes());
    bytes.extend_from_slice(&7u32.to_le_bytes());
    bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
    bytes.extend_from_slice(body);
    bytes
}

fn sample_status(completion: ExecCompletion) -> ExecStatus {
    ExecStatus {
        id: 9,
        at: Moment(100),
        completion,
        output: vec![0xCA, 0xFE],
        truncated: true,
    }
}

#[test]
fn durable_exec_requests_round_trip_without_running_the_guest() {
    for request in [
        Request::ExecStart {
            cmd: "echo retained".to_owned(),
        },
        Request::ExecStatus,
    ] {
        let mut bytes = Vec::new();
        encode_request(7, &request, &mut bytes).expect("encode");
        let (_, decoded, consumed) = decode_request(&bytes).expect("decode").expect("complete");
        assert_eq!(decoded, request);
        assert_eq!(consumed, bytes.len());
    }
}

#[test]
fn durable_exec_state_round_trips_every_completion_shape() {
    for status in [
        None,
        Some(sample_status(ExecCompletion::Pending)),
        Some(sample_status(ExecCompletion::Exited {
            status: 17,
            at: Moment(111),
        })),
        Some(sample_status(ExecCompletion::Aborted { at: Moment(112) })),
    ] {
        let reply = Ok(Reply::ExecState(status));
        let mut bytes = Vec::new();
        encode_reply(7, &reply, &mut bytes).expect("encode");
        let (_, decoded, consumed) = decode_reply(&bytes).expect("decode").expect("complete");
        assert_eq!(decoded, reply);
        assert_eq!(consumed, bytes.len());
    }
}

#[test]
fn durable_exec_start_truncations_need_more() {
    let request = Request::ExecStart {
        cmd: "retained".to_owned(),
    };
    let mut bytes = Vec::new();
    encode_request(7, &request, &mut bytes).expect("encode");
    for length in 0..bytes.len() {
        assert_eq!(
            decode_request(&bytes[..length]),
            Ok(None),
            "prefix {length}"
        );
    }
    assert!(decode_request(&bytes).expect("decode").is_some());
}

#[test]
fn durable_exec_state_truncations_need_more() {
    let reply = Ok(Reply::ExecState(Some(sample_status(
        ExecCompletion::Exited {
            status: 17,
            at: Moment(111),
        },
    ))));
    let mut bytes = Vec::new();
    encode_reply(7, &reply, &mut bytes).expect("encode");
    for length in 0..bytes.len() {
        assert_eq!(decode_reply(&bytes[..length]), Ok(None), "prefix {length}");
    }
    assert!(decode_reply(&bytes).expect("decode").is_some());
}

#[test]
fn durable_exec_tags_and_flags_reject_unknown_values() {
    // Request::ExecStart with a non-UTF-8 command is malformed, not a guessed
    // command. The length prefix is otherwise complete.
    assert_eq!(
        decode_request(&frame(&[0x0F, 0x01, 0x00, 0x00, 0x00, 0xFF])),
        Err(control_proto::ProtocolError::ShortFrame)
    );

    // Optional ExecState presence is a canonical 0/1 byte.
    assert_eq!(
        decode_reply(&frame(&[0x00, 0x0D, 0x02])),
        Err(control_proto::ProtocolError::ShortFrame)
    );

    // A present status reaches its completion tag only after id and endpoint.
    let mut unknown_completion = vec![0x00, 0x0D, 0x01];
    unknown_completion.extend_from_slice(&9u64.to_le_bytes());
    unknown_completion.extend_from_slice(&100u64.to_le_bytes());
    unknown_completion.push(0xFF);
    assert_eq!(
        decode_reply(&frame(&unknown_completion)),
        Err(control_proto::ProtocolError::ShortFrame)
    );

    // `truncated` is also canonical: values other than 0/1 are malformed.
    let mut bad_flag = vec![0x00, 0x0D, 0x01];
    bad_flag.extend_from_slice(&9u64.to_le_bytes());
    bad_flag.extend_from_slice(&100u64.to_le_bytes());
    bad_flag.push(0x00); // pending
    bad_flag.extend_from_slice(&0u32.to_le_bytes());
    bad_flag.push(0x02);
    assert_eq!(
        decode_reply(&frame(&bad_flag)),
        Err(control_proto::ProtocolError::ShortFrame)
    );

    // An unknown StopReason tag is rejected even when its outer reply is valid.
    assert_eq!(
        decode_reply(&frame(&[0x00, 0x04, 0xFF])),
        Err(control_proto::ProtocolError::ShortFrame)
    );
}

#[test]
fn exec_complete_stop_round_trips_and_rejects_trailing_body_bytes() {
    let reply = Ok(Reply::Stop(StopReason::ExecComplete {
        vtime: Moment(12),
        id: 34,
    }));
    let mut encoded = Vec::new();
    encode_reply(7, &reply, &mut encoded).expect("encode");
    let (_, decoded, consumed) = decode_reply(&encoded).expect("decode").expect("complete");
    assert_eq!(decoded, reply);
    assert_eq!(consumed, encoded.len());

    let mut trailing = vec![0x00, 0x04, 0x07];
    trailing.extend_from_slice(&12u64.to_le_bytes());
    trailing.extend_from_slice(&34u64.to_le_bytes());
    trailing.push(0x00);
    assert_eq!(
        decode_reply(&frame(&trailing)),
        Err(control_proto::ProtocolError::ShortFrame)
    );
}
