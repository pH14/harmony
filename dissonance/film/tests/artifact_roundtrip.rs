// SPDX-License-Identifier: AGPL-3.0-or-later
//! The plan and the capture bundle are **replayable artifacts** (task 87 §1/§2):
//! a `FilmPlan` is the query, serializable and inspectable; a `CaptureBundle` can
//! be rendered later or elsewhere. This pins both round-trips through serde JSON.

use environment::{EnvCodec, FaultPolicy};
use film::{
    BillboardScenario, CaptureBundle, ClipSelect, FilmPlan, FrameRenderer, FrameTick,
    MockBillboardServer, Session, StampRenderer, film as project, write_ppm,
};

fn clock(n: u32) -> Vec<FrameTick> {
    (0..n)
        .map(|i| FrameTick {
            frame: i,
            moment: 500 + u64::from(i) * 50,
        })
        .collect()
}

#[test]
fn film_plan_round_trips_through_json() {
    let ticks = clock(5);
    let window = BillboardScenario::new(0x4000, ticks.clone()).window();
    let plan = FilmPlan::derive(&ticks, window, ClipSelect::All, Some(2), 4096).unwrap();
    let json = serde_json::to_string_pretty(&plan).unwrap();
    let back: FilmPlan = serde_json::from_str(&json).unwrap();
    assert_eq!(plan, back);
}

#[test]
fn capture_bundle_renders_identically_after_a_json_round_trip() {
    let scenario = BillboardScenario::new(0x4000, clock(4));
    let window = scenario.window();
    let ticks = scenario.ticks.clone();
    let plan = FilmPlan::derive(&ticks, window, ClipSelect::All, None, 1 << 16).unwrap();
    let repro = EnvCodec::seeded(7, FaultPolicy::none());
    let mut session = Session::connect(MockBillboardServer::boot(scenario)).unwrap();
    let bundle = project(&mut session, &repro, &plan).unwrap();

    // Persist and reload the bundle (rendered "later or elsewhere").
    let json = serde_json::to_string(&bundle).unwrap();
    let reloaded: CaptureBundle = serde_json::from_str(&json).unwrap();
    assert_eq!(bundle, reloaded);

    // Rendering the reloaded bundle produces byte-identical output.
    let render = |b: &CaptureBundle| -> Vec<Vec<u8>> {
        let mut r = StampRenderer::default();
        b.frames
            .iter()
            .map(|c| write_ppm(&r.render(c).unwrap()))
            .collect()
    };
    assert_eq!(render(&bundle), render(&reloaded));
}

#[test]
fn loaded_bundle_validation_catches_tampering() {
    let scenario = BillboardScenario::new(0x4000, clock(3));
    let window = scenario.window();
    let ticks = scenario.ticks.clone();
    let plan = FilmPlan::derive(&ticks, window, ClipSelect::All, None, 1 << 16).unwrap();
    let repro = EnvCodec::seeded(11, FaultPolicy::none());
    let mut session = Session::connect(MockBillboardServer::boot(scenario)).unwrap();
    let bundle = project(&mut session, &repro, &plan).unwrap();

    // A faithful bundle validates.
    bundle.validate().unwrap();

    // Round-trip through JSON, then tamper the last frame's *stored* header so it
    // disagrees with its bytes: validate must catch the header/bytes mismatch
    // self-describingly (not render garbage). This clip has no frame gap, so the
    // last frame's counter is 2.
    let json = serde_json::to_string(&bundle).unwrap();
    let mut reloaded: CaptureBundle = serde_json::from_str(&json).unwrap();
    reloaded.frames.last_mut().unwrap().header.joypad ^= 0xFF;
    assert!(matches!(
        reloaded.validate().unwrap_err(),
        film::CaptureError::HeaderMismatch { frame: 2 }
    ));

    // Corrupting the bytes' header region (magic) is caught as a parse failure.
    let mut torn: CaptureBundle = serde_json::from_str(&json).unwrap();
    torn.frames[0].bytes[0] ^= 0xFF;
    assert!(matches!(
        torn.validate().unwrap_err(),
        film::CaptureError::Header { frame: 0, .. }
    ));
}
