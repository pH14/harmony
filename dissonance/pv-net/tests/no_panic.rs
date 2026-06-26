// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 1 — `parse`, `on_tx`, and `restore_state` never panic and never read
//! out of bounds on arbitrary/truncated/mutated bytes. (The `cargo-fuzz` target
//! in `fuzz/` is the deeper, continuous version of these properties.)

mod common;

use common::{BROADCAST, SeededOracle, config, mac, node_map};
use proptest::prelude::*;
use pv_net::{Switch, VTime, parse};

proptest! {
    #![proptest_config(config(512))]

    /// `parse` tolerates any byte string.
    #[test]
    fn parse_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..2048)) {
        let nodes = node_map(4);
        let _ = parse(&bytes, &nodes);
    }

    /// `on_tx` — the guest-controlled TX entry point — never panics, and an
    /// unparseable frame is dropped (empty result).
    #[test]
    fn on_tx_never_panics_and_drops_malformed(
        now in any::<u64>(),
        bytes in prop::collection::vec(any::<u8>(), 0..2048),
        seed in any::<u64>(),
    ) {
        let mut s = Switch::new(node_map(4), VTime(100));
        let mut oracle = SeededOracle::new(seed);
        let out = s.on_tx(VTime(now), bytes.clone(), &mut oracle);
        if parse(&bytes, &node_map(4)).is_none() {
            prop_assert!(out.is_empty(), "a frame that fails parse is dropped");
        }
        let _ = s.due(VTime(now)); // draining must not panic either
    }

    /// A valid L2 header with an arbitrary tail (which may look like IPv4/TCP)
    /// drives the scheduling paths — including broadcast — without panicking.
    #[test]
    fn on_tx_with_structured_frames_never_panics(
        now in any::<u64>(),
        src in 0u8..4,
        dst in 0u8..4,
        broadcast in any::<bool>(),
        tail in prop::collection::vec(any::<u8>(), 0..300),
        seed in any::<u64>(),
    ) {
        let dst_mac = if broadcast { BROADCAST } else { mac(dst) };
        let mut f = Vec::new();
        f.extend_from_slice(&dst_mac);
        f.extend_from_slice(&mac(src));
        f.extend_from_slice(&tail);

        let mut s = Switch::new(node_map(4), VTime(100));
        let mut oracle = SeededOracle::new(seed);
        let _ = s.on_tx(VTime(now), f, &mut oracle);
        let _ = s.due(VTime(now.wrapping_add(1)));
        let _ = s.save_state(); // serialization of any reachable state is total
    }

    /// `restore_state` returns a `Result` for any bytes — never a panic.
    #[test]
    fn restore_state_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..2048)) {
        let mut s = Switch::new(node_map(4), VTime(100));
        let _ = s.restore_state(&bytes);
    }
}
