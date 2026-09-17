// SPDX-License-Identifier: AGPL-3.0-or-later

#![cfg(not(miri))]

use std::{path::PathBuf, time::Instant};

use gb_machine::{
    gambatte::GambatteMachine,
    gb::{A, ButtonChord, DOWN, START, UP},
};
use sha2::{Digest, Sha256};

struct Inputs {
    rom: Vec<u8>,
    core: PathBuf,
    core_sha256: String,
}

fn inputs() -> Inputs {
    let core = PathBuf::from(
        std::env::var_os("HARMONY_GAMBATTE_CORE")
            .expect("HARMONY_GAMBATTE_CORE must name the pinned libretro core"),
    );
    let rom = std::fs::read(
        std::env::var_os("HARMONY_BLUE_ROM").expect("HARMONY_BLUE_ROM must name the Blue image"),
    )
    .expect("the ROM image reads");
    let core_sha256 = format!(
        "{:x}",
        Sha256::digest(std::fs::read(&core).expect("the core image reads"))
    );
    Inputs {
        rom,
        core,
        core_sha256,
    }
}

fn machine(inputs: &Inputs) -> GambatteMachine {
    GambatteMachine::from_rom_bytes(&inputs.rom, &inputs.core, &inputs.core_sha256)
        .expect("the pinned core loads the ROM")
}

fn boot(machine: &mut GambatteMachine) {
    for chord in [
        ButtonChord::new(0, 120),
        ButtonChord::new(0, 120),
        ButtonChord::new(START, 4),
        ButtonChord::new(0, 60),
    ] {
        machine.run_chord(chord).expect("boot chord");
    }
    machine.clear_frames();
}

#[test]
#[ignore = "needs the pinned Gambatte core and the Blue ROM"]
fn a_restored_snapshot_replays_to_identical_ram_and_identical_bytes() {
    let inputs = inputs();
    let mut machine = machine(&inputs);
    boot(&mut machine);

    let tail = [
        ButtonChord::new(A, 8),
        ButtonChord::new(DOWN, 16),
        ButtonChord::new(0, 30),
        ButtonChord::new(UP, 12),
    ];

    let root = machine.snapshot().expect("root snapshot");
    machine.restore(root).expect("restore root");
    machine.run_chords(&tail).expect("first pass");
    let first_ram = machine.read_wram().expect("first RAM");
    let first_end = machine.snapshot().expect("first endpoint");
    let first_bytes = machine.take_snapshot(first_end).expect("first bytes");

    machine.restore(root).expect("restore root again");
    machine.run_chords(&tail).expect("second pass");
    let second_ram = machine.read_wram().expect("second RAM");
    let second_end = machine.snapshot().expect("second endpoint");
    let second_bytes = machine.take_snapshot(second_end).expect("second bytes");

    assert_eq!(first_ram, second_ram, "work RAM must replay identically");
    assert_eq!(
        first_bytes, second_bytes,
        "serialized state must replay identically"
    );

    machine.restore(root).expect("restore root for the split");
    machine.run_chords(&tail[..2]).expect("first half");
    let middle = machine.snapshot().expect("middle snapshot");
    machine.restore(middle).expect("restore middle");
    machine.run_chords(&tail[2..]).expect("second half");
    assert_eq!(
        machine.read_wram().expect("split RAM"),
        first_ram,
        "a split suffix must reach the endpoint the whole suffix reached"
    );
}

#[test]
#[ignore = "needs the pinned Gambatte core and the Blue ROM"]
fn a_snapshot_crosses_machines_and_a_foreign_core_identity_is_refused() {
    let inputs = inputs();
    let mut first = machine(&inputs);
    boot(&mut first);
    first
        .run_chord(ButtonChord::new(A, 20))
        .expect("advance the first machine");
    let snap = first.snapshot().expect("snapshot");
    let bytes = first.take_snapshot(snap).expect("snapshot bytes");
    let expected = first.read_wram().expect("first RAM");

    let mut second = machine(&inputs);
    let imported = second.import_snapshot(&bytes);
    second.restore(imported).expect("cross-machine restore");
    assert_eq!(
        second.read_wram().expect("second RAM"),
        expected,
        "a snapshot must restore the same work RAM in an independent core"
    );

    second
        .run_chord(ButtonChord::new(DOWN, 24))
        .expect("advance the second machine");
    let after_second = second.read_wram().expect("second advanced RAM");
    first
        .run_chord(ButtonChord::new(DOWN, 24))
        .expect("advance the first machine");
    assert_eq!(
        first.read_wram().expect("first advanced RAM"),
        after_second,
        "two cores must agree on what the same action does to the same state"
    );

    let mut wrong = bytes.clone();
    wrong[8] ^= 0xff;
    let imported = second.import_snapshot(&wrong);
    assert!(
        second.restore(imported).is_err(),
        "a snapshot naming another core revision must be refused"
    );
}

#[test]
#[ignore = "needs the pinned Gambatte core and the Blue ROM"]
#[allow(clippy::disallowed_methods)]
fn headless_frame_rate() {
    let inputs = inputs();
    let mut machine = machine(&inputs);
    boot(&mut machine);
    let chords = 400;
    let hold = 30;
    let started = Instant::now();
    for _ in 0..chords {
        machine
            .run_chord(ButtonChord::new(0, hold))
            .expect("timed chord");
        machine.clear_frames();
    }
    let elapsed = started.elapsed();
    let frames = f64::from(chords * u32::from(hold));
    println!(
        "gambatte headless: {frames} frames in {:.3} s = {:.0} frames per second, \
         {:.0} actions per second at {hold} frames each",
        elapsed.as_secs_f64(),
        frames / elapsed.as_secs_f64(),
        f64::from(chords) / elapsed.as_secs_f64()
    );
}
