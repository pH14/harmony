// SPDX-License-Identifier: AGPL-3.0-or-later
//! The projector driven end-to-end against the in-crate mock server with the
//! deterministic stamp renderer (task 87 gate 1): a scripted clip round-trips to
//! the right frames, header corruption is a hard error, dropped sessions recover,
//! chunked reads reassemble, and reads are hash-neutral — all with no ROM and no
//! core.

use environment::{EnvCodec, EnvSpec, FaultPolicy};
use film::{
    BillboardScenario, ClipSelect, Corruption, FilmError, FilmPlan, FrameRenderer, FrameTick,
    MockBillboardServer, MomentRef, Session, StampRenderer, blake3_hex, contact_sheet,
    film as project, write_ppm,
};

/// A fault-free, genesis-complete reproducer for the tests.
fn reproducer() -> EnvSpec {
    EnvCodec::seeded(0x51E_F11B, FaultPolicy::none())
}

/// A frame clock of `n` frames, moments spaced 100 apart from 1000, with a frame
/// gap after the third so the derivation is exercised over non-contiguous frame
/// counters (task 87 gate 1: "frame clocks with gaps").
fn clock(n: u32) -> Vec<FrameTick> {
    (0..n)
        .map(|i| FrameTick {
            frame: if i < 3 { i } else { i + 5 }, // a gap: …2, 8, 9, 10…
            moment: 1000 + u64::from(i) * 100,
        })
        .collect()
}

/// Connect a session over a scenario and derive an all-frames plan.
fn setup(
    scenario: BillboardScenario,
    read_cap: u32,
) -> (Session<MockBillboardServer>, FilmPlan, EnvSpec) {
    let ticks = scenario.ticks.clone();
    let window = scenario.window();
    let plan = FilmPlan::derive(&ticks, window, ClipSelect::All, None, read_cap).unwrap();
    let session = Session::connect(MockBillboardServer::boot(scenario)).unwrap();
    (session, plan, reproducer())
}

#[test]
fn three_frame_clip_round_trips_to_three_correct_frames() {
    let scenario = BillboardScenario::new(0x10_0000, clock(3));
    let (mut session, plan, repro) = setup(scenario, 1 << 16);

    let bundle = project(&mut session, &repro, &plan).unwrap();
    assert_eq!(bundle.len(), 3);
    // Every capture's header frame matches the frame-clock frame it was addressed
    // by (the alignment invariant), and the moment matches the plan.
    for (capture, shot) in bundle.frames.iter().zip(&plan.frames) {
        assert_eq!(capture.frame, shot.frame);
        assert_eq!(capture.header.frame, shot.frame);
        assert_eq!(capture.moment, shot.moment);
    }

    // Render each capture → three distinct, correctly-sized frames.
    let mut renderer = StampRenderer::default();
    let frames: Vec<_> = bundle
        .frames
        .iter()
        .map(|c| renderer.render(c).unwrap())
        .collect();
    assert_eq!(frames.len(), 3);
    assert_ne!(frames[0], frames[1]);
    assert_ne!(frames[1], frames[2]);
    // PPM + contact sheet produce, and hashes are stable.
    let sheet = contact_sheet(&frames, 3, [0, 0, 0]).unwrap();
    let h = blake3_hex(&write_ppm(&sheet));
    assert_eq!(h, blake3_hex(&write_ppm(&sheet)));
}

#[test]
fn render_of_a_bundle_is_deterministic() {
    let scenario = BillboardScenario::new(0x10_0000, clock(4));
    let (mut session, plan, repro) = setup(scenario, 1 << 16);
    let bundle = project(&mut session, &repro, &plan).unwrap();

    let render_all = |bundle: &film::CaptureBundle| -> Vec<u8> {
        let mut r = StampRenderer::default();
        let frames: Vec<_> = bundle.frames.iter().map(|c| r.render(c).unwrap()).collect();
        write_ppm(&contact_sheet(&frames, 2, [0, 0, 0]).unwrap())
    };
    // The same capture bundle rendered twice → byte-identical (the box gate's
    // render-determinism claim, at the fake-renderer level).
    assert_eq!(render_all(&bundle), render_all(&bundle));
}

#[test]
fn header_frame_mismatch_is_a_hard_error() {
    let scenario = BillboardScenario::new(0x10_0000, clock(3));
    let ticks = scenario.ticks.clone();
    let window = scenario.window();
    let plan = FilmPlan::derive(&ticks, window, ClipSelect::All, None, 1 << 16).unwrap();
    let server = MockBillboardServer::boot(scenario).with_corruption(Corruption::WrongFrame);
    let mut session = Session::connect(server).unwrap();

    let err = project(&mut session, &reproducer(), &plan).unwrap_err();
    assert!(
        matches!(err, FilmError::Header { .. }),
        "expected a hard header error, got {err:?}"
    );
}

