// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::regs::point_bit;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CheckCapture {
    run: u64,
    start_generation: u64,
    points: u64,
}

impl CheckCapture {
    #[must_use]
    pub fn new(run: u64, start_generation: u64) -> Self {
        Self {
            run,
            start_generation,
            points: 0,
        }
    }

    #[must_use]
    pub fn run(self) -> u64 {
        self.run
    }

    pub fn note_success(&mut self, point: u32) {
        if let Some(bit) = point_bit(point) {
            self.points |= bit;
        }
    }

    #[must_use]
    pub fn complete(self, end_generation: u64) -> CheckEvidence {
        CheckEvidence {
            run: self.run,
            start_generation: self.start_generation,
            end_generation,
            points: self.points,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CheckEvidence {
    pub run: u64,
    pub start_generation: u64,
    pub end_generation: u64,
    pub points: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_supported_points_enter_the_bitmap() {
        let mut capture = CheckCapture::new(1, 1);
        capture.note_success(0);
        capture.note_success(47);
        capture.note_success(48);
        let evidence = capture.complete(1);
        assert_eq!(evidence.points, (1_u64 << 0) | (1_u64 << 47));
    }
}
