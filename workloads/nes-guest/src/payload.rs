// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::{core_seam::Core, nova::NovaChannel};
use nes_protocol::{MAX_HOLD_FRAMES, NovaBillboardLayout};

pub const REG_GPA: u32 = 1;
pub const REG_LEN: u32 = 2;
pub const CATALOG: &[harmony_sdk::Point] = &[
    harmony_sdk::Point::state(REG_GPA, "nes.publication.gpa"),
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

    pub fn run_chord<H: NovaChannel>(
        &mut self,
        channel: &mut H,
        bytes: &mut [u8],
    ) -> Result<(), String>
    where
        H::Error: std::fmt::Debug,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core_seam::MockCore;
    struct Channel {
        action: [u8; 2],
        completed: Vec<u64>,
    }
    impl NovaChannel for Channel {
        type Error = ();
        fn payload_fetch(&mut self, out: &mut [u8; 2]) -> Result<(), ()> {
            *out = self.action;
            Ok(())
        }
        fn state_set(&mut self, _: u32, _: u64) -> Result<(), ()> {
            panic!("no action state events")
        }
        fn state_max(&mut self, _: u32, _: u64) -> Result<(), ()> {
            panic!("no action state events")
        }
        fn reachable(&mut self, _: u32) -> Result<(), ()> {
            panic!("no game interpretation")
        }
        fn frame_complete(&mut self, frame: u64) -> Result<(), ()> {
            self.completed.push(frame);
            Ok(())
        }
    }
    #[test]
    fn power_on_and_exact_intervals_use_shared_decoder() {
        let mut agent = NesAgent::new(MockCore::new()).unwrap();
        let mut bytes = vec![0; agent.layout().total_len()];
        agent.prime(&mut bytes).unwrap();
        assert!(
            nes_protocol::parse_billboard(&bytes, false)
                .unwrap()
                .work_frames
                .is_empty()
        );
        let mut channel = Channel {
            action: [0, 120],
            completed: vec![],
        };
        agent.run_chord(&mut channel, &mut bytes).unwrap();
        assert_eq!(
            nes_protocol::parse_billboard(&bytes, true)
                .unwrap()
                .work_frames
                .len(),
            120
        );
        channel.action = [0, 0];
        agent.run_chord(&mut channel, &mut bytes).unwrap();
        assert_eq!(channel.completed, [120, 121]);
        assert_eq!(
            nes_protocol::parse_billboard(&bytes, true)
                .unwrap()
                .work_frames
                .len(),
            1
        );
    }
    #[test]
    fn rejects_truncated_publication_before_consuming_input() {
        let mut agent = NesAgent::new(MockCore::new()).unwrap();
        let mut channel = Channel {
            action: [0, 1],
            completed: vec![],
        };
        assert!(agent.run_chord(&mut channel, &mut []).is_err());
        assert!(channel.completed.is_empty());
    }
}
