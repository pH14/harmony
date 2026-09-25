// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::Span;
use crate::catalog::Fault;
use process_proto::{ProcessAction, decode_target, encode_target};

#[must_use]
pub fn process_target(node: u16, fault: &Fault) -> Vec<u8> {
    encode_target(node, &to_process_action(fault))
}

#[must_use]
pub fn decode_process_target(b: &[u8]) -> Option<(u16, Fault)> {
    let (node, action) = decode_target(b)?;
    Some((node, from_process_action(action)?))
}

fn to_process_action(fault: &Fault) -> ProcessAction {
    match fault {
        Fault::ProcPause(Span(nanos)) => ProcessAction::Pause(*nanos),
        Fault::ProcKill => ProcessAction::Kill,
        Fault::ProcRestart => ProcessAction::Restart,
        Fault::RunHook(id) => ProcessAction::RunHook(*id),
        Fault::ProcEventKill { rarity } => ProcessAction::EventKill { rarity: *rarity },
        Fault::ProcEventPark {
            edges,
            hold,
            target,
        } => ProcessAction::EventPark {
            edges: *edges,
            hold_nanos: hold.0,
            target: *target,
        },
        _ => unreachable!("process_target received a non-process fault"),
    }
}

fn from_process_action(action: ProcessAction) -> Option<Fault> {
    Some(match action {
        ProcessAction::Pause(nanos) => Fault::ProcPause(Span(nanos)),
        ProcessAction::Kill => Fault::ProcKill,
        ProcessAction::Restart => Fault::ProcRestart,
        ProcessAction::RunHook(id) => Fault::RunHook(id),
        ProcessAction::EventKill { rarity } => Fault::ProcEventKill { rarity },
        ProcessAction::EventPark {
            edges,
            hold_nanos,
            target,
        } => Fault::ProcEventPark {
            edges,
            hold: Span(hold_nanos),
            target,
        },
    })
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
                edges: 3,
                hold: Span(2_000_000),
                target: None,
            },
            Fault::ProcEventPark {
                edges: 1,
                hold: Span(2_000_000),
                target: crate::ParkTarget::new(0x892e8, 0x892ec),
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
        let bytes = [0, 0, 21, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(decode_process_target(&bytes), None);
    }

    #[test]
    fn event_selectors_outside_their_ranges_do_not_decode() {
        let kill = process_target(0, &Fault::ProcEventKill { rarity: 64 });
        assert_eq!(decode_process_target(&kill), None);
        for edges in [0, crate::EVENT_PARK_EDGE_LIMIT + 1] {
            let park = process_target(
                0,
                &Fault::ProcEventPark {
                    edges,
                    hold: Span(1),
                    target: None,
                },
            );
            assert_eq!(decode_process_target(&park), None);
        }
    }
}
