// SPDX-License-Identifier: AGPL-3.0-or-later
use consonance_client::session::{SearchSession, SparseSnapshot};
use consonance_wasm::{
    Invocation, WasmSession,
    admission::{AdmittedModule, Profile},
};
use control_proto::{
    Answer as WireAnswer, DecisionId, Moment, Resolution, StopConditions, StopMask, StopReason,
    class_bit,
};
use environment::{
    channel::Answer,
    input_spec::{InputSpec, nominal_factory},
};
use std::path::Path;

fn module() -> AdmittedModule {
    AdmittedModule::new(
        &wat::parse_str(include_str!("../qualification/guest/portable.wat")).unwrap(),
        Profile {
            memory_pages: 1,
            ..Profile::default()
        },
    )
    .unwrap()
}
fn fresh(artifact: &SparseSnapshot) -> WasmSession {
    WasmSession::from_snapshot(module(), artifact, nominal_factory()).unwrap()
}
fn frame(session: &mut WasmSession) {
    let (at, id) = match session
        .run(
            StopConditions {
                deadline: Some(Moment(1_000_000)),
                on: StopMask::NONE.arm(class_bit::BUGGIFY),
            },
            None,
        )
        .unwrap()
    {
        StopReason::Decision { vtime, id, .. } => (vtime, id),
        other => panic!("{other:?}"),
    };
    let stop = session
        .run(
            StopConditions {
                deadline: Some(Moment(1_000_000)),
                on: StopMask::NONE.arm(class_bit::SNAPSHOT_POINT),
            },
            Some(Resolution {
                vtime: at,
                service: 9,
                id: DecisionId(id.0),
                answer: WireAnswer(Answer::Data(vec![5; 4]).encode()),
            }),
        )
        .unwrap();
    assert!(matches!(stop, StopReason::SnapshotPoint { .. }));
}
#[test]
fn portable_continuations_transfer_complete_float_and_pending_state() {
    let mut input = InputSpec::seeded(7);
    input.set_payloads(Some(vec![vec![1, 2, 3, 4]]));
    let mut session =
        WasmSession::new(module(), input, Invocation::new("run"), nominal_factory()).unwrap();
    session
        .run_until(consonance_wasm::meter::Meter::QUANTUM)
        .unwrap();
    let loop_id = session.snapshot().unwrap().0;
    let loop_artifact = session.export_sparse_snapshot(loop_id, None).unwrap();
    let page = &loop_artifact
        .pages()
        .iter()
        .find(|(gfn, _)| *gfn == 0)
        .unwrap()
        .1;
    let count = u32::from_le_bytes(page[320..324].try_into().unwrap());
    assert!(count > 0 && count < 20_000);
    let pending = session
        .run(
            StopConditions {
                deadline: Some(Moment(1_000_000)),
                on: StopMask::NONE.arm(class_bit::BUGGIFY),
            },
            None,
        )
        .unwrap();
    assert!(matches!(pending, StopReason::Decision { .. }));
    let pending_id = session.snapshot().unwrap().0;
    let pending_artifact = session.export_sparse_snapshot(pending_id, None).unwrap();
    let mut loop_fresh = fresh(&loop_artifact);
    frame(&mut loop_fresh);
    let expected = loop_fresh.state_hash().unwrap();
    let final_id = loop_fresh.snapshot().unwrap().0;
    let final_artifact = loop_fresh.export_sparse_snapshot(final_id, None).unwrap();
    let page = &final_artifact
        .pages()
        .iter()
        .find(|(gfn, _)| *gfn == 0)
        .unwrap()
        .1;
    assert_eq!(
        u64::from_le_bytes(page[296..304].try_into().unwrap()),
        0x40b2_3456_789a_bcde
    );
    let mut pending_fresh = fresh(&pending_artifact);
    frame(&mut pending_fresh);
    assert_eq!(pending_fresh.state_hash().unwrap(), expected);
    if let Ok(directory) = std::env::var("WASM_TRANSFER_OUT") {
        std::fs::create_dir_all(&directory).unwrap();
        for (name, artifact) in [("loop", &loop_artifact), ("pending", &pending_artifact)] {
            std::fs::write(
                Path::new(&directory).join(format!("{name}.snapshot")),
                postcard::to_stdvec(artifact).unwrap(),
            )
            .unwrap();
        }
        std::fs::write(
            Path::new(&directory).join("expected.json"),
            serde_json::to_vec(&expected).unwrap(),
        )
        .unwrap();
    }
    if let Ok(directory) = std::env::var("WASM_TRANSFER_IN") {
        for host in std::fs::read_dir(directory).unwrap() {
            let directory = host.unwrap().path();
            if !directory.is_dir() {
                continue;
            }
            let oracle: [u8; 32] =
                serde_json::from_slice(&std::fs::read(directory.join("expected.json")).unwrap())
                    .unwrap();
            assert_eq!(oracle, expected);
            for name in ["loop", "pending"] {
                let artifact = postcard::from_bytes(
                    &std::fs::read(directory.join(format!("{name}.snapshot"))).unwrap(),
                )
                .unwrap();
                let mut restored = fresh(&artifact);
                frame(&mut restored);
                assert_eq!(
                    restored.state_hash().unwrap(),
                    oracle,
                    "{} {name}",
                    directory.display()
                );
            }
        }
    }
    frame(&mut session);
    assert_eq!(session.state_hash().unwrap(), expected);
    session.replay_snapshot(loop_id).unwrap();
    frame(&mut session);
    assert_eq!(session.state_hash().unwrap(), expected);
}
