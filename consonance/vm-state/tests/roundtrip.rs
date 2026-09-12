// SPDX-License-Identifier: AGPL-3.0-or-later

mod common;

use common::{arb_vm_state, config};
use proptest::prelude::*;
use vm_state::VmState;

proptest! {
    #![proptest_config(config(512))]

    #[test]
    fn roundtrip(s in arb_vm_state()) {
        let bytes = s.encode().expect("an integer-ratio VmState always encodes");
        let back = VmState::decode(&bytes).expect("a freshly encoded blob always decodes");
        prop_assert_eq!(back, s);
    }
}
