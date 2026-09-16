// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::VecDeque;

use crate::target::{
    CUR_MAP_HEIGHT, CUR_MAP_WIDTH, NUM_SPRITES, OVERWORLD_MAP, SPRITE_COORDINATE_BIAS,
    SPRITE_MAP_X, SPRITE_MAP_Y, SPRITE_STATE_DATA_2, SPRITE_STRUCT_BYTES, SPRITE_STRUCT_COUNT,
    TALKING_OVER_TILE_COUNT, TILESET_BANK, TILESET_BLOCKS_POINTER, TILESET_COLLISION_POINTER,
    TILESET_TALKING_OVER_TILES, WRAM_BASE, byte, word,
};

pub const MAP_BORDER_BLOCKS: usize = 3;
pub const BLOCK_BYTES: usize = 16;
pub const BLOCK_TILES: usize = 4;
pub const STEPS_PER_BLOCK: usize = 2;
pub const STEP_FOOT_ROW: usize = 1;
pub const COLLISION_LIST_END: u8 = 0xff;
pub const ROM_BANK_BYTES: usize = 0x4000;
pub const MAX_MAP_BLOCKS: usize = 64;

pub const STEP_DOWN: u8 = 0;
pub const STEP_UP: u8 = 1;
pub const STEP_LEFT: u8 = 2;
pub const STEP_RIGHT: u8 = 3;
pub const STEPS: [u8; 4] = [STEP_DOWN, STEP_UP, STEP_LEFT, STEP_RIGHT];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Overworld {
    width: usize,
    height: usize,
    walkable: Vec<bool>,
    tiles: Vec<u8>,
    collision: Vec<u8>,
    talk_over: Vec<u8>,
}

impl Overworld {
    #[must_use]
    pub fn decode(wram: &[u8], rom: &[u8]) -> Option<Self> {
        let blocks_wide = usize::from(byte(wram, CUR_MAP_WIDTH));
        let blocks_high = usize::from(byte(wram, CUR_MAP_HEIGHT));
        if blocks_wide == 0
            || blocks_high == 0
            || blocks_wide > MAX_MAP_BLOCKS
            || blocks_high > MAX_MAP_BLOCKS
        {
            return None;
        }
        let bank = usize::from(byte(wram, TILESET_BANK));
        let blocks = rom_slice(rom, bank, word(wram, TILESET_BLOCKS_POINTER))?;
        let collision = collision_list(rom_slice(rom, 0, word(wram, TILESET_COLLISION_POINTER))?);
        let stride = blocks_wide + 2 * MAP_BORDER_BLOCKS;
        let map_base = usize::from(OVERWORLD_MAP - WRAM_BASE);
        let width = blocks_wide * STEPS_PER_BLOCK;
        let height = blocks_high * STEPS_PER_BLOCK;
        let mut walkable = vec![false; width * height];
        let mut tiles = vec![0_u8; width * height];
        for y in 0..height {
            for x in 0..width {
                let block_index = map_base
                    + (y / STEPS_PER_BLOCK + MAP_BORDER_BLOCKS) * stride
                    + x / STEPS_PER_BLOCK
                    + MAP_BORDER_BLOCKS;
                let block = wram.get(block_index).copied()?;
                let tile_row = (y % STEPS_PER_BLOCK) * STEPS_PER_BLOCK + STEP_FOOT_ROW;
                let tile_column = (x % STEPS_PER_BLOCK) * STEPS_PER_BLOCK;
                let offset = usize::from(block) * BLOCK_BYTES + tile_row * BLOCK_TILES + tile_column;
                let tile = blocks.get(offset).copied()?;
                tiles[y * width + x] = tile;
                walkable[y * width + x] = collision.contains(&tile);
            }
        }
        let talk_over = (0..TALKING_OVER_TILE_COUNT)
            .map(|index| byte(wram, TILESET_TALKING_OVER_TILES + index))
            .collect();
        let mut overworld = Self {
            width,
            height,
            walkable,
            tiles,
            collision,
            talk_over,
        };
        overworld.block_standing_sprites(wram);
        Some(overworld)
    }

    fn block_standing_sprites(&mut self, wram: &[u8]) {
        let present = u16::from(byte(wram, NUM_SPRITES)).min(SPRITE_STRUCT_COUNT - 1);
        for index in 1..=present {
            let base = SPRITE_STATE_DATA_2 + SPRITE_STRUCT_BYTES * index;
            let y = byte(wram, base + SPRITE_MAP_Y);
            let x = byte(wram, base + SPRITE_MAP_X);
            if y < SPRITE_COORDINATE_BIAS || x < SPRITE_COORDINATE_BIAS {
                continue;
            }
            let (x, y) = (
                usize::from(x - SPRITE_COORDINATE_BIAS),
                usize::from(y - SPRITE_COORDINATE_BIAS),
            );
            if x < self.width && y < self.height {
                self.walkable[y * self.width + x] = false;
            }
        }
    }

