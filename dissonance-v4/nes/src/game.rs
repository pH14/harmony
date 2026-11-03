// SPDX-License-Identifier: AGPL-3.0-or-later

use engine::target::{Kind, Vocabulary};
use serde::{Deserialize, Serialize};

pub const WRAM_LEN: usize = 0x800;
pub const CART_BASE: usize = 0x6000;
pub const CART_LEN: usize = 0x2000;
pub const START_BIT: u8 = 3;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Scalar {
    pub bytes: Vec<u16>,
    pub kind: Kind,
    #[serde(default)]
    pub signed: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Predicate {
    pub address: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eq: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ne: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ge: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub le: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bits: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bucket: Option<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Game {
    pub name: String,
    pub rom_sha256: String,
    pub cartridge_ram: bool,
    pub scalars: Vec<Scalar>,
    pub controls: Vec<u8>,
    pub start_input: bool,
    pub goal: Vec<Predicate>,
}

pub const GAMES: [(&str, &str); 5] = [
    ("smb", include_str!("../games/smb.json")),
    ("metroid", include_str!("../games/metroid.json")),
    ("zelda", include_str!("../games/zelda.json")),
    ("ice-climbers", include_str!("../games/ice-climbers.json")),
    ("famidash", include_str!("../games/famidash.json")),
];

pub fn by_name(name: &str) -> Result<Game, String> {
    let text = GAMES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, t)| *t)
        .ok_or_else(|| format!("no game config named {name}"))?;
    serde_json::from_str(text).map_err(|e| e.to_string())
}

pub fn by_rom(sha: &str) -> Result<Game, String> {
    for (name, _) in GAMES {
        let g = by_name(name)?;
        if g.rom_sha256 == sha {
            return Ok(g);
        }
    }
    Err(format!("no game config for ROM {sha}"))
}

pub fn read(address: u16, wram: &[u8], cart: &[u8]) -> u8 {
    let a = usize::from(address);
    if a < WRAM_LEN {
        wram[a]
    } else if (CART_BASE..CART_BASE + CART_LEN).contains(&a) {
        cart.get(a - CART_BASE).copied().unwrap_or(0)
    } else {
        0
    }
}

impl Predicate {
    pub fn holds(&self, wram: &[u8], cart: &[u8], start: u8) -> bool {
        let v = read(self.address, wram, cart);
        self.eq.is_none_or(|x| v == x)
            && self.ne.is_none_or(|x| v != x)
            && self.ge.is_none_or(|x| v >= x)
            && self.le.is_none_or(|x| v <= x)
            && self.bits.is_none_or(|m| v & m == m)
            && self.bucket.is_none_or(|w| v / w != start / w)
    }

    pub fn validate(&self, cartridge_ram: bool) -> Result<(), String> {
        let n = [self.eq, self.ne, self.ge, self.le, self.bits, self.bucket]
            .iter()
            .filter(|x| x.is_some())
            .count();
        if n != 1 {
            return Err(format!(
                "predicate on {:#x} needs exactly one test",
                self.address
            ));
        }
        if self.bucket == Some(0) {
            return Err("bucket width must be at least 1".into());
        }
        let a = usize::from(self.address);
        if a >= WRAM_LEN && !(cartridge_ram && (CART_BASE..CART_BASE + CART_LEN).contains(&a)) {
            return Err(format!("predicate address {a:#x} is outside readable RAM"));
        }
        Ok(())
    }
}

impl Game {
    pub fn kinds(&self) -> Vec<Kind> {
        self.scalars.iter().map(|s| s.kind).collect()
    }

    pub fn chord_inputs(&self) -> u32 {
        1u32 << self.controls.len()
    }

    pub fn vocabulary(&self, base_steps: u64) -> Vocabulary {
        Vocabulary {
            inputs: self.chord_inputs() + u32::from(self.start_input),
            holds: true,
            base_steps,
        }
    }

    pub fn buttons(&self, input: u32) -> u8 {
        if input >= self.chord_inputs() {
            return 1 << START_BIT;
        }
        self.controls
            .iter()
            .enumerate()
            .filter(|(i, _)| input & (1 << i) != 0)
            .fold(0u8, |acc, (_, b)| acc | (1 << b))
    }

    pub fn scalars_into(&self, wram: &[u8], cart: &[u8], out: &mut Vec<u64>) {
        out.clear();
        for s in &self.scalars {
            let v = s.bytes.iter().enumerate().fold(0u64, |acc, (i, &a)| {
                acc | (u64::from(read(a, wram, cart)) << (8 * i))
            });
            let v = if s.signed {
                v ^ (1u64 << (8 * s.bytes.len() - 1))
            } else {
                v
            };
            out.push(v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_game_parses_and_maps_buttons() {
        for (name, _) in GAMES {
            let g = by_name(name).unwrap();
            assert!(!g.scalars.is_empty() && g.scalars.len() <= 24, "{name}");
            for p in &g.goal {
                p.validate(g.cartridge_ram).unwrap();
            }
            let all = g.chord_inputs() - 1;
            let mask = g.controls.iter().fold(0u8, |a, b| a | (1 << b));
            assert_eq!(g.buttons(all), mask);
            assert_eq!(g.buttons(0), 0);
            if g.start_input {
                assert_eq!(g.buttons(g.chord_inputs()), 1 << START_BIT);
            }
            assert_eq!(by_rom(&g.rom_sha256).unwrap().name, g.name);
        }
    }

    #[test]
    fn multi_byte_scalars_read_low_byte_first() {
        let g = by_name("smb").unwrap();
        let mut wram = vec![0u8; WRAM_LEN];
        wram[0x86] = 0x34;
        wram[0x6D] = 0x12;
        let mut out = Vec::new();
        g.scalars_into(&wram, &[], &mut out);
        assert_eq!(out[0], 0x1234);
    }

    #[test]
    fn signed_scalars_keep_numeric_order() {
        let g = by_name("famidash").unwrap();
        assert!(g.scalars[0].signed);
        let mut wram = vec![0u8; WRAM_LEN];
        let mut read_at = |v: i32| {
            wram[0x4A2..0x4A6].copy_from_slice(&v.to_le_bytes());
            let mut out = Vec::new();
            g.scalars_into(&wram, &[], &mut out);
            out[0]
        };
        assert!(read_at(-5) < read_at(0) && read_at(0) < read_at(7));
    }
}
