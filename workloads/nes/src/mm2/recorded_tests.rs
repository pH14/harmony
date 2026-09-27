// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

fn ram(bytes: &[(usize, u8)]) -> [u8; WRAM_SIZE] {
    let mut wram = [0; WRAM_SIZE];
    for &(address, value) in bytes {
        wram[address] = value;
    }
    wram
}

#[test]
fn recorded_action_232_sprite_scratch_is_not_a_menu() {
    let wram = ram(&[(0x4, 3), (0x29, 14), (0xfd, 10), (0xfe, 2)]);
    assert_eq!(wram[4], 3);
    assert_eq!(decode_state(&wram).unwrap().menu, MENU_CLOSED);
}

#[test]
fn recorded_action_3263_menu_bank_selects_the_cursor() {
    let wram = ram(&[(0x4, 0), (0x29, 13), (0xfd, 6), (0xfe, 0)]);
    assert_eq!(decode_state(&wram).unwrap().menu, 6);
}

#[test]
fn recorded_action_4522_continue_meter_is_not_an_encounter() {
    let wram = ram(&[
        (0x29, 14),
        (0x2a, 11),
        (0x2c, 0),
        (0x9a, 255),
        (0xb1, 0),
        (0x6c1, 28),
    ]);
    let state = decode_state(&wram).unwrap();
    assert_eq!(state.boss_health, 28);
    assert!(!crate::mm2::archive::milestones(state, 255).reached_boss);
}

#[test]
fn recorded_action_4486_boobeam_targets_and_ammunition() {
    let wram = ram(&[
        (0x2a, 11),
        (0xa3, 6),
        (0xb1, 2),
        (0x414, 109),
        (0x415, 109),
        (0x416, 109),
        (0x417, 109),
        (0x418, 109),
        (0x419, 87),
        (0x41a, 87),
        (0x41b, 87),
        (0x41c, 87),
        (0x41d, 87),
        (0x434, 195),
        (0x435, 195),
        (0x436, 131),
        (0x437, 131),
        (0x438, 131),
        (0x439, 146),
        (0x43a, 146),
        (0x43b, 146),
        (0x43c, 146),
        (0x43d, 146),
    ]);
    let state = decode_state(&wram).unwrap();
    assert_eq!(state.boobeam_targets, 1023);
    assert_eq!(state.crash_shots, 1);
}

#[test]
fn recorded_action_5401_teleport_keeps_the_refight_stage() {
    let wram = ram(&[(0x2a, 4), (0x2c, 11), (0xb1, 0), (0xb3, 7), (0xbc, 128)]);
    let state = decode_state_for_stage(&wram, WILY5_STAGE).unwrap();
    assert_eq!(wram[STAGE], 4);
    assert_eq!(state.stage, WILY5_STAGE);
    assert_eq!(state.refighting_mask, 128);
    assert_eq!(state.refight_boss, WILY5_REFIGHT_HUB);
}

#[test]
fn recorded_action_6437_machine_refill_does_not_count_as_damage() {
    let wram = ram(&[(0x2a, 12), (0xb1, 4), (0xb3, 12), (0xbc, 255), (0x6c1, 1)]);
    let state = decode_state(&wram).unwrap();
    assert!(state.wily_machine_shell_broken);
    assert_eq!(state.boss_health, 1);
    assert_eq!(state.boss_damage(), 0);
}

#[test]
fn recorded_actions_902_903_preserve_confirmed_damage_in_render_tracking() {
    let before = ram(&[
        (0x20, 20),
        (0x2a, 0),
        (0x10f, 61),
        (0x11f, 0),
        (0x41f, 78),
        (0x43f, 195),
        (0x440, 20),
        (0x6df, 20),
    ]);
    let after = ram(&[
        (0x20, 20),
        (0x2a, 0),
        (0x10f, 61),
        (0x11f, 1),
        (0x41f, 78),
        (0x43f, 195),
        (0x440, 20),
        (0x6df, 18),
    ]);
    assert_eq!(enemy_damage_between(&before, &after), 2);
    let state = decode_state(&before).unwrap();
    let mut tracking = RenderTracking::new(before, state, state.stage);
    assert_eq!(tracking.update(after).unwrap().enemy_damage, 2);
    assert_eq!(tracking.update(after).unwrap().enemy_damage, 2);
}