    #[must_use]
    pub fn width(&self) -> usize {
        self.width
    }

    #[must_use]
    pub fn height(&self) -> usize {
        self.height
    }

    #[must_use]
    pub fn tile(&self, x: u8, y: u8) -> u8 {
        let (x, y) = (usize::from(x), usize::from(y));
        if x < self.width && y < self.height {
            self.tiles[y * self.width + x]
        } else {
            0
        }
    }

    #[must_use]
    pub fn collision(&self) -> &[u8] {
        &self.collision
    }

    #[must_use]
    pub fn talks_over(&self, x: u8, y: u8) -> bool {
        let (ux, uy) = (usize::from(x), usize::from(y));
        ux < self.width && uy < self.height && self.talk_over.contains(&self.tile(x, y))
    }

    #[must_use]
    pub fn walkable(&self, x: u8, y: u8) -> bool {
        let (x, y) = (usize::from(x), usize::from(y));
        x < self.width && y < self.height && self.walkable[y * self.width + x]
    }

    #[must_use]
    pub fn neighbour(&self, x: u8, y: u8, step: u8) -> Option<(u8, u8)> {
        let (dx, dy) = step_delta(step);
        let x = i64::from(x) + i64::from(dx);
        let y = i64::from(y) + i64::from(dy);
        if x < 0 || y < 0 || x >= self.width as i64 || y >= self.height as i64 {
            return None;
        }
        Some((u8::try_from(x).ok()?, u8::try_from(y).ok()?))
    }

    #[must_use]
    pub fn distances(&self, from: (u8, u8)) -> Vec<u32> {
        let mut distance = vec![u32::MAX; self.width * self.height];
        if usize::from(from.0) >= self.width || usize::from(from.1) >= self.height {
            return distance;
        }
        let mut queue = VecDeque::new();
        distance[self.slot(from)] = 0;
        queue.push_back(from);
        while let Some(here) = queue.pop_front() {
            let so_far = distance[self.slot(here)];
            for step in STEPS {
                let Some(next) = self.neighbour(here.0, here.1, step) else {
                    continue;
                };
                let slot = self.slot(next);
                if distance[slot] != u32::MAX {
                    continue;
                }
                distance[slot] = so_far.saturating_add(1);
                if self.walkable(next.0, next.1) {
                    queue.push_back(next);
                }
            }
        }
        distance
    }

    #[must_use]
    pub fn route(&self, from: (u8, u8), to: (u8, u8)) -> Option<Vec<u8>> {
        if from == to {
            return Some(Vec::new());
        }
        if usize::from(to.0) >= self.width || usize::from(to.1) >= self.height {
            return None;
        }
        let mut arrived_by = vec![u8::MAX; self.width * self.height];
        let mut visited = vec![false; self.width * self.height];
        let mut queue = VecDeque::new();
        visited[self.slot(from)] = true;
        queue.push_back(from);
        while let Some(here) = queue.pop_front() {
            for step in STEPS {
                let Some(next) = self.neighbour(here.0, here.1, step) else {
                    continue;
                };
                let slot = self.slot(next);
                if visited[slot] || (!self.walkable(next.0, next.1) && next != to) {
                    continue;
                }
                visited[slot] = true;
                arrived_by[slot] = step;
                if next == to {
                    return Some(self.unwind(&arrived_by, from, to));
                }
                queue.push_back(next);
            }
        }
        None
    }

    fn slot(&self, at: (u8, u8)) -> usize {
        usize::from(at.1) * self.width + usize::from(at.0)
    }

    fn unwind(&self, arrived_by: &[u8], from: (u8, u8), to: (u8, u8)) -> Vec<u8> {
        let mut steps = Vec::new();
        let mut here = to;
        while here != from {
            let step = arrived_by[self.slot(here)];
            steps.push(step);
            let (dx, dy) = step_delta(step);
            here = (
                u8::try_from(i64::from(here.0) - i64::from(dx)).unwrap_or(0),
                u8::try_from(i64::from(here.1) - i64::from(dy)).unwrap_or(0),
            );
        }
        steps.reverse();
        steps
    }
}

#[must_use]
pub fn step_delta(step: u8) -> (i8, i8) {
    match step {
        STEP_UP => (0, -1),
        STEP_LEFT => (-1, 0),
        STEP_RIGHT => (1, 0),
        _ => (0, 1),
    }
}

