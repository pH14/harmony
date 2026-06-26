// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 2 — determinism: equal states encode to identical bytes, regardless of
//! how their MSR map or timer queue was built.

mod common;

use common::{arb_timers, arb_vm_state, config, fully_populated};
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};
use vm_state::{MsrBlock, TimerEntry, TimerQueueState, VmState, VmStateError, VtimeState};

/// An integer (encodable) V-time config; `VmState::default()` has `ratio_den == 0`.
fn int_ratio() -> VtimeState {
    VtimeState {
        ratio_den: 1,
        ..Default::default()
    }
}

#[test]
fn encode_is_byte_identical_twice() {
    let s = fully_populated();
    assert_eq!(s.encode().unwrap(), s.encode().unwrap());
}

#[test]
fn msr_insertion_order_is_irrelevant() {
    // Two `==` VmStates whose MSR maps were built by inserting in different
    // orders — the BTreeMap canonicalizes, so they are equal and encode
    // identically.
    let mut map_a = BTreeMap::new();
    map_a.insert(0x10u32, 0x1111u64);
    map_a.insert(0x174, 0x2222);
    map_a.insert(0xC000_0080, 0x3333);

    let mut map_b = BTreeMap::new();
    map_b.insert(0xC000_0080u32, 0x3333u64);
    map_b.insert(0x174, 0x2222);
    map_b.insert(0x10, 0x1111);

    let a = VmState {
        msrs: MsrBlock(map_a),
        vtime: int_ratio(),
        ..Default::default()
    };
    let b = VmState {
        msrs: MsrBlock(map_b),
        vtime: int_ratio(),
        ..Default::default()
    };

    assert_eq!(
        a, b,
        "BTreeMap makes insertion order irrelevant to equality"
    );
    assert_eq!(a.encode().unwrap(), b.encode().unwrap());
}

/// Build a `VmState` carrying just these timer entries (integer ratio so the
/// only thing that can make `encode` fail is the timer queue).
fn with_timers(entries: Vec<TimerEntry>, next_seq: u64) -> VmState {
    VmState {
        timers: TimerQueueState { entries, next_seq },
        vtime: int_ratio(),
        ..Default::default()
    }
}

#[test]
fn canonical_timer_queue_round_trips_in_seq_order() {
    // Canonical order is (50,5), (100,1), (100,2): at deadline 100, seq 1 (token
    // 99) precedes seq 2 (token 7) — i.e. FIFO/seq order, the reverse of token
    // order, proving the sort key is seq and not token.
    let canonical = vec![
        TimerEntry {
            deadline_vns: 50,
            seq: 5,
            token: 8,
            period_vns: 7,
        },
        TimerEntry {
            deadline_vns: 100,
            seq: 1,
            token: 99,
            period_vns: 0,
        },
        TimerEntry {
            deadline_vns: 100,
            seq: 2,
            token: 7,
            period_vns: 0,
        },
    ];
    let s = with_timers(canonical.clone(), 6);
    let bytes = s.encode().unwrap();
    let decoded = VmState::decode(&bytes).unwrap();
    assert_eq!(decoded.timers.entries, canonical);
    assert_eq!(decoded, s, "canonical queue round-trips exactly");
}

#[test]
fn out_of_order_timer_queue_is_rejected() {
    // Same set as above but listed out of (deadline_vns, seq) order. `encode`
    // must REJECT it (not silently sort) so the round-trip contract can't be
    // quietly violated.
    let out_of_order = vec![
        TimerEntry {
            deadline_vns: 100,
            seq: 2,
            token: 7,
            period_vns: 0,
        },
        TimerEntry {
            deadline_vns: 50,
            seq: 5,
            token: 8,
            period_vns: 7,
        },
        TimerEntry {
            deadline_vns: 100,
            seq: 1,
            token: 99,
            period_vns: 0,
        },
    ];
    assert_eq!(
        with_timers(out_of_order, 6).encode(),
        Err(VmStateError::InvalidField)
    );
}

#[test]
fn duplicate_key_timer_queue_is_rejected() {
    // Two entries share (deadline_vns, seq) — not unique, so rejected.
    let dup = vec![
        TimerEntry {
            deadline_vns: 100,
            seq: 1,
            token: 7,
            period_vns: 0,
        },
        TimerEntry {
            deadline_vns: 100,
            seq: 1,
            token: 9,
            period_vns: 0,
        },
    ];
    assert_eq!(
        with_timers(dup, 2).encode(),
        Err(VmStateError::InvalidField)
    );
}