#[test]
fn bad_magic_is_a_hard_error() {
    let scenario = BillboardScenario::new(0x10_0000, clock(2));
    let ticks = scenario.ticks.clone();
    let window = scenario.window();
    let plan = FilmPlan::derive(&ticks, window, ClipSelect::All, None, 1 << 16).unwrap();
    let server = MockBillboardServer::boot(scenario).with_corruption(Corruption::BadMagic);
    let mut session = Session::connect(server).unwrap();

    assert!(matches!(
        project(&mut session, &reproducer(), &plan).unwrap_err(),
        FilmError::Header { .. }
    ));
}

#[test]
fn recovers_from_a_dropped_session() {
    // Two injected read drops < the retry budget → the clip still films
    // correctly (re-materialize at the failed frame and continue).
    let scenario = BillboardScenario::new(0x10_0000, clock(3));
    let ticks = scenario.ticks.clone();
    let window = scenario.window();
    let plan = FilmPlan::derive(&ticks, window, ClipSelect::All, None, 1 << 16).unwrap();
    let server = MockBillboardServer::boot(scenario).with_read_drops(2);
    let mut session = Session::connect(server).unwrap();

    let bundle = project(&mut session, &reproducer(), &plan).unwrap();
    assert_eq!(bundle.len(), 3);
    for (capture, shot) in bundle.frames.iter().zip(&plan.frames) {
        assert_eq!(capture.header.frame, shot.frame);
    }
}

#[test]
fn exhausted_drops_report_session_dropped() {
    // More injected drops than the retry budget → a loud SessionDropped, never a
    // silent gap.
    let scenario = BillboardScenario::new(0x10_0000, clock(3));
    let ticks = scenario.ticks.clone();
    let window = scenario.window();
    let plan = FilmPlan::derive(&ticks, window, ClipSelect::All, None, 1 << 16).unwrap();
    let server = MockBillboardServer::boot(scenario).with_read_drops(50);
    let mut session = Session::connect(server).unwrap();

    assert!(matches!(
        project(&mut session, &reproducer(), &plan).unwrap_err(),
        FilmError::SessionDropped { frame: 0, .. }
    ));
}

#[test]
fn chunked_reads_reassemble_the_billboard() {
    // A tiny read cap forces the billboard window into many chunks; the projector
    // must reassemble them into a valid, verifiable header.
    let scenario = BillboardScenario::new(0x10_0000, clock(3));
    let (mut session, plan, repro) = setup(scenario, 100); // 100-byte cap
    assert!(
        plan.read_chunks().len() > 1,
        "the small cap should split the window"
    );
    let bundle = project(&mut session, &repro, &plan).unwrap();
    assert_eq!(bundle.len(), 3);
    for (capture, shot) in bundle.frames.iter().zip(&plan.frames) {
        assert_eq!(capture.header.frame, shot.frame);
        // The reassembled buffer really carries the declared regions.
        assert_eq!(capture.savestate().len(), 64);
        assert_eq!(capture.work_ram().len(), 2048);
    }
}

#[test]
fn billboard_reads_are_hash_neutral() {
    // Observation invariance (task 80 / the one-timeline claim): reading the
    // billboard at a fixed moment does not change the whole-state hash.
    let scenario = BillboardScenario::new(0x10_0000, clock(3));
    let window = scenario.window();
    let ticks = scenario.ticks.clone();
    let plan = FilmPlan::derive(&ticks, window, ClipSelect::All, None, 1 << 16).unwrap();
    let mut session = Session::connect(MockBillboardServer::boot(scenario)).unwrap();

    let mref = MomentRef::new(reproducer(), plan.frames[1].moment);
    let mut mat = session.materialize(&mref).unwrap();
    let before = mat.hash().unwrap();
    for chunk in plan.read_chunks() {
        mat.read(chunk.gpa, chunk.len).unwrap();
    }
    let _ = mat.regs().unwrap();
    let after = mat.hash().unwrap();
    assert_eq!(before, after, "read/regs must be hash-invariant");
}

#[test]
fn a_long_clip_films_end_to_end() {
    // The box-gate shape (≥300 frames) at portable scale — one materialize, then
    // linear runs, every header verified.
    let scenario = BillboardScenario::new(0x10_0000, clock(300));
    let (mut session, plan, repro) = setup(scenario, 1 << 16);
    let bundle = project(&mut session, &repro, &plan).unwrap();
    assert_eq!(bundle.len(), 300);
    // Spot-check the alignment invariant across the whole clip.
    for (capture, shot) in bundle.frames.iter().zip(&plan.frames) {
        assert_eq!(capture.header.frame, shot.frame);
    }
}
