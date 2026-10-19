// SPDX-License-Identifier: AGPL-3.0-or-later

fn frame(service: ServiceId, opcode: u16, seq: u32, payload: &[u8]) -> Vec<u8> {
    let mut frame = vec![0; hypercall_proto::HEADER_LEN + payload.len()];
    hypercall_proto::encode_request(service, opcode, seq, payload, &mut frame).unwrap();
    frame
}

fn build(request: &[u8]) -> Vmm<MockBackend> {
    let mut vmm = Vmm::new(
        configured_mock(vec![Exit::Arch(X86Exit::Io {
            port: DOORBELL_PORT,
            size: 4,
            write: Some(request.len() as u32),
        })]),
        GuestRam::new(TEST_RAM).unwrap(),
    );
    vmm.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 7).unwrap());
    enable_nominal(&mut vmm, 7);
    vmm.guest_slice_mut(REQ_GPA as u64, request.len())
        .unwrap()
        .copy_from_slice(request);
    vmm
}

fn erase_transport(vmm: &mut Vmm<MockBackend>) {
    vmm.guest_slice_mut(REQ_GPA as u64, 2 * HC_PAGE)
        .unwrap()
        .fill(0);
}

#[test]
fn service_transcript_detects_equal_length_entropy_requests_after_endpoint_reconvergence() {
    let one = frame(ServiceId::Entropy, 1, 9, &1_u32.to_le_bytes());
    let two = frame(ServiceId::Entropy, 1, 9, &2_u32.to_le_bytes());
    assert_eq!(one.len(), two.len());
    let mut first = build(&one);
    let mut second = build(&two);
    first.step().unwrap();
    second.step().unwrap();
    let first_response = decode(first.guest_slice(RESP_GPA as u64, HC_PAGE).unwrap()).unwrap();
    let second_response = decode(second.guest_slice(RESP_GPA as u64, HC_PAGE).unwrap()).unwrap();
    assert_eq!((first_response.1.len(), second_response.1.len()), (1, 2));
    assert_eq!(first.entropy_state(), second.entropy_state());
    assert_eq!(
        first.virtual_time_trace().unwrap().normalized_log(),
        second.virtual_time_trace().unwrap().normalized_log()
    );
    erase_transport(&mut first);
    erase_transport(&mut second);
    assert_eq!(first.state_hash().unwrap(), second.state_hash().unwrap());
    let first_history = first.event_history().unwrap();
    let second_history = second.event_history().unwrap();
    assert_eq!((first_history.count, second_history.count), (2, 2));
    assert_ne!(first_history.digest, second_history.digest);
}

#[test]
fn service_transcript_binds_error_responses_and_exchange_order() {
    let unknown = frame(ServiceId::Entropy, 99, 4, &[]);
    let malformed = frame(ServiceId::Entropy, 1, 4, &[]);
    let mut left = build(&unknown);
    let mut right = build(&malformed);
    left.step().unwrap();
    right.step().unwrap();
    assert_eq!(
        decode(left.guest_slice(RESP_GPA as u64, HC_PAGE).unwrap())
            .unwrap()
            .0
            .status,
        Status::UnknownOpcode as u16
    );
    assert_eq!(
        decode(right.guest_slice(RESP_GPA as u64, HC_PAGE).unwrap())
            .unwrap()
            .0
            .status,
        Status::BadRequest as u16
    );
    erase_transport(&mut left);
    erase_transport(&mut right);
    assert_eq!(left.state_hash().unwrap(), right.state_hash().unwrap());
    assert_ne!(left.event_history(), right.event_history());
    for (vmm, request) in [(&mut left, &malformed), (&mut right, &unknown)] {
        vmm.guest_slice_mut(REQ_GPA as u64, request.len())
            .unwrap()
            .copy_from_slice(request);
        assert_eq!(
            vmm.service_doorbell(request.len() as u32).unwrap(),
            Step::Continued
        );
        erase_transport(vmm);
    }
    assert_eq!(left.state_hash().unwrap(), right.state_hash().unwrap());
    assert_eq!(
        left.event_history().unwrap().count,
        right.event_history().unwrap().count
    );
    assert_ne!(left.event_history(), right.event_history());
}

#[test]
fn service_transcript_deferred_entropy_restores_request_and_completes_once() {
    let request = frame(ServiceId::Entropy, 1, 9, &2_u32.to_le_bytes());
    let mut immediate = build(&request);
    immediate.step().unwrap();
    let expected = immediate.event_history();
    let mut source = build(&request);
    assert_eq!(source.step_until(Some(0)).unwrap(), Step::SdkStop);
    assert_eq!(source.event_history().unwrap().count, 1);
    let entropy_before = source.entropy_state();
    let sdk = source.sdk_snapshot().unwrap().unwrap();
    let state = source.save_vm_state().unwrap();
    let memory = source.guest_memory().to_vec();
    let history = source.event_history().unwrap();
    assert_eq!(history.count, 1);
    let mut restored = build(&request);
    restored.restore_snapshot(&memory, &state).unwrap();
    restored.sdk_restore(&sdk).unwrap();
    restored.restore_event_history(history).unwrap();
    erase_transport(&mut restored);
    assert_eq!(restored.step_until(Some(0)).unwrap(), Step::SdkStop);
    assert_eq!(restored.event_history(), Some(history));
    assert_eq!(restored.entropy_state(), entropy_before);
    assert_eq!(restored.step().unwrap(), Step::Continued);
    assert_eq!(restored.event_history(), expected);
    assert_eq!(restored.entropy_state(), immediate.entropy_state());
    let completed = restored.event_history();
    assert!(restored.pending_deferred_doorbell().is_none());
    restored.sdk_snapshot().unwrap();
    restored.take_virtual_time_trace();
    assert_eq!(restored.event_history(), completed);
    restored.restore_event_history(history).unwrap();
    restored.sdk_restore(&sdk).unwrap();
    restored.restore_snapshot(&memory, &state).unwrap();
    restored.step().unwrap();
    assert_eq!(restored.event_history(), expected);
}

