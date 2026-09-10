// SPDX-License-Identifier: AGPL-3.0-or-later
//! The [`DecisionClass::Process`](crate::DecisionClass::Process) target
//! encoding, shared by the host searcher and the in-guest fault agent so a
//! [`StandingFault`](crate::StandingFault)'s opaque `target` bytes mean the same
//! thing on both sides.
//!
//! Layout: `u16` node id little-endian, then [`Fault::encode`](crate::Answer)
//! bytes as written by the shared catalog codec.

use crate::catalog::Fault;
use crate::codec::{self, Reader};

/// Encode a process-class standing-fault target.
#[must_use]
pub fn process_target(node: u16, fault: &Fault) -> Vec<u8> {
    let mut w = node.to_le_bytes().to_vec();
    codec::write_fault(&mut w, fault);
    w
}

/// Decode bytes produced by [`process_target`]. `None` on any malformed or
/// trailing input — a malformed target never panics a service.
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
            Fault::ProcEventKill { ordinal: 123_456 },
            Fault::ProcRestart,
            Fault::ProcPause(Span(1234)),
            Fault::RunHook(7),
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
        // Tag 18 is unassigned; a target naming it is refused rather than
        // reinterpreted as a neighbouring process fault.
        assert_eq!(decode_process_target(&[0, 0, 18]), None);
    }
}
