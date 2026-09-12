// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::catalog::DecisionPoint;
use crate::policy::FaultPolicy;
use crate::prng::Prng;
use crate::{Answer, DecisionClass, Environment, MAX_SUPPLY_LEN, Outcome};

const FAULT_DOMAIN: u64 = 0xD1B5_4A32_D192_ED03;

#[derive(Clone, Debug)]
pub struct SeededEnv {
    supply: Prng,
    fault: Prng,
    policy: FaultPolicy,
}

impl SeededEnv {
    pub fn new(seed: u64, policy: FaultPolicy) -> Self {
        Self {
            supply: Prng::new(seed),
            fault: Prng::new(seed ^ FAULT_DOMAIN),
            policy,
        }
    }

    pub fn stream_state(&self) -> [u8; 16] {
        let mut out = [0_u8; 16];
        out[..8].copy_from_slice(&self.supply.raw_state().to_le_bytes());
        out[8..].copy_from_slice(&self.fault.raw_state().to_le_bytes());
        out
    }

    pub fn restore_stream_state(&mut self, state: &[u8; 16]) {
        let supply = u64::from_le_bytes([
            state[0], state[1], state[2], state[3], state[4], state[5], state[6], state[7],
        ]);
        let fault = u64::from_le_bytes([
            state[8], state[9], state[10], state[11], state[12], state[13], state[14], state[15],
        ]);
        self.supply = Prng::from_raw_state(supply);
        self.fault = Prng::from_raw_state(fault);
    }

    pub(crate) fn answer(&mut self, point: &DecisionPoint) -> Answer {
        match point {
            DecisionPoint::Entropy { bytes } | DecisionPoint::Payload { bytes } => {
                Answer::Supply(self.supply_bytes(*bytes))
            }
            DecisionPoint::Scheduler { ready } => Answer::Supply(self.scheduler_pick(*ready)),
            DecisionPoint::NetFlow { .. } => {
                self.policy.sample(DecisionClass::NetFlow, &mut self.fault)
            }
            DecisionPoint::BlockIo { .. } => {
                self.policy.sample(DecisionClass::BlockIo, &mut self.fault)
            }
            DecisionPoint::Process { .. } => {
                self.policy.sample(DecisionClass::Process, &mut self.fault)
            }
            DecisionPoint::Buggify { point } => self.policy.sample_buggify(*point, &mut self.fault),
        }
    }

    fn supply_bytes(&mut self, bytes: u32) -> Vec<u8> {
        let n = bytes.min(MAX_SUPPLY_LEN) as usize;
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            let word = self.supply.next_u64().to_le_bytes();
            let take = (n - out.len()).min(8);
            out.extend_from_slice(&word[..take]);
        }
        out
    }

    fn scheduler_pick(&mut self, ready: u32) -> Vec<u8> {
        let w = self.supply.next_u64();
        let idx = if ready == 0 {
            0
        } else {
            (w % ready as u64) as u32
        };
        idx.to_le_bytes().to_vec()
    }
}

impl Environment for SeededEnv {
    fn decide(&mut self, point: &DecisionPoint) -> Outcome {
        Outcome::Resolved(self.answer(point))
    }
}