#[must_use]
pub fn step_towards(from: (u8, u8), to: (u8, u8)) -> Option<u8> {
    STEPS.into_iter().find(|step| {
        let (dx, dy) = step_delta(*step);
        i64::from(from.0) + i64::from(dx) == i64::from(to.0)
            && i64::from(from.1) + i64::from(dy) == i64::from(to.1)
    })
}

fn rom_slice(rom: &[u8], bank: usize, pointer: u16) -> Option<&[u8]> {
    let offset = if usize::from(pointer) < ROM_BANK_BYTES {
        usize::from(pointer)
    } else {
        bank.max(1) * ROM_BANK_BYTES + usize::from(pointer) - ROM_BANK_BYTES
    };
    rom.get(offset..)
}

fn collision_list(bytes: &[u8]) -> Vec<u8> {
    bytes
        .iter()
        .take_while(|tile| **tile != COLLISION_LIST_END)
        .copied()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wram_with(map: &[u8], width: u8, height: u8) -> Vec<u8> {
        let mut wram = vec![0_u8; 8192];
        let mut put = |address: u16, value: u8| {
            wram[usize::from(address - WRAM_BASE)] = value;
        };
        put(CUR_MAP_WIDTH, width);
        put(CUR_MAP_HEIGHT, height);
        put(TILESET_BANK, 0);
        put(TILESET_BLOCKS_POINTER, 0x00);
        put(TILESET_BLOCKS_POINTER + 1, 0x10);
        put(TILESET_COLLISION_POINTER, 0x00);
        put(TILESET_COLLISION_POINTER + 1, 0x20);
        let base = usize::from(OVERWORLD_MAP - WRAM_BASE);
        wram[base..base + map.len()].copy_from_slice(map);
        wram
    }

    fn rom_with_two_blocks() -> Vec<u8> {
        let mut rom = vec![0_u8; 0x8000];
        rom[0x1000..0x1000 + BLOCK_BYTES].copy_from_slice(&[0xaa; BLOCK_BYTES]);
        rom[0x1010..0x1010 + BLOCK_BYTES].copy_from_slice(&[0xbb; BLOCK_BYTES]);
        rom[0x2000] = 0xaa;
        rom[0x2001] = COLLISION_LIST_END;
        rom
    }

    #[test]
    fn a_block_of_walkable_tiles_becomes_four_walkable_steps() {
        let stride = 1 + 2 * MAP_BORDER_BLOCKS;
        let mut map = vec![1_u8; stride * (1 + 2 * MAP_BORDER_BLOCKS)];
        map[MAP_BORDER_BLOCKS * stride + MAP_BORDER_BLOCKS] = 0;
        let overworld = Overworld::decode(&wram_with(&map, 1, 1), &rom_with_two_blocks())
            .expect("decode a one-block map");
        assert_eq!((overworld.width(), overworld.height()), (2, 2));
        for y in 0..2 {
            for x in 0..2 {
                assert!(overworld.walkable(x, y));
            }
        }
    }

    #[test]
    fn a_route_goes_around_a_wall_and_refuses_an_unreachable_tile() {
        let (blocks_wide, blocks_high) = (3_usize, 3_usize);
        let stride = blocks_wide + 2 * MAP_BORDER_BLOCKS;
        let mut map = vec![1_u8; stride * (blocks_high + 2 * MAP_BORDER_BLOCKS)];
        for by in 0..blocks_high {
            for bx in 0..blocks_wide {
                map[(by + MAP_BORDER_BLOCKS) * stride + bx + MAP_BORDER_BLOCKS] =
                    u8::from(bx == 1 && by < 2);
            }
        }
        let overworld = Overworld::decode(&wram_with(&map, 3, 3), &rom_with_two_blocks())
            .expect("decode a walled map");
        let route = overworld.route((0, 0), (4, 0)).expect("a route around");
        assert_eq!(route.len(), 12);
    }

    #[test]
    fn a_goal_tile_is_entered_even_when_it_is_closed_and_nothing_crosses_it() {
        let (blocks_wide, blocks_high) = (3_usize, 3_usize);
        let stride = blocks_wide + 2 * MAP_BORDER_BLOCKS;
        let mut map = vec![1_u8; stride * (blocks_high + 2 * MAP_BORDER_BLOCKS)];
        for by in 0..blocks_high {
            for bx in 0..blocks_wide {
                map[(by + MAP_BORDER_BLOCKS) * stride + bx + MAP_BORDER_BLOCKS] = u8::from(bx == 1);
            }
        }
        let overworld = Overworld::decode(&wram_with(&map, 3, 3), &rom_with_two_blocks())
            .expect("decode a fully walled map");
        assert_eq!(overworld.route((0, 0), (2, 0)), Some(vec![STEP_RIGHT, STEP_RIGHT]));
        assert!(overworld.route((0, 0), (4, 0)).is_none());
    }
}
