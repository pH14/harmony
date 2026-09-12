// SPDX-License-Identifier: AGPL-3.0-or-later

const MUL: u64 = 0x2545_F491_4F6C_DD1D;
const FALLBACK: u64 = 0x9E37_79B9_7F4A_7C15;

#[derive(Clone, Debug)]
pub(crate) struct Prng {
    state: u64,
}

impl Prng {
    pub(crate) fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 { FALLBACK } else { seed },
        }
    }

    pub(crate) fn next_u64(&mut self) -> u64 {
        self.state ^= self.state >> 12;
        self.state ^= self.state << 25;
        self.state ^= self.state >> 27;
        self.state.wrapping_mul(MUL)
    }

    pub(crate) fn raw_state(&self) -> u64 {
        self.state
    }

    pub(crate) fn from_raw_state(state: u64) -> Self {
        Self {
            state: if state == 0 { FALLBACK } else { state },
        }
    }
}
