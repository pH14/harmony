// SPDX-License-Identifier: AGPL-3.0-or-later

use std::fmt;

pub mod joypad {
    pub const A: u8 = 1 << 0;
    pub const B: u8 = 1 << 1;
    pub const SELECT: u8 = 1 << 2;
    pub const START: u8 = 1 << 3;
    pub const UP: u8 = 1 << 4;
    pub const DOWN: u8 = 1 << 5;
    pub const LEFT: u8 = 1 << 6;
    pub const RIGHT: u8 = 1 << 7;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Chord {
    pub buttons: u8,
    pub weight: u16,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ChordAlphabet {
    entries: Vec<Chord>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ChordError {
    Empty,
    ZeroWeight { index: usize },
    BadWeightSum { sum: u64 },
    Parse { what: String },
}

impl fmt::Display for ChordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChordError::Empty => write!(f, "chord alphabet is empty"),
            ChordError::ZeroWeight { index } => {
                write!(f, "chord alphabet entry {index} has weight 0")
            }
            ChordError::BadWeightSum { sum } => {
                write!(
                    f,
                    "chord alphabet weights sum to {sum}, must be exactly 256"
                )
            }
            ChordError::Parse { what } => write!(f, "unparseable chord spec: {what:?}"),
        }
    }
}

impl std::error::Error for ChordError {}

impl ChordAlphabet {
    pub fn new(entries: Vec<Chord>) -> Result<Self, ChordError> {
        if entries.is_empty() {
            return Err(ChordError::Empty);
        }
        if let Some(index) = entries.iter().position(|c| c.weight == 0) {
            return Err(ChordError::ZeroWeight { index });
        }
        let sum: u64 = entries.iter().map(|c| u64::from(c.weight)).sum();
        if sum != 256 {
            return Err(ChordError::BadWeightSum { sum });
        }
        Ok(ChordAlphabet { entries })
    }

    pub fn smb_default() -> Self {
        use joypad::{A, B, DOWN, LEFT, RIGHT};
        let entries = vec![
            Chord {
                buttons: RIGHT,
                weight: 56,
            },
            Chord {
                buttons: RIGHT | B,
                weight: 56,
            },
            Chord {
                buttons: RIGHT | A,
                weight: 48,
            },
            Chord {
                buttons: RIGHT | A | B,
                weight: 48,
            },
            Chord {
                buttons: A,
                weight: 16,
            },
            Chord {
                buttons: LEFT,
                weight: 12,
            },
            Chord {
                buttons: DOWN,
                weight: 12,
            },
            Chord {
                buttons: 0,
                weight: 8,
            },
        ];
        ChordAlphabet::new(entries).expect("default alphabet weights sum to 256")
    }

    pub fn decode(&self, byte: u8) -> u8 {
        let mut cursor = u32::from(byte);
        for chord in &self.entries {
            let w = u32::from(chord.weight);
            if cursor < w {
                return chord.buttons;
            }
            cursor -= w;
        }
        0
    }

    pub fn entries(&self) -> &[Chord] {
        &self.entries
    }

    pub fn parse(spec: &str) -> Result<Self, ChordError> {
        let mut entries = Vec::new();
        for part in spec.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let (names, weight) = part.split_once(':').ok_or_else(|| ChordError::Parse {
                what: part.to_string(),
            })?;
            let weight: u16 = weight.trim().parse().map_err(|_| ChordError::Parse {
                what: part.to_string(),
            })?;
            let mut buttons = 0u8;
            for name in names.split('+') {
                buttons |= match name.trim().to_ascii_uppercase().as_str() {
                    "A" => joypad::A,
                    "B" => joypad::B,
                    "SELECT" => joypad::SELECT,
                    "START" => joypad::START,
                    "UP" => joypad::UP,
                    "DOWN" => joypad::DOWN,
                    "LEFT" => joypad::LEFT,
                    "RIGHT" => joypad::RIGHT,
                    "NEUTRAL" => 0,
                    other => {
                        return Err(ChordError::Parse {
                            what: other.to_string(),
                        });
                    }
                };
            }
            entries.push(Chord { buttons, weight });
        }
        ChordAlphabet::new(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_alphabet_weights_sum_to_256() {
        let a = ChordAlphabet::smb_default();
        let sum: u32 = a.entries().iter().map(|c| u32::from(c.weight)).sum();
        assert_eq!(sum, 256);
    }

    #[test]
    fn decode_is_total_and_matches_cumulative_thresholds() {
        let a = ChordAlphabet::smb_default();
        let mut expected = Vec::new();
        for chord in a.entries() {
            for _ in 0..chord.weight {
                expected.push(chord.buttons);
            }
        }
        assert_eq!(expected.len(), 256);
        for byte in 0..=255u8 {
            assert_eq!(a.decode(byte), expected[byte as usize], "byte {byte}");
        }
    }

    #[test]
    fn boundary_bytes_select_the_right_chords() {
        let a = ChordAlphabet::smb_default();
        assert_eq!(a.decode(0), joypad::RIGHT);
        assert_eq!(a.decode(55), joypad::RIGHT);
        assert_eq!(a.decode(56), joypad::RIGHT | joypad::B);
        assert_eq!(a.decode(255), 0);
    }

    #[test]
    fn weight_overflow_is_rejected_not_wrapped() {
        let entries = vec![
            Chord {
                buttons: 0,
                weight: u16::MAX
            };
            65_537
        ];
        assert_eq!(
            ChordAlphabet::new(entries),
            Err(ChordError::BadWeightSum {
                sum: 65_537u64 * 65_535
            })
        );
    }

    #[test]
    fn rejects_bad_weight_sums_and_zero_weights() {
        assert_eq!(
            ChordAlphabet::new(vec![Chord {
                buttons: 0,
                weight: 255
            }]),
            Err(ChordError::BadWeightSum { sum: 255 })
        );
        assert_eq!(
            ChordAlphabet::new(vec![
                Chord {
                    buttons: 0,
                    weight: 0
                },
                Chord {
                    buttons: 1,
                    weight: 256
                },
            ]),
            Err(ChordError::ZeroWeight { index: 0 })
        );
        assert_eq!(ChordAlphabet::new(vec![]), Err(ChordError::Empty));
    }

    #[test]
    fn parses_the_default_spec_string() {
        let parsed = ChordAlphabet::parse(
            "RIGHT:56,RIGHT+B:56,RIGHT+A:48,RIGHT+A+B:48,A:16,LEFT:12,DOWN:12,NEUTRAL:8",
        )
        .unwrap();
        assert_eq!(parsed, ChordAlphabet::smb_default());
    }

    #[test]
    fn parse_rejects_unknown_buttons_and_bad_weights() {
        assert!(matches!(
            ChordAlphabet::parse("FROG:256"),
            Err(ChordError::Parse { .. })
        ));
        assert!(matches!(
            ChordAlphabet::parse("A:abc"),
            Err(ChordError::Parse { .. })
        ));
        assert!(matches!(
            ChordAlphabet::parse("A:1,B:2"),
            Err(ChordError::BadWeightSum { sum: 3 })
        ));
    }
}
