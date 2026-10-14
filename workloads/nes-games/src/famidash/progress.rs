// SPDX-License-Identifier: AGPL-3.0-or-later

pub const LEVEL_COUNT: u8 = 43;
pub const MAIN_LEVEL_COUNT: u8 = 22;

const LEVEL_LENGTHS: [u8; LEVEL_COUNT as usize] = [
    0x90, 0x88, 0x96, 0x87, 0x8c, 0x86, 0x90, 0xa0, 0x84, 0x88, 0xa1, 0x8b, 0x8d, 0x91, 0xb2, 0xa9,
    0xa7, 0x9d, 0xaf, 0xbf, 0x93, 0xb3, 0x66, 0x90, 0x30, 0xa4, 0x90, 0xac, 0xa2, 0x88, 0x79, 0xa5,
    0xba, 0xb0, 0x66, 0x90, 0xb5, 0x69, 0xf0, 0x87, 0x98, 0xa2, 0x8d,
];

pub const LEVEL_NAMES: [&str; LEVEL_COUNT as usize] = [
    "Stereo Madness",
    "Back on Track",
    "Polargeist",
    "Dry Out",
    "Base After Base",
    "Can't Let Go",
    "Jumper",
    "Time Machine",
    "Cycles",
    "xStep",
    "Clutterfunk",
    "Theory of Everything",
    "Electroman Adventures",
    "Clubstep",
    "Electrodynamix",
    "Hexagon Force",
    "Blast Processing",
    "Theory of Everything 2",
    "Geometrical Dominator",
    "Deadlocked",
    "Fingerdash",
    "Dash",
    "Retray",
    "Level Easy",
    "The Challenge",
    "Dorabae Basic4",
    "Dorabae Basic6",
    "Dreamer",
    "Ground to Space",
    "Subzero",
    "Kappaclysm",
    "Sunshine",
    "Revolution",
    "Funnygame Holiday",
    "Lost in the Woods",
    "Bloodbath But No",
    "Rainbow Tylenol",
    "Greif",
    "Extraordinary Excitement",
    "The Lightning Road",
    "The Nightmare",
    "Demon Park",
    "Fire Temple",
];

#[must_use]
pub fn level_length(level: u8) -> Option<u32> {
    LEVEL_LENGTHS
        .get(usize::from(level))
        .map(|&length| u32::from(length))
}

#[must_use]
pub fn percent(level: u8, scroll_x: i32) -> u8 {
    let Some(length) = level_length(level) else {
        return 0;
    };
    let scrolled = u32::try_from(scroll_x.max(0)).unwrap_or(0);
    u8::try_from((scrolled / length).min(99)).unwrap_or(99)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_divides_the_scroll_by_the_level_length_and_stops_at_99() {
        assert_eq!(percent(0, -240), 0);
        assert_eq!(percent(0, 143), 0);
        assert_eq!(percent(0, 144), 1);
        assert_eq!(percent(0, 14_038), 97);
        assert_eq!(percent(0, 20_000), 99);
        assert_eq!(percent(LEVEL_COUNT, 1_000), 0);
    }

    #[test]
    fn every_level_has_a_name_and_a_length() {
        for level in 0..LEVEL_COUNT {
            assert!(level_length(level).is_some_and(|length| length > 0));
            assert!(!LEVEL_NAMES[usize::from(level)].is_empty());
        }
    }
}
