// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{env, fs, path::Path};

use sha2::{Digest, Sha256};

use crate::{
    metroid::{
        action::MetroidAction,
        archive::MetroidArchiveKey,
        game::Metroid,
        target::{MetroidSnapshot, MetroidTarget},
    },
    nes_backend::packed_state_len,
    target::{ExitKind, Target},
    workload::Shadow,
};

const SAVE_EVERY: usize = 40;
const DETOUR: usize = 6;

fn var(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
}

fn actions(path: &str) -> Vec<MetroidAction> {
    let tape: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    serde_json::from_value(tape["actions"].clone()).unwrap()
}

fn runnable(target: &MetroidTarget<machine::quicknes::QuickNesMachine>) -> bool {
    target.exit_kind() == ExitKind::Ok && !target.is_dead()
}

#[test]
#[ignore = "needs HARMONY_METROID_ROM, HARMONY_QUICKNES_CORE and HARMONY_METROID_TAPES"]
fn the_route_strategy_shadow_agrees_with_the_hand_key_on_replayed_tapes_with_restores() {
    let rom = fs::read(var("HARMONY_METROID_ROM")).unwrap();
    let core = var("HARMONY_QUICKNES_CORE");
    let core_sha256 = format!("{:x}", Sha256::digest(fs::read(&core).unwrap()));
    let mut target =
        MetroidTarget::from_rom_bytes_headless(&rom, Path::new(&core), &core_sha256).unwrap();
    let shadow = Shadow::<Metroid, MetroidArchiveKey>::new(Metroid).unwrap();
    target.set_shadow(shadow.state_hook());
    let genesis = target.snapshot().unwrap();
    target.shadow_current();
    let mut state_bytes = (0_usize, 0_usize, 0_usize);
    for path in var("HARMONY_METROID_TAPES").split(':') {
        let tape = actions(path);
        target.restore(&genesis).unwrap();
        let mut restores = 0_usize;
        let mut index = 0;
        while index < tape.len() && runnable(&target) {
            target.apply(&tape[index]);
            index += 1;
            if index % SAVE_EVERY != 0 || !runnable(&target) {
                target.shadow_current();
                continue;
            }
            let saved = target.snapshot().unwrap();
            target.shadow_current();
            state_bytes = (
                state_bytes.0 + saved.emulator_state_bytes_len(),
                state_bytes.1 + 1,
                packed_state_len(saved.emulator_state()).unwrap(),
            );
            for action in tape.iter().skip(index).take(DETOUR).rev() {
                target.apply(action);
                target.shadow_current();
                if !runnable(&target) {
                    break;
                }
            }
            target.restore(&saved).unwrap();
            restores += 1;
        }
        let report = shadow.report().unwrap();
        eprintln!(
            "metroid tape {path}: {index} of {} actions, {restores} restores, state {:?}; shadow so far {} states, {} keyed, {} distinct pairs, disagreements {:?}, unknown restores {}, unobserved {}",
            tape.len(),
            target.mechanical_state(),
            report.states,
            report.keyed_states,
            report.distinct_pairs,
            report.disagreements,
            report.unknown_restores,
            report.unobserved,
        );
    }
    let mean = state_bytes.0 / state_bytes.1.max(1);
    eprintln!(
        "metroid snapshots: {} saved, mean emulator state {mean} bytes packed from {} bytes, MetroidSnapshot {} bytes, charge {} bytes",
        state_bytes.1,
        state_bytes.2,
        size_of::<MetroidSnapshot>(),
        size_of::<MetroidSnapshot>() + mean,
    );
    let report = shadow.report().unwrap();
    assert_eq!(report.disagreements.total(), 0, "{:?}", report.examples);
    assert_eq!(report.unknown_restores, 0);
    assert_eq!(report.unobserved, 0);
}
