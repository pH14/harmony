// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::catalog::Fault;
use crate::codec::{self, Reader};

#[must_use]
pub fn process_target(node: u16, fault: &Fault) -> Vec<u8> {
    let mut w = node.to_le_bytes().to_vec();
    codec::write_fault(&mut w, fault);
    w
}

#[must_use]
pub fn decode_process_target(b: &[u8]) -> Option<(u16, Fault)> {
    let mut r = Reader::new(b);
    let node = r.u16().ok()?;
    let fault = codec::read_fault(&mut r).ok()?;
    if !r.at_end() {
        return None;
    }
    Some((node, fault))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Span;

    #[test]
    fn round_trips_every_process_fault() {
        for f in [
            Fault::ProcKill,
            Fault::ProcRestart,
            Fault::ProcPause(Span(1234)),
            Fault::RunHook(7),
            Fault::ProcEventKill { rarity: 0 },
            Fault::ProcEventPark {
                rarity: 3,
                hold: Span(2_000_000),
            },
            Fault::ProcPark {
                addr: 0x4b_0e86,
                hits: 28,
                hold: Span(2_000_000),
            },
        ] {
            let bytes = process_target(3, &f);
            assert_eq!(decode_process_target(&bytes), Some((3, f)));
        }
    }

    #[test]
    fn rejects_truncated_and_trailing() {
        let bytes = process_target(0, &Fault::ProcKill);
        assert_eq!(decode_process_target(&bytes[..bytes.len() - 1]), None);
        let mut extra = bytes.clone();
        extra.push(0);
        assert_eq!(decode_process_target(&extra), None);
    }

    #[test]
    fn a_retired_tag_does_not_decode() {
        assert_eq!(decode_process_target(&[0, 0, 18]), None);
    }

    #[test]
    fn an_event_park_with_no_hold_does_not_decode() {
        let bytes = [0, 0, 21, 1, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(decode_process_target(&bytes), None);
    }

    #[test]
    fn event_rarity_outside_the_shared_width_does_not_decode() {
        let kill = process_target(0, &Fault::ProcEventKill { rarity: 64 });
        assert_eq!(decode_process_target(&kill), None);
        let park = process_target(
            0,
            &Fault::ProcEventPark {
                rarity: 64,
                hold: Span(1),
            },
        );
        assert_eq!(decode_process_target(&park), None);
    }
}
