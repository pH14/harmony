// SPDX-License-Identifier: AGPL-3.0-or-later
use consonance_client::session::SearchSession;
use consonance_wasm::{
    Invocation, WasmSession,
    admission::{AdmittedModule, Profile},
};
use control_proto::{
    Answer as WireAnswer, DecisionId, Moment, Resolution, StopConditions, StopMask, StopReason,
    class_bit,
};
use environment::{
    channel::{Answer, Effect},
    input_spec::{InputSpec, ServiceConfig, nominal_factory},
};

fn session(source: &str, payloads: Option<Vec<Vec<u8>>>) -> WasmSession {
    let profile = Profile {
        memory_pages: 1,
        ..Profile::default()
    };
    let module = AdmittedModule::new(&wat::parse_str(source).unwrap(), profile).unwrap();
    let mut input = InputSpec::seeded(7);
    input.set_payloads(payloads);
    WasmSession::new(module, input, Invocation::new("run"), nominal_factory()).unwrap()
}
const DECISION: &str = r#"(module
 (import "harmony_v1" "request" (func $request (param i32 i32 i32 i32 i32) (result i32)))
 (memory (export "memory") 1 1)
 (data (i32.const 0) "\09\00\07\00\00\00\00\00\00\00\aa\bb\cc\dd")
 (data (i32.const 64) "\04\00\00\00")
 (data (i32.const 128) "\00\00\00\04")
 (data (i32.const 160) "\02\00\00\04\01\00\00\00\01\00\00\00\50\00\00\00\00\00\00\00\08\00\00\00\00\00\00\00")
 (func (export "run")
  i32.const 393219 i32.const 0 i32.const 14 i32.const 32 i32.const 5 call $request drop
  i32.const 131073 i32.const 64 i32.const 4 i32.const 80 i32.const 4 call $request drop
  i32.const 524289 i32.const 64 i32.const 4 i32.const 84 i32.const 4 call $request drop
  i32.const 262145 i32.const 160 i32.const 28 i32.const 0 i32.const 0 call $request drop
  i32.const 262145 i32.const 128 i32.const 4 i32.const 0 i32.const 0 call $request drop
  loop br 0 end))"#;