#[test]
fn service_transcript_external_answer_uses_captured_request_and_rejects_duplicate_completion() {
    let mut payload = 19_u16.to_le_bytes().to_vec();
    payload.extend_from_slice(&77_u64.to_le_bytes());
    payload.extend_from_slice(b"request");
    let request = frame(ServiceId::Sdk, 3, 42, &payload);
    let mut source = build(&request);
    assert_eq!(source.step_until(Some(0)).unwrap(), Step::SdkStop);
    let sdk = source.sdk_snapshot().unwrap().unwrap();
    let state = source.save_vm_state().unwrap();
    let memory = source.guest_memory().to_vec();
    let before = source.event_history().unwrap();
    let mut restored = build(&request);
    restored.restore_snapshot(&memory, &state).unwrap();
    restored.sdk_restore(&sdk).unwrap();
    restored.restore_event_history(before).unwrap();
    erase_transport(&mut restored);
    let answer = channel::Answer::Data(vec![0x5a]);
    source.resolve_service_answer(answer.clone(), None).unwrap();
    restored.resolve_service_answer(answer, None).unwrap();
    assert_eq!(source.event_history(), restored.event_history());
    assert_eq!(source.event_history().unwrap().count, before.count + 1);
    let completed = restored.event_history();
    assert!(
        restored
            .resolve_service_answer(channel::Answer::Nominal, None)
            .is_err()
    );
    assert_eq!(restored.event_history(), completed);
    let mut changed = build(&request);
    changed.restore_snapshot(&memory, &state).unwrap();
    changed.sdk_restore(&sdk).unwrap();
    changed.restore_event_history(before).unwrap();
    changed
        .resolve_service_answer(channel::Answer::Data(vec![0x6b]), None)
        .unwrap();
    source.sdk_restore(&sdk).unwrap();
    changed.sdk_restore(&sdk).unwrap();
    erase_transport(&mut source);
    erase_transport(&mut changed);
    assert_eq!(source.state_hash().unwrap(), changed.state_hash().unwrap());
    assert_eq!(
        source.event_history().unwrap().count,
        changed.event_history().unwrap().count
    );
    assert_ne!(source.event_history(), changed.event_history());
}

#[test]
fn service_transcript_coverage_answer_matches_inline_replay_from_saved_semantics() {
    let mut payload = 7_u32.to_le_bytes().to_vec();
    payload.extend_from_slice(&1_u64.to_le_bytes());
    payload.extend_from_slice(&2_u32.to_le_bytes());
    let request = frame(ServiceId::Sdk, 2, 91, &payload);
    let mut source = build(&request);
    assert_eq!(source.step_until(Some(0)).unwrap(), Step::SdkStop);
    let (moment, question) = source.pending_service_question().unwrap();
    let question = question.clone();
    let sdk = source.sdk_snapshot().unwrap().unwrap();
    let state = source.save_vm_state().unwrap();
    let memory = source.guest_memory().to_vec();
    let history = source.event_history().unwrap();
    let mut restored = build(&request);
    restored.restore_snapshot(&memory, &state).unwrap();
    restored.sdk_restore(&sdk).unwrap();
    restored.restore_event_history(history).unwrap();
    erase_transport(&mut restored);
    let answer = channel::Answer::Data(64_u64.to_le_bytes().to_vec());
    restored
        .resolve_service_answer(answer.clone(), None)
        .unwrap();
    let mut inline = build(&request);
    inline
        .sdk
        .as_mut()
        .unwrap()
        .env
        .record_question(moment, &question, answer);
    assert_eq!(inline.step().unwrap(), Step::Continued);
    let response = restored.guest_slice(RESP_GPA as u64, HC_PAGE).unwrap();
    assert_eq!(decode(response).unwrap().0.seq, 91);
    assert_eq!(
        response,
        inline.guest_slice(RESP_GPA as u64, HC_PAGE).unwrap()
    );
    assert_eq!(restored.sdk_coverage(), inline.sdk_coverage());
    assert_eq!(restored.event_history(), inline.event_history());
    assert_eq!(restored.event_history().unwrap().count, 2);
    erase_transport(&mut inline);
    erase_transport(&mut restored);
    assert_eq!(restored.state_hash().unwrap(), inline.state_hash().unwrap());
}

#[test]
fn service_transcript_overflow_or_response_failure_cannot_publish_uncommitted_response() {
    let request = frame(ServiceId::Entropy, 1, 9, &1_u32.to_le_bytes());
    for count in [u64::MAX - 1, u64::MAX] {
        let mut vmm = build(&request);
        let history = control_proto::EventHistory {
            count,
            digest: [5; 32],
        };
        vmm.restore_event_history(history).unwrap();
        assert!(vmm.step().is_err());
        assert_eq!(vmm.event_history(), Some(history));
        assert!(
            vmm.guest_slice(RESP_GPA as u64, HC_PAGE)
                .unwrap()
                .iter()
                .all(|&byte| byte == 0)
        );
    }
    let mut vmm = build(&request);
    vmm.set_host_operation_guard(std::sync::Arc::new(|site| {
        if site == HostOperationSite::SdkResponse {
            Err("injected response failure".into())
        } else {
            Ok(())
        }
    }));
    let before = vmm.event_history();
    assert!(vmm.step().is_err());
    assert_eq!(vmm.event_history(), before);
    assert!(
        vmm.guest_slice(RESP_GPA as u64, HC_PAGE)
            .unwrap()
            .iter()
            .all(|&byte| byte == 0)
    );
}
