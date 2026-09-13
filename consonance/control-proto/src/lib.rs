// SPDX-License-Identifier: AGPL-3.0-or-later

mod codec;
mod error;
mod types;

pub use codec::{decode_reply, decode_request, encode_reply, encode_request};
pub use error::{ControlError, ProtocolError};
pub use types::{
    Answer, CapFlags, Caps, CoverageGeometry, CrashInfo, CrashKind, DecisionId, EventRef,
    HashScope, HostFault, Moment, RegsView, Reply, Reproducer, Request, Resolution, SnapId,
    StopConditions, StopMask, StopReason, class_bit,
};

pub const PROTO_VERSION: u16 = 1;

pub const APP_PROTOCOL_VERSION: u16 = 12;

pub const READ_CAP: u32 = 1 << 18;

pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_constants_are_pinned() {
        assert_eq!(MAX_FRAME_LEN, 16_777_216);
        assert_eq!(PROTO_VERSION, 1);
        assert_eq!(APP_PROTOCOL_VERSION, 12);
        assert_eq!(READ_CAP, 262_144);
    }
}
