// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    #[default]
    Main,
    B,
    C,
    D,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Menu {
    MainLevels,
    CommunityLevels,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct Group {
    pub id: &'static str,
    pub name: &'static str,
    pub side: Side,
    pub first: u8,
    pub last: u8,
    pub menu: Menu,
}

impl Group {
    #[must_use]
    pub fn contains(&self, side: Side, level: u8) -> bool {
        self.side == side && (self.first..=self.last).contains(&level)
    }
}

pub const GROUPS: [Group; 5] = [
    Group {
        id: "main",
        name: "Main",
        side: Side::Main,
        first: 0,
        last: 21,
        menu: Menu::MainLevels,
    },
    Group {
        id: "community",
        name: "Community",
        side: Side::Main,
        first: 22,
        last: 42,
        menu: Menu::CommunityLevels,
    },
    Group {
        id: "b",
        name: "B-Sides",
        side: Side::B,
        first: 0,
        last: 30,
        menu: Menu::CommunityLevels,
    },
    Group {
        id: "c",
        name: "C-Sides",
        side: Side::C,
        first: 0,
        last: 24,
        menu: Menu::CommunityLevels,
    },
    Group {
        id: "d",
        name: "D-Sides",
        side: Side::D,
        first: 0,
        last: 9,
        menu: Menu::CommunityLevels,
    },
];

const MAIN_NAMES: [&str; 43] = [
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

const MAIN_LENGTHS: [u16; 43] = [
    144, 136, 150, 135, 140, 134, 144, 160, 132, 136, 161, 139, 141, 145, 178, 169, 167, 157, 175,
    191, 147, 179, 102, 144, 48, 164, 144, 172, 162, 136, 121, 165, 186, 176, 102, 144, 181, 105,
    240, 135, 152, 162, 141,
];

const B_NAMES: [&str; 31] = [
    "Sonar",
    "Dark Paradise",
    "Ninox",
    "Look at This Level",
    "Ultiate Destruction",
    "Moonlight",
    "Fofii Fofii Fofii",
    "Select Payment Type",
    "Pyrophoric",
    "Cryogenic",
    "Oceane",
    "Generation Retro",
    "Scarlet Surge",
    "Tiny Tunes",
    "Fire Aura",
    "Rainbow Dust",
    "Factory Time",
    "The Steamworks",
    "Raining Tacos",
    "Dorabae Basic10",
    "Subtle Oddities",
    "Power Trip",
    "Dance of Violins",
    "A Pretty Easy Level",
    "High Life",
    "Infinite Circles",
    "Decode",
    "Death Moon",
    "Clutterfunk 2",
    "Speed Racer",
    "Skeletal Shenanigans",
];

const B_LENGTHS: [u16; 31] = [
    141, 100, 123, 92, 108, 121, 111, 188, 80, 153, 93, 178, 225, 130, 191, 171, 144, 204, 187,
    151, 192, 192, 150, 112, 340, 111, 138, 293, 177, 128, 213,
];

const C_NAMES: [&str; 25] = [
    "Super Cycles",
    "X",
    "Problematic",
    "Sonic Blaster",
    "Motion",
    "Endorphin Rush",
    "Every Madness",
    "Try This GD",
    "Hell",
    "Invisible Light",
    "PG Clubstep",
    "Deadly Clubstep",
    "Thermodynamix",
    "Nine Circles",
    "Fairydust",
    "Stalemate",
    "8o",
    "Hi",
    "Eon",
    "Sonic Wave",
    "Bloodbath",
    "Cataclysm",
    "Aftermath",
    "Every End",
    "Lucky Draw",
];

const C_LENGTHS: [u16; 25] = [
    136, 144, 110, 166, 166, 139, 144, 212, 162, 151, 148, 147, 142, 187, 130, 146, 120, 154, 646,
    251, 213, 125, 124, 732, 137,
];

const D_NAMES: [&str; 10] = [
    "Golden Haze",
    "Demon Cryogenic",
    "Forest Temple",
    "Windy Landscape",
    "Silent Clubstep",
    "Aftercatabath",
    "Shardscapes",
    "Element 111 RG",
    "Slaughterhouse",
    "Kratos",
];

const D_LENGTHS: [u16; 10] = [159, 168, 144, 154, 149, 447, 111, 162, 152, 135];

impl Side {
    pub const ALL: [Self; 4] = [Self::Main, Self::B, Self::C, Self::D];

    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::B => "b",
            Self::C => "c",
            Self::D => "d",
        }
    }

    #[must_use]
    pub fn rom_file(self) -> &'static str {
        match self {
            Self::Main => "famidash.nes",
            Self::B => "famidash-b-sides.nes",
            Self::C => "famidash-c-sides.nes",
            Self::D => "famidash-d-sides.nes",
        }
    }

    #[must_use]
    pub fn rom_sha256(self) -> &'static str {
        match self {
            Self::Main => "549458fc0a82868cdefa3df9398e4d49782e1b06debace846bbd570c803ee43d",
            Self::B => "31a7c539ba2df69fc4f1ff7a87dc562811b389e6d54763b8bf66c74b703c7e28",
            Self::C => "e2886d7c0e1f9e460f424e1e19db8a962264dec86c12572c179cf05a5c4d108d",
            Self::D => "1a2e6e20c1eb5f8bae1b717f456016c87673ba38b1377fa44c24de740616e99f",
        }
    }

    #[must_use]
    pub fn from_rom(rom: &[u8]) -> Option<Self> {
        let sha256 = format!("{:x}", Sha256::digest(rom));
        Self::ALL
            .into_iter()
            .find(|side| side.rom_sha256() == sha256)
    }

    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|side| side.id() == id)
    }

    fn names(self) -> &'static [&'static str] {
        match self {
            Self::Main => &MAIN_NAMES,
            Self::B => &B_NAMES,
            Self::C => &C_NAMES,
            Self::D => &D_NAMES,
        }
    }

    fn lengths(self) -> &'static [u16] {
        match self {
            Self::Main => &MAIN_LENGTHS,
            Self::B => &B_LENGTHS,
            Self::C => &C_LENGTHS,
            Self::D => &D_LENGTHS,
        }
    }

    #[must_use]
    pub fn level_count(self) -> u8 {
        u8::try_from(self.names().len()).unwrap_or(u8::MAX)
    }

    #[must_use]
    pub fn level_name(self, level: u8) -> Option<&'static str> {
        self.names().get(usize::from(level)).copied()
    }

    #[must_use]
    pub fn level_length(self, level: u8) -> Option<u16> {
        self.lengths().get(usize::from(level)).copied()
    }

    #[must_use]
    pub fn group(self, level: u8) -> Option<&'static Group> {
        GROUPS.iter().find(|group| group.contains(self, level))
    }
}

