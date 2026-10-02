// SPDX-License-Identifier: AGPL-3.0-or-later

pub use nes_agent::{CATALOG, NesAgent, REG_HANDLE, REG_LEN};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{core_seam::MockCore, nova::NovaChannel};
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
