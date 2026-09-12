// SPDX-License-Identifier: AGPL-3.0-or-later

use harmony_sdk::Point;

pub const REG_GAME_MODE: u32 = 1;
pub const REG_WORLD: u32 = 2;
pub const REG_LEVEL: u32 = 3;
pub const REG_X_BUCKET: u32 = 4;
pub const REG_POWERUP: u32 = 5;
pub const REG_DEPTH: u32 = 6;
pub const REG_FRAME: u32 = 7;
pub const REG_BILLBOARD_GPA: u32 = 8;
pub const REG_BILLBOARD_LEN: u32 = 9;

pub const POINT_LEVEL_CLEARED: u32 = 1;
pub const POINT_WORLD_TWO: u32 = 2;

pub const CATALOG: &[Point] = &[
    Point::state(REG_GAME_MODE, "smb_game_mode"),
    Point::state(REG_WORLD, "smb_world"),
    Point::state(REG_LEVEL, "smb_level"),
    Point::state(REG_X_BUCKET, "smb_x_bucket"),
    Point::state(REG_POWERUP, "smb_powerup"),
    Point::state(REG_DEPTH, "smb_depth"),
    Point::state(REG_FRAME, "smb_frame"),
    Point::state(REG_BILLBOARD_GPA, "smb_billboard_gpa"),
    Point::state(REG_BILLBOARD_LEN, "smb_billboard_len"),
    Point::reachable(POINT_LEVEL_CLEARED, "smb_level_cleared"),
    Point::reachable(POINT_WORLD_TWO, "smb_world_two"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_ids_fit_the_pack_state_bound() {
        for reg in [
            REG_GAME_MODE,
            REG_WORLD,
            REG_LEVEL,
            REG_X_BUCKET,
            REG_POWERUP,
            REG_DEPTH,
            REG_FRAME,
            REG_BILLBOARD_GPA,
            REG_BILLBOARD_LEN,
        ] {
            assert!(reg < (1 << 16), "reg {reg} would alias in pack_state");
        }
    }

    #[test]
    fn catalog_passes_sdk_validation() {
        struct NoTransport;
        impl hypercall_proto::Transport for NoTransport {
            type Error = ();
            fn exchange(&mut self, _req: &[u8], _resp: &mut [u8]) -> Result<usize, ()> {
                Err(())
            }
        }
        match harmony_sdk::Sdk::init(NoTransport, CATALOG) {
            Ok(_) => panic!("init cannot succeed over the failing transport"),
            Err(err) => assert!(
                matches!(err, harmony_sdk::SdkError::Client(_)),
                "catalog was rejected before reaching the transport: {err:?}"
            ),
        }
    }
}
