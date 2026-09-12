// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::ram::WORK_RAM_LEN;

pub const RETRO_DEVICE_JOYPAD: u32 = 1;
pub const RETRO_MEMORY_SYSTEM_RAM: u32 = 2;
pub const RETRO_ENVIRONMENT_GET_CAN_DUPE: u32 = 3;
pub const RETRO_ENVIRONMENT_SET_PIXEL_FORMAT: u32 = 10;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EnvResponse {
    AcceptPixelFormat,
    CanDupe,
    Unsupported,
}

pub fn env_response(cmd: u32) -> EnvResponse {
    match cmd {
        RETRO_ENVIRONMENT_SET_PIXEL_FORMAT => EnvResponse::AcceptPixelFormat,
        RETRO_ENVIRONMENT_GET_CAN_DUPE => EnvResponse::CanDupe,
        _ => EnvResponse::Unsupported,
    }
}

pub fn joypad_bit(id: u32) -> Option<u8> {
    Some(match id {
        8 => 0,
        0 => 1,
        2 => 2,
        3 => 3,
        4 => 4,
        5 => 5,
        6 => 6,
        7 => 7,
        _ => return None,
    })
}

pub fn input_state_response(joypad: u8, port: u32, device: u32, id: u32) -> i16 {
    if port != 0 || device != RETRO_DEVICE_JOYPAD {
        return 0;
    }
    match joypad_bit(id) {
        Some(bit) => i16::from((joypad >> bit) & 1),
        None => 0,
    }
}

pub fn copy_work_ram(core_ram: &[u8], out: &mut [u8]) -> bool {
    if out.len() < WORK_RAM_LEN || core_ram.is_empty() {
        return false;
    }
    let n = core_ram.len().min(WORK_RAM_LEN);
    out[..n].copy_from_slice(&core_ram[..n]);
    out[n..WORK_RAM_LEN].fill(0);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chord::joypad;

    #[test]
    fn env_responses_cover_exactly_the_two_supported_commands() {
        assert_eq!(env_response(10), EnvResponse::AcceptPixelFormat);
        assert_eq!(env_response(3), EnvResponse::CanDupe);
        for cmd in [0u32, 1, 2, 4, 9, 11, 27, 31, 65_581] {
            assert_eq!(env_response(cmd), EnvResponse::Unsupported, "cmd {cmd}");
        }
    }

    #[test]
    fn joypad_bits_match_the_nes_shift_order_masks() {
        let cases = [
            (8u32, joypad::A),
            (0, joypad::B),
            (2, joypad::SELECT),
            (3, joypad::START),
            (4, joypad::UP),
            (5, joypad::DOWN),
            (6, joypad::LEFT),
            (7, joypad::RIGHT),
        ];
        for (id, mask) in cases {
            let bit = joypad_bit(id).unwrap();
            assert_eq!(1u8 << bit, mask, "libretro id {id}");
        }
        assert_eq!(joypad_bit(1), None);
        assert_eq!(joypad_bit(9), None);
    }

    #[test]
    fn input_state_reports_only_port_zero_joypad() {
        let byte = joypad::RIGHT | joypad::A;
        assert_eq!(input_state_response(byte, 0, RETRO_DEVICE_JOYPAD, 7), 1);
        assert_eq!(input_state_response(byte, 0, RETRO_DEVICE_JOYPAD, 8), 1);
        assert_eq!(input_state_response(byte, 0, RETRO_DEVICE_JOYPAD, 0), 0);
        assert_eq!(input_state_response(byte, 1, RETRO_DEVICE_JOYPAD, 7), 0);
        assert_eq!(input_state_response(byte, 0, 2, 7), 0);
        assert_eq!(input_state_response(byte, 0, RETRO_DEVICE_JOYPAD, 42), 0);
    }

    #[test]
    fn copy_work_ram_clamps_and_zero_fills() {
        let src = vec![0xABu8; 100];
        let mut out = vec![0xFFu8; WORK_RAM_LEN + 4];
        assert!(copy_work_ram(&src, &mut out));
        assert!(out[..100].iter().all(|&b| b == 0xAB));
        assert!(out[100..WORK_RAM_LEN].iter().all(|&b| b == 0));
        assert!(
            out[WORK_RAM_LEN..].iter().all(|&b| b == 0xFF),
            "past the region untouched"
        );

        let src = vec![0x11u8; WORK_RAM_LEN + 999];
        assert!(copy_work_ram(&src, &mut out));
        assert!(out[..WORK_RAM_LEN].iter().all(|&b| b == 0x11));

        assert!(!copy_work_ram(&src, &mut vec![0u8; WORK_RAM_LEN - 1]));
        assert!(!copy_work_ram(&[], &mut out));
    }
}
