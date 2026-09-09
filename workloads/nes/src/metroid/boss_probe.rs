// SPDX-License-Identifier: AGPL-3.0-or-later
//! Raw read-only encounter diagnostics; never archive or terminal policy.

use machine::MachineError;
use serde::Serialize;

/// Raw enemy-slot bytes. The special byte is not a permanent enemy identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct EnemyBytes {
    pub slot: u8,
    pub status: u8,
    pub data_index: u8,
    pub special: u8,
    pub hit_points: u8,
    pub x: u8,
    pub y: u8,
    pub name_table: u8,
}

/// Raw fields at one frame boundary, with no inferred encounter or damage total.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BossMemory {
    pub area: u8,
    pub mode: u8,
    pub door: u8,
    pub room_number: u8,
    pub loader_present: u8,
    pub kraid_status: u8,
    pub ridley_status: u8,
    pub enemies: Vec<EnemyBytes>,
}

pub(crate) fn decode(wram: &[u8], cartridge: &[u8]) -> Result<BossMemory, MachineError> {
    fn byte(memory: &[u8], index: usize) -> Result<u8, MachineError> {
        memory.get(index).copied().ok_or_else(|| {
            MachineError::Backend(format!("Metroid diagnostic RAM index {index:#x} is absent"))
        })
    }
    // Pinned disassembly 4270d57f, Bank07 GetEnemyType/LEB4D/LF85A:
    // six enemy slots at offsets 0x00..0x50; 0x60..0xb0 are projectiles.
    // Cartridge indices are CPU addresses minus the mapped 0x6000 base.
    let mut enemies = Vec::with_capacity(6);
    for slot in (0..0x60u8).step_by(0x10) {
        let offset = usize::from(slot);
        enemies.push(EnemyBytes {
            slot,
            status: byte(cartridge, 0xaf4 + offset)?,
            data_index: byte(cartridge, 0xb02 + offset)?,
            special: byte(wram, 0x40f + offset)?,
            hit_points: byte(wram, 0x40b + offset)?,
            x: byte(wram, 0x401 + offset)?,
            y: byte(wram, 0x400 + offset)?,
            name_table: byte(cartridge, 0xafb + offset)?,
        });
    }
    Ok(BossMemory {
        area: byte(wram, 0x74)?,
        mode: byte(wram, 0x1e)?,
        door: byte(wram, 0x56)?,
        room_number: byte(wram, 0x5a)?,
        loader_present: byte(cartridge, 0x987)?,
        kraid_status: byte(cartridge, 0x87b)?,
        ridley_status: byte(cartridge, 0x87c)?,
        enemies,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_probe_keeps_inactive_stale_and_overwritten_fields_distinct() {
        let mut wram = [0; 2048];
        let mut cartridge = [0; 8192];
        wram[0x74] = 0x12;
        wram[0x40f] = 0x40;
        wram[0x40b] = 99;
        let stale = decode(&wram, &cartridge).unwrap();
        assert_eq!(stale.loader_present, 0);
        assert_eq!(stale.enemies[0].status, 0);
        assert_eq!(stale.enemies[0].hit_points, 99);
        cartridge[0x987] = 1;
        cartridge[0xaf4] = 2;
        let loaded = decode(&wram, &cartridge).unwrap();
        wram[0x40f] = 3;
        wram[0x40b] = 98;
        let changed = decode(&wram, &cartridge).unwrap();
        assert_eq!(loaded.loader_present, changed.loader_present);
        assert_eq!(loaded.enemies[0].status, changed.enemies[0].status);
        assert_ne!(loaded.enemies[0].special, changed.enemies[0].special);
        assert_eq!(changed.enemies[0].hit_points, 98);
        assert_eq!(changed.enemies.len(), 6);
        assert_eq!(changed.enemies[5].slot, 0x50);
        assert!(decode(&wram[..0x450], &cartridge).is_err());
        assert!(decode(&wram, &cartridge[..0xb02]).is_err());
    }
}
