// SPDX-License-Identifier: AGPL-3.0-or-later
//! Raw motion bytes for retrospective diagnostics, never archive identity.

use serde::Serialize;

/// Pinned disassembly 4270d57f names these bytes. Speeds retain their raw
/// two's-complement representation; the remaining bytes are not assumed to
/// be independent physical features or complete motion state.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct Kinematics {
    pub direction: u8,
    pub vertical_speed: u8,
    pub horizontal_speed: u8,
    pub jump_displacement: u8,
    pub gravity: u8,
    pub horizontal_acceleration: u8,
    pub horizontal_speed_maximum: u8,
}

impl Kinematics {
    /// Encode the opaque tuple (raw facing, horizontal sign, vertical sign).
    /// Preserve every raw facing byte without imposing a preferred direction.
    #[must_use]
    pub fn coarse_context(self) -> u16 {
        fn sign_class(byte: u8) -> u16 {
            if byte == 0 {
                1
            } else if byte < 128 {
                2
            } else {
                0
            }
        }
        u16::from(self.direction) * 9
            + sign_class(self.horizontal_speed) * 3
            + sign_class(self.vertical_speed)
    }
}

pub(crate) fn decode(wram: &[u8; 2048]) -> Kinematics {
    Kinematics {
        direction: wram[0x4d],
        vertical_speed: wram[0x308],
        horizontal_speed: wram[0x309],
        jump_displacement: wram[0x30f],
        gravity: wram[0x314],
        horizontal_acceleration: wram[0x315],
        horizontal_speed_maximum: wram[0x316],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn motion_probe_preserves_signed_raw_bytes_and_distinct_accumulators() {
        let mut wram = [0; 2048];
        for (address, value) in [
            (0x4d, 1),
            (0x308, 0xfa),
            (0x309, 0xfe),
            (0x30f, 11),
            (0x314, 12),
            (0x315, 13),
            (0x316, 14),
        ] {
            wram[address] = value;
        }
        assert_eq!(
            decode(&wram),
            Kinematics {
                direction: 1,
                vertical_speed: 0xfa,
                horizontal_speed: 0xfe,
                jump_displacement: 11,
                gravity: 12,
                horizontal_acceleration: 13,
                horizontal_speed_maximum: 14,
            }
        );
        wram[0x309] = 0;
        assert_eq!(decode(&wram).horizontal_speed, 0);
        assert_eq!(decode(&wram).vertical_speed, 0xfa);
    }

    #[test]
    fn coarse_context_preserves_facing_and_sign_without_ranking_magnitudes() {
        let mut wram = [0; 2048];
        let mut contexts = std::collections::BTreeSet::new();
        for facing in 0..=u8::MAX {
            wram[0x4d] = facing;
            for x in [0xff, 0, 1] {
                wram[0x309] = x;
                for y in [0xff, 0, 1] {
                    wram[0x308] = y;
                    assert!(contexts.insert(decode(&wram).coarse_context()));
                }
            }
        }
        assert_eq!(contexts.len(), 256 * 9);
        wram[0x309] = 2;
        wram[0x308] = 0xfe;
        let before = decode(&wram).coarse_context();
        wram[0x309] = 127;
        wram[0x308] = 128;
        assert_eq!(before, decode(&wram).coarse_context());
    }
}