#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
)]
pub struct Stage {
    pub side: Side,
    pub level: u8,
    pub goal: u8,
}

impl Stage {
    pub fn new(side: Side, level: u8, goal: u8) -> Result<Self, String> {
        let group = side
            .group(level)
            .ok_or_else(|| format!("Famidash {} side has no level {level}", side.id()))?;
        if goal < level || !group.contains(side, goal) {
            return Err(format!(
                "Famidash {} level {goal} is outside the {} levels after level {level}",
                side.id(),
                group.name
            ));
        }
        Ok(Self { side, level, goal })
    }

    pub fn single(side: Side, level: u8) -> Result<Self, String> {
        Self::new(side, level, level)
    }

    pub fn through_group(side: Side, level: u8) -> Result<Self, String> {
        let last = side
            .group(level)
            .ok_or_else(|| format!("Famidash {} side has no level {level}", side.id()))?
            .last;
        Self::new(side, level, last)
    }

    #[must_use]
    pub fn group(self) -> &'static Group {
        self.side.group(self.level).unwrap_or(&GROUPS[0])
    }
}

#[must_use]
pub fn percent(side: Side, level: u8, scroll_x: i32) -> u8 {
    let Some(length) = side.level_length(level) else {
        return 0;
    };
    let scrolled = u32::try_from(scroll_x.max(0)).unwrap_or(0);
    u8::try_from((scrolled / u32::from(length)).min(99)).unwrap_or(99)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_divides_the_scroll_by_the_level_length() {
        assert_eq!(percent(Side::Main, 0, 0), 0);
        assert_eq!(percent(Side::Main, 0, 143), 0);
        assert_eq!(percent(Side::Main, 0, 144 * 37), 37);
        assert_eq!(percent(Side::Main, 0, 20_000), 99);
        assert_eq!(percent(Side::Main, 43, 1_000), 0);
        assert_eq!(percent(Side::C, 23, 732 * 98), 98);
        assert_eq!(percent(Side::D, 5, 447 * 50 + 446), 50);
    }

    #[test]
    fn every_level_has_a_name_a_length_and_one_group() {
        let mut total = 0;
        for side in Side::ALL {
            assert_eq!(side.names().len(), side.lengths().len());
            for level in 0..side.level_count() {
                assert!(side.level_length(level).is_some_and(|length| length > 0));
                assert!(side.level_name(level).is_some_and(|name| !name.is_empty()));
                let groups = GROUPS
                    .iter()
                    .filter(|group| group.contains(side, level))
                    .count();
                assert_eq!(groups, 1, "{side:?} level {level}");
                total += 1;
            }
        }
        assert_eq!(total, 109);
        for group in GROUPS {
            assert!(group.first <= group.last);
            assert!(group.last < group.side.level_count());
        }
    }

    #[test]
    fn a_stage_stays_inside_one_group() {
        assert_eq!(
            Stage::through_group(Side::Main, 0),
            Ok(Stage {
                side: Side::Main,
                level: 0,
                goal: 21
            })
        );
        assert_eq!(Stage::through_group(Side::Main, 30).unwrap().goal, 42);
        assert_eq!(Stage::through_group(Side::D, 9).unwrap().goal, 9);
        assert_eq!(Stage::single(Side::B, 30).unwrap().goal, 30);
        assert!(Stage::single(Side::B, 31).is_err());
        assert!(Stage::new(Side::Main, 20, 22).is_err());
        assert!(Stage::new(Side::Main, 5, 4).is_err());
        assert_eq!(
            Stage::single(Side::Main, 22).unwrap().group().menu,
            Menu::CommunityLevels
        );
    }

    #[test]
    fn the_rom_digests_match_the_pinned_harmony_build() {
        let pins = include_str!("../../../nes/famidash-versions.env");
        for (side, key) in [
            (Side::Main, "FAMIDASH_ROM_SHA256"),
            (Side::B, "FAMIDASH_B_SIDES_ROM_SHA256"),
            (Side::C, "FAMIDASH_C_SIDES_ROM_SHA256"),
            (Side::D, "FAMIDASH_D_SIDES_ROM_SHA256"),
        ] {
            let line = format!("{key}={}", side.rom_sha256());
            assert!(pins.lines().any(|pin| pin == line), "{line}");
            assert_eq!(Side::from_id(side.id()), Some(side));
        }
        assert_eq!(Side::from_rom(&[0]), None);
    }
}
