// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 1 — round-trip identity over arbitrary encodable `VmState`s.

mod common;

use common::{arb_vm_state, config};
use proptest::prelude::*;
use vm_state::VmState;

proptest! {
    #![proptest_config(config(512))]

    /// `decode(&encode(s).unwrap()) == Ok(s)` for every constructible,
    /// integer-ratio `VmState`.
    #[test]
    fn roundtrip(s in arb_vm_state()) {
        let bytes = s.encode().expect("an integer-ratio VmState always encodes");
        let back = VmState::decode(&bytes).expect("a freshly encoded blob always decodes");
        prop_assert_eq!(back, s);
    }
}