#[test]
fn duplicate_token_timer_queue_is_rejected() {
    // Canonical keys, but token 7 appears twice — task-05's token->entry index
    // would be ambiguous, so `encode` rejects it.
    let dup_token = vec![
        TimerEntry {
            deadline_vns: 100,
            seq: 0,
            token: 7,
            period_vns: 0,
        },
        TimerEntry {
            deadline_vns: 200,
            seq: 1,
            token: 7,
            period_vns: 0,
        },
    ];
    assert_eq!(
        with_timers(dup_token, 2).encode(),
        Err(VmStateError::InvalidField)
    );
}

#[test]
fn seq_at_or_above_next_seq_is_rejected() {
    // seq 5 == next_seq 5 → a restored queue would reuse seq 5 for its next
    // same-deadline insertion, colliding. Rejected (and seq > next_seq likewise).
    let collide = vec![TimerEntry {
        deadline_vns: 100,
        seq: 5,
        token: 7,
        period_vns: 0,
    }];
    assert_eq!(
        with_timers(collide, 5).encode(),
        Err(VmStateError::InvalidField)
    );
}

/// Small (deadline, seq, token) key space so order violations, duplicate keys,
/// duplicate tokens, and seq/next_seq collisions all occur frequently.
fn arb_timer_entry() -> impl Strategy<Value = TimerEntry> {
    (0u64..4, 0u64..4, 0u64..4, any::<u64>()).prop_map(|(deadline_vns, seq, token, period_vns)| {
        TimerEntry {
            deadline_vns,
            seq,
            token,
            period_vns,
        }
    })
}

/// Whether `entries` + `next_seq` satisfy all three task-05 TimerQueue
/// invariants `encode` enforces (mirror of `validate_timers`).
fn timers_valid(entries: &[TimerEntry], next_seq: u64) -> bool {
    let ascending_unique_keys = entries
        .windows(2)
        .all(|w| (w[0].deadline_vns, w[0].seq) < (w[1].deadline_vns, w[1].seq));
    let seq_below_next = entries.iter().all(|e| e.seq < next_seq);
    let mut tokens = BTreeSet::new();
    let unique_tokens = entries.iter().all(|e| tokens.insert(e.token));
    ascending_unique_keys && seq_below_next && unique_tokens
}

proptest! {
    #![proptest_config(config(256))]

    /// Encoding any state is a pure function of the state.
    #[test]
    fn encode_is_pure(s in arb_vm_state()) {
        prop_assert_eq!(s.encode().unwrap(), s.encode().unwrap());
    }

    /// `encode` accepts a timer queue iff it satisfies every task-05 invariant
    /// (canonical (deadline, seq) order, unique tokens, seq < next_seq) — never
    /// silently fixing one — and every accepted queue round-trips exactly.
    #[test]
    fn encode_accepts_iff_timers_valid(
        mut entries in prop::collection::vec(arb_timer_entry(), 0..8),
        canonicalize in any::<bool>(),
        next_seq in 0u64..8,
    ) {
        if canonicalize {
            entries.sort_by_key(|e| (e.deadline_vns, e.seq));
            entries.dedup_by_key(|e| (e.deadline_vns, e.seq));
        }
        let valid = timers_valid(&entries, next_seq);
        let s = with_timers(entries, next_seq);
        match s.encode() {
            Ok(bytes) => {
                prop_assert!(valid, "encode accepted an invalid timer queue");
                prop_assert_eq!(VmState::decode(&bytes).unwrap(), s);
            }
            Err(VmStateError::InvalidField) => {
                prop_assert!(!valid, "encode rejected a valid timer queue");
            }
            Err(other) => prop_assert!(false, "unexpected error: {:?}", other),
        }
    }

    /// Starting from a valid queue, injecting EITHER a duplicate token OR a
    /// `seq == next_seq` makes `encode` reject it with InvalidField — covering
    /// both task-05 invariants beyond canonical ordering.
    #[test]
    fn encode_rejects_duplicate_token_or_high_seq(
        base in arb_timers(),
        inject_token_dup in any::<bool>(),
    ) {
        prop_assume!(base.entries.len() >= 2);
        // arb_timers yields a valid queue, so the baseline encodes.
        prop_assert!(with_timers(base.entries.clone(), base.next_seq).encode().is_ok());

        let mut entries = base.entries.clone();
        if inject_token_dup {
            // (a) duplicate token: copy entry 0's token onto entry 1. Keys, seqs,
            //     and next_seq stay valid, so ONLY token-uniqueness is violated.
            entries[1].token = entries[0].token;
        } else {
            // (b) seq >= next_seq: bump the last (largest-key) entry's seq to
            //     next_seq. It stays the largest key, so ONLY seq < next_seq fails.
            let last = entries.len() - 1;
            entries[last].seq = base.next_seq;
        }
        prop_assert_eq!(
            with_timers(entries, base.next_seq).encode(),
            Err(VmStateError::InvalidField)
        );
    }
}
