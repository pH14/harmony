// SPDX-License-Identifier: AGPL-3.0-or-later
#![no_std]

extern crate alloc;
use alloc::{
    format,
    string::{String, ToString},
};
use nes_protocol::{MAX_HOLD_FRAMES, NovaBillboardLayout};

pub const REG_HANDLE: u32 = 1;
pub const REG_LEN: u32 = 2;
pub const CATALOG: &[harmony_sdk::Point] = &[
    harmony_sdk::Point::state(REG_HANDLE, "nes.publication.handle"),
    harmony_sdk::Point::state(REG_LEN, "nes.publication.len"),
];

pub struct NesAgent<C> {
    core: C,
    layout: NovaBillboardLayout,
    frame: u32,
}

impl<C: Core> NesAgent<C> {
    pub fn new(core: C) -> Result<Self, String> {
        Ok(Self {
            core,
            layout: NovaBillboardLayout::new(0).map_err(|e| e.to_string())?,
            frame: 0,
        })
    }

    pub fn layout(&self) -> NovaBillboardLayout {
        self.layout
    }

    pub fn prime(&mut self, bytes: &mut [u8]) -> Result<(), String> {
        self.validate(bytes)?;
        if !self
            .core
            .read_work_ram(self.layout.work_ram_slot_mut(bytes, 0))
        {
            return Err("NES work RAM unavailable".into());
        }
        self.publish(bytes, 0, 0)
    }

    pub fn run_chord<H: Channel>(&mut self, channel: &mut H, bytes: &mut [u8]) -> Result<(), String>
    where
        H::Error: core::fmt::Debug,
    {
        self.validate(bytes)?;
        let mut action = [0; 2];
        channel
            .payload_fetch(&mut action)
            .map_err(|e| format!("NES input: {e:?}"))?;
        let hold = action[1].clamp(1, MAX_HOLD_FRAMES);
        let endpoint = self
            .frame
            .checked_add(u32::from(hold))
            .ok_or("NES frame overflow")?;
        for slot in 0..usize::from(hold) {
            self.core.run_frame(action[0]);
            if !self
                .core
                .read_work_ram(self.layout.work_ram_slot_mut(bytes, slot))
            {
                return Err("NES work RAM unavailable".into());
            }
        }
        self.frame = endpoint;
        self.publish(bytes, action[0], hold)?;
        channel
            .frame_complete(u64::from(self.frame))
            .map_err(|e| format!("NES completion: {e:?}"))
    }

    fn validate(&self, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() < self.layout.total_len() {
            return Err("NES publication truncated".into());
        }
        Ok(())
    }

    fn publish(&mut self, bytes: &mut [u8], buttons: u8, frames: u8) -> Result<(), String> {
        let save = self.layout.save_ram_mut(bytes);
        save.fill(0);
        if let Some(len) = self.core.read_save_ram(save)
            && len > save.len()
        {
            return Err("NES save RAM exceeds publication".into());
        }
        self.layout
            .write_header(bytes, self.frame, buttons, frames, false, false)
            .map_err(|e| e.to_string())
    }
}

pub trait Core {
    fn serialize_size(&mut self) -> usize;

    fn serialize(&mut self, out: &mut [u8]) -> bool;

    fn run_frame(&mut self, joypad: u8);

    fn read_work_ram(&mut self, out: &mut [u8]) -> bool;

    fn read_save_ram(&mut self, _out: &mut [u8]) -> Option<usize> {
        None
    }
}

pub trait Channel {
    type Error;

    fn payload_fetch(&mut self, out: &mut [u8; 2]) -> Result<(), Self::Error>;
    fn state_set(&mut self, reg: u32, value: u64) -> Result<(), Self::Error>;
    fn state_max(&mut self, reg: u32, value: u64) -> Result<(), Self::Error>;
    fn reachable(&mut self, point: u32) -> Result<(), Self::Error>;
    fn frame_complete(&mut self, frame_count: u64) -> Result<(), Self::Error>;
}