fn decision_stop(session: &mut WasmSession) -> (u64, u64) {
    match session
        .run(
            StopConditions {
                deadline: Some(Moment(10000)),
                on: StopMask::NONE.arm(class_bit::BUGGIFY),
            },
            None,
        )
        .unwrap()
    {
        StopReason::Decision { vtime, id, .. } => (vtime.0, id.0),
        other => panic!("{other:?}"),
    }
}
fn resolve(at: u64, id: u64, answer: Answer) -> Resolution {
    Resolution {
        vtime: Moment(at),
        service: 9,
        id: DecisionId(id),
        answer: WireAnswer(answer.encode()),
    }
}
fn frame() -> StopConditions {
    StopConditions {
        deadline: Some(Moment(10000)),
        on: StopMask::NONE.arm(class_bit::SNAPSHOT_POINT),
    }
}
#[test]
fn decision_validation_replay_and_import_completion_are_atomic() {
    let mut guest = session(DECISION, Some(vec![vec![1, 2, 3, 4]]));
    let (at, id) = decision_stop(&mut guest);
    let hash = guest.state_hash().unwrap();
    let checkpoint = guest.snapshot().unwrap().0;
    let wrong = resolve(at + 1, id, Answer::Nominal);
    assert!(guest.run(frame(), Some(wrong)).is_err());
    assert_eq!(guest.state_hash().unwrap(), hash);
    assert!(
        guest
            .run(frame(), Some(resolve(at, id, Answer::Data(vec![5; 5]))))
            .is_err()
    );
    assert_eq!(guest.state_hash().unwrap(), hash);
    assert!(matches!(
        guest
            .run(frame(), Some(resolve(at, id, Answer::Data(vec![5; 4]))))
            .unwrap(),
        StopReason::SnapshotPoint { .. }
    ));
    assert_eq!(guest.sdk_events().unwrap().len(), 2);
    let observation = guest.read_observation(1, 0, 8).unwrap();
    assert_eq!(&observation[4..], [1, 2, 3, 4]);
    let completed = guest.state_hash().unwrap();
    let completed_checkpoint = guest.snapshot().unwrap().0;
    guest.replay_snapshot(checkpoint).unwrap();
    assert_eq!(guest.state_hash().unwrap(), hash);
    guest
        .run(frame(), Some(resolve(at, id, Answer::Data(vec![5; 4]))))
        .unwrap();
    assert_eq!(guest.state_hash().unwrap(), completed);
    guest.replay_snapshot(completed_checkpoint).unwrap();
    guest.run_until(4096).unwrap();
    assert_eq!(guest.sdk_events().unwrap().len(), 2);
    guest.replay_snapshot(completed_checkpoint).unwrap();
    assert_eq!(guest.state_hash().unwrap(), completed);
}
#[test]
fn payload_branch_preserves_entropy_and_siblings_and_rejects_effects_before_restore() {
    let mut guest = session(DECISION, Some(vec![vec![1, 2, 3, 4]]));
    let (at, id) = decision_stop(&mut guest);
    let checkpoint = guest.snapshot().unwrap().0;
    let before = guest.state_hash().unwrap();
    assert!(
        guest
            .branch_with_service(
                checkpoint,
                ServiceConfig::default(),
                vec![],
                vec![(
                    0,
                    Effect::WriteMemory {
                        gpa: 0,
                        bytes: vec![1]
                    }
                )]
            )
            .is_err()
    );
    assert_eq!(guest.state_hash().unwrap(), before);
    let mut config = ServiceConfig::default();
    config.identity.push(9);
    assert!(
        guest
            .branch_with_service(checkpoint, config, vec![], vec![])
            .is_err()
    );
    assert_eq!(guest.state_hash().unwrap(), before);
    guest
        .run(frame(), Some(resolve(at, id, Answer::Nominal)))
        .unwrap();
    let left = guest.read_observation(1, 0, 8).unwrap();
    guest
        .branch_payloads(checkpoint, vec![vec![9, 8, 7, 6]])
        .unwrap();
    guest
        .run(frame(), Some(resolve(at, id, Answer::Nominal)))
        .unwrap();
    let right = guest.read_observation(1, 0, 8).unwrap();
    assert_eq!(&left[..4], &right[..4]);
    assert_eq!(&right[4..], [9, 8, 7, 6]);
    guest.replay_snapshot(checkpoint).unwrap();
    guest
        .run(frame(), Some(resolve(at, id, Answer::Nominal)))
        .unwrap();
    assert_eq!(guest.read_observation(1, 0, 8).unwrap(), left);
}
#[test]
fn cancellation_cannot_be_rewound_by_snapshot_or_branch() {
    let mut guest = session(
        "(module (memory 1 1) (func (export \"run\") loop br 0 end))",
        None,
    );
    let checkpoint = guest.snapshot().unwrap().0;
    guest.run_until(1024).unwrap();
    guest.cancellation().cancel();
    assert!(guest.run_until(2048).is_err());
    assert!(guest.replay_snapshot(checkpoint).is_err());
    assert!(guest.branch_payloads(checkpoint, vec![]).is_err());
    assert!(guest.snapshot().is_err());
    assert!(guest.abandoned());
}
#[test]
fn request_partitioning_does_not_change_state() {
    let source = "(module (memory 1 1) (global $x (mut i32) (i32.const 0)) (func (export \"run\") loop global.get $x i32.const 1 i32.add global.set $x br 0 end))";
    let mut one = session(source, None);
    let mut many = session(source, None);
    one.run_until(8193).unwrap();
    for deadline in [1, 1025, 4097, 8193] {
        many.run_until(deadline).unwrap();
    }
    assert_eq!(
        one.current_moment().unwrap(),
        many.current_moment().unwrap()
    );
    assert_eq!(one.state_hash().unwrap(), many.state_hash().unwrap());
}
#[test]
fn observation_revocation_and_assertion_are_captured_once() {
    let source = r#"(module
      (import "harmony_v1" "request" (func $r (param i32 i32 i32 i32 i32) (result i32)))
      (memory 1 1)
      (data (i32.const 0) "\02\00\00\04\01\00\00\00\02\00\00\00\00\01\00\00\00\00\00\00\04\00\00\00\00\00\00\00")
      (data (i32.const 32) "\02\00\00\04\01\00\00\00\02\00\00\00\00\00\00\00\00\00\00\00\00\00\00\00\00\00\00\00")
      (data (i32.const 64) "\05\00\00\01\01\03\00bad")
      (func (export "run")
       i32.const 262145 i32.const 0 i32.const 28 i32.const 0 i32.const 0 call $r drop
       i32.const 262145 i32.const 32 i32.const 28 i32.const 0 i32.const 0 call $r drop
       i32.const 262145 i32.const 64 i32.const 10 i32.const 0 i32.const 0 call $r drop
       loop br 0 end))"#;
    let mut guest = session(source, None);
    assert!(
        matches!(guest.run_until(10000).unwrap(), StopReason::Assertion { ev, .. } if ev.id == 5 && ev.data == b"bad")
    );
    assert!(guest.read_observation(2, 0, 4).is_err());
    assert_eq!(guest.sdk_events().unwrap().len(), 3);
    let checkpoint = guest.snapshot().unwrap().0;
    guest.run_until(10000).unwrap();
    assert_eq!(guest.sdk_events().unwrap().len(), 3);
    guest.replay_snapshot(checkpoint).unwrap();
    assert_eq!(guest.sdk_events().unwrap().len(), 3);
}
#[test]
fn closed_wasi_console_errors_and_replay_are_explicit() {
    let source = r#"(module
      (import "wasi_snapshot_preview1" "fd_write" (func $write (param i32 i32 i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "fd_close" (func $close (param i32) (result i32)))
      (import "wasi_snapshot_preview1" "fd_seek" (func $seek (param i32 i64 i32 i32) (result i32)))
      (memory 1 1)
      (data (i32.const 0) "\20\00\00\00\03\00\00\00") (data (i32.const 32) "hey")
      (func (export "run")
       i32.const 1 i32.const 0 i32.const 1 i32.const 16 call $write drop
       i32.const 1 i64.const 0 i32.const 0 i32.const 24 call $seek i32.const 70 i32.ne if unreachable end
       i32.const 2 i32.const 65535 i32.const 1 i32.const 16 call $write i32.const 21 i32.ne if unreachable end
       i32.const 1 call $close drop
       i32.const 1 i32.const 0 i32.const 1 i32.const 16 call $write i32.const 8 i32.ne if unreachable end
       i32.const 1 call $close i32.const 8 i32.ne if unreachable end))"#;
    let mut guest = session(source, None);
    let setup = guest.setup_handle().0;
    assert!(matches!(
        guest.run_until(10000).unwrap(),
        StopReason::Quiescent { .. }
    ));
    assert_eq!(guest.console_tail().unwrap(), b"hey");
    let hash = guest.state_hash().unwrap();
    guest.replay_snapshot(setup).unwrap();
    guest.run_until(10000).unwrap();
    assert_eq!(guest.state_hash().unwrap(), hash);
    assert_eq!(guest.console_tail().unwrap(), b"hey");
}

#[test]
fn entropy_and_coverage_answers_match_the_shared_sdk() {
    let source = r#"(module
      (import "harmony_v1" "request" (func $r (param i32 i32 i32 i32 i32) (result i32)))
      (memory 1 1)
      (data (i32.const 0) "\08\00\00\00")
      (data (i32.const 32) "\01\00\00\00\01\00\00\00\00\00\00\00\03\00\00\00")
      (data (i32.const 160) "\02\00\00\04\01\00\00\00\01\00\00\00\50\00\00\00\00\00\00\00\14\00\00\00\00\00\00\00")
      (func (export "run")
       i32.const 131073 i32.const 0 i32.const 4 i32.const 80 i32.const 8 call $r drop
       i32.const 393218 i32.const 32 i32.const 16 i32.const 88 i32.const 12 call $r drop
       i32.const 393218 i32.const 32 i32.const 16 i32.const 88 i32.const 12 call $r i32.const -1 i32.ne if unreachable end
       i32.const 262145 i32.const 160 i32.const 28 i32.const 0 i32.const 0 call $r drop))"#;
    let mut guest = session(source, None);
    assert!(matches!(
        guest.run_until(10000).unwrap(),
        StopReason::Quiescent { .. }
    ));
    let actual = guest.read_observation(1, 0, 20).unwrap();
    use environment::{
        channel::{Question, RecordedEnv, ServiceResponse},
        sdk,
    };
    let mut expected = RecordedEnv::nominal(7);
    let ServiceResponse::Answered(Answer::Data(entropy)) =
        expected.decide(&Question::entropy(8).unwrap()).unwrap()
    else {
        unreachable!()
    };
    let coverage = sdk::decide_coverage(
        &mut expected,
        &mut std::collections::BTreeMap::new(),
        0,
        1,
        1,
        3,
    )
    .unwrap();
    assert_eq!(&actual[..8], entropy);
    assert_eq!(&actual[8..], coverage.encode());
}

#[test]
fn malformed_guest_requests_return_deterministic_protocol_errors() {
    let source = r#"(module
      (import "harmony_v1" "request" (func $r (param i32 i32 i32 i32 i32) (result i32)))
      (memory 1 1)
      (func (export "run")
       i32.const 393219 i32.const 65535 i32.const 14 i32.const 0 i32.const 5 call $r i32.const -4 i32.ne if unreachable end
       i32.const 393219 i32.const 0 i32.const 9 i32.const 32 i32.const 5 call $r i32.const -1 i32.ne if unreachable end
       i32.const 131073 i32.const 0 i32.const 3 i32.const 32 i32.const 8 call $r i32.const -1 i32.ne if unreachable end
       i32.const 458753 i32.const 0 i32.const 0 i32.const 0 i32.const 0 call $r i32.const -2 i32.ne if unreachable end))"#;
    let mut guest = session(source, None);
    assert!(matches!(
        guest.run_until(10000).unwrap(),
        StopReason::Quiescent { .. }
    ));
    assert!(guest.sdk_events().unwrap().is_empty());
    assert!(guest.console_tail().unwrap().is_empty());
    assert_eq!(guest.telemetry_counters()[0].1, 4);
}
