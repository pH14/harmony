// SPDX-License-Identifier: AGPL-3.0-or-later

use hypercall_proto::observation::{DESCRIPTOR_LEN, InvalidDescriptor, MAX_LEN, MAX_REGIONS};

#[test]
fn observation_public_limits_and_error_text_are_stable() {
    assert_eq!(MAX_LEN, 2_097_152);
    assert_eq!(MAX_REGIONS, 16);
    assert_eq!(DESCRIPTOR_LEN, 24);
    assert_eq!(
        InvalidDescriptor.to_string(),
        "invalid observation descriptor"
    );
}
