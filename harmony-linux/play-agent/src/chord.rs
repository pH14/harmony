// SPDX-License-Identifier: AGPL-3.0-or-later
//! The weighted chord input policy: one byte of decision entropy per input
//! window, decoded against a weighted alphabet of NES button chords.
//!
//! Per-frame uniform buttons is a known-bad policy (a random walk); the chord
//! window is what makes the entropy stream mean something (task 86 §play-agent).
//! One entropy byte selects a chord by cumulative weight; the chord is then held
//! for the whole `W`-frame window (A held across a window = full jump height).
//! Weights **must sum to exactly 256** so a single byte maps onto the alphabet
//! with no bias and no rejection loop — the decode is a total function of the
//! byte, and the number of entropy bytes drawn per run is a pure function of
//! the frame count (determinism rule 4).
//!
//! The joypad byte's bit layout is the NES hardware controller shift order —
//! the exact layout `film`'s `CoreReplay` replays (`dissonance/film/src/
//! core_replay.rs::joypad_pressed`, a local mirror per conventions rule 2):
//! bit 0 = A, 1 = B, 2 = Select, 3 = Start, 4 = Up, 5 = Down, 6 = Left,
//! 7 = Right.

use std::fmt;

/// NES joypad button masks in hardware controller shift order — the billboard's
/// joypad-byte contract (mirrors `film::core_replay::joypad_pressed`).
pub mod joypad {
    /// A (jump).
    pub const A: u8 = 1 << 0;
    /// B (run / fireball).
    pub const B: u8 = 1 << 1;
    /// Select — excluded from the default alphabet (menu navigation).
    pub const SELECT: u8 = 1 << 2;
    /// Start — excluded from the default alphabet (pausing burns budget).
    pub const START: u8 = 1 << 3;
    /// D-pad up.
    pub const UP: u8 = 1 << 4;
    /// D-pad down (duck / pipe entry).
    pub const DOWN: u8 = 1 << 5;
    /// D-pad left.
    pub const LEFT: u8 = 1 << 6;
    /// D-pad right (the way SMB scrolls).
    pub const RIGHT: u8 = 1 << 7;
}

/// One weighted chord: a joypad byte and its selection weight.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Chord {
    /// The joypad byte (NES shift order, see [`joypad`]).
    pub buttons: u8,
    /// Selection weight; the alphabet's weights sum to exactly 256.
    pub weight: u16,
}

/// The weighted chord alphabet: decodes one entropy byte into a joypad byte by
/// cumulative weight. Weights sum to exactly 256 (checked at construction), so
/// `decode` is total and unbiased over a uniform byte.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ChordAlphabet {
    entries: Vec<Chord>,
}

/// Why a chord alphabet failed to construct or parse.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ChordError {
    /// The alphabet has no entries.
    Empty,
    /// A chord has weight zero (it could never be selected — a config typo).
    ZeroWeight {
        /// Index of the zero-weight entry.
        index: usize,
    },
    /// The weights do not sum to exactly 256.
    BadWeightSum {
        /// The actual sum (u64: a hostile `--alphabet` can carry enough
        /// max-weight entries to wrap a u32 accumulator — round-8 P2).
        sum: u64,
    },
    /// A textual chord spec failed to parse.
    Parse {
        /// The offending fragment.
        what: String,
    },
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
    /// Build an alphabet, validating the exact-256 weight-sum invariant.
    pub fn new(entries: Vec<Chord>) -> Result<Self, ChordError> {
        if entries.is_empty() {
            return Err(ChordError::Empty);
        }
        if let Some(index) = entries.iter().position(|c| c.weight == 0) {
            return Err(ChordError::ZeroWeight { index });
        }
        // u64 accumulation: user-controlled u16 weights (an `--alphabet` can
        // carry tens of thousands of entries) must not wrap a u32 into a
        // spurious 256 — or panic in debug (round-8 P2).
        let sum: u64 = entries.iter().map(|c| u64::from(c.weight)).sum();
        if sum != 256 {
            return Err(ChordError::BadWeightSum { sum });
        }
        Ok(ChordAlphabet { entries })
    }

    /// The default SMB alphabet (task 86 §play-agent): rightward-biased chords
    /// — `RIGHT`, `RIGHT+B` (run), `RIGHT+A` (jump), `RIGHT+A+B` (run-jump),
    /// neutral `A`, `LEFT`, `DOWN` (duck / pipe entry), neutral. `START` and
    /// `SELECT` are excluded (pausing burns budget). Weights are a manifest
    /// parameter; these defaults bias rightward because SMB only scrolls right.
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
        // Statically-valid construction: the literal weights above sum to 256.
        ChordAlphabet::new(entries).expect("default alphabet weights sum to 256")
    }

    /// Decode one entropy byte into a joypad byte by cumulative weight. Total:
    /// because the weights sum to exactly 256, every byte value selects exactly
    /// one chord.
    pub fn decode(&self, byte: u8) -> u8 {
        let mut cursor = u32::from(byte);
        for chord in &self.entries {
            let w = u32::from(chord.weight);
            if cursor < w {
                return chord.buttons;
            }
            cursor -= w;
        }
        // Unreachable by the 256-sum invariant; return neutral rather than
        // panicking on library input (rule 4: never panic on untrusted input).
        0
    }

    /// The alphabet entries (for reports and manifests).
    pub fn entries(&self) -> &[Chord] {
        &self.entries
    }

    /// Parse an alphabet from a manifest string:
    /// `"RIGHT:56,RIGHT+B:56,RIGHT+A:48,RIGHT+A+B:48,A:16,LEFT:12,DOWN:12,NEUTRAL:8"`.
    /// Button names are the [`joypad`] constants plus `NEUTRAL` (no buttons);
    /// chords join names with `+`; weights follow `:` and must sum to 256.
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
        // Walk all 256 byte values and recompute the expected chord by hand.
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
        assert_eq!(a.decode(255), 0); // the last (neutral) chord
    }

    /// Round-8 P2: a weight set big enough to wrap a u32 accumulator is a
    /// clean BadWeightSum with the true u64 sum — never a panic/wrap that
    /// could alias 256.
    #[test]
    fn weight_overflow_is_rejected_not_wrapped() {
        // 65537 × 65535 = 2^32 − 1: the u32-wrapping shape.
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
