// SPDX-License-Identifier: AGPL-3.0-or-later
//! The play-agent's state-register and assertion-point catalog.
//!
//! Register ids are `NS_STATE` local ids, private to this agent (there is no
//! central registry; each agent owns its catalog — the `sdk-demo` precedent).
//! They are kept **below 2^16** because the host-side `LinkSensor` packs
//! `(reg, value)` into one 64-bit feature id as `(reg & 0xFFFF) << 48 |
//! (value & 0xFFFF_FFFF_FFFF)` (`dissonance/link/src/sensor.rs::pack_state`) —
//! a register id past 16 bits would alias, and a value past 48 bits would
//! truncate (the billboard GPA comfortably fits 48 bits on this guest's RAM
//! sizes).
//!
//! The host-side campaign keys cells on `(REG_GAME_MODE, REG_WORLD, REG_LEVEL,
//! REG_X_BUCKET)` — the analog of Antithesis's discretized `(x, y)` tuple.
//! `REG_FRAME` is the frame clock task 87 (`film`) addresses film frames by;
//! `REG_BILLBOARD_GPA`/`REG_BILLBOARD_LEN` publish the billboard buffer once at
//! init.

use harmony_sdk::Point;

/// `OperMode` — 0 title, 1 gameplay, 2 victory, 3 game over (`state_set`,
/// once per input window).
pub const REG_GAME_MODE: u32 = 1;
/// 0-indexed world number (`state_set`, once per window).
pub const REG_WORLD: u32 = 2;
/// 0-indexed level ("dash") number (`state_set`, once per window).
pub const REG_LEVEL: u32 = 3;
/// Absolute X bucketed by the manifest's bucket width (`state_set`, once per
/// window).
pub const REG_X_BUCKET: u32 = 4;
/// `PlayerStatus` — 0 small, 1 big, 2 fiery (`state_set`, once per window).
pub const REG_POWERUP: u32 = 5;
/// Furthest `(world, level)` ordinal reached (`state_max` — the host mints
/// novelty only on a genuine increase; warp-zone jumps are legitimate).
pub const REG_DEPTH: u32 = 6;
/// The frame counter, emitted **every vblank** (`state_set`) — the frame clock
/// task 87 addresses film frames by.
pub const REG_FRAME: u32 = 7;
/// The billboard buffer's guest-physical address (`state_set`, once at init).
pub const REG_BILLBOARD_GPA: u32 = 8;
/// The billboard buffer's total length in bytes (`state_set`, once at init).
pub const REG_BILLBOARD_LEN: u32 = 9;

/// Legibility marker: first flagpole — any level cleared (the `(world, level)`
/// ordinal rose above its starting value during gameplay). A marker, not a bug
/// (task 84's ruling; zero fault vocabulary).
pub const POINT_LEVEL_CLEARED: u32 = 1;
/// Legibility marker: reached any world ≥ 2 (by castle *or* by warp zone; both
/// are real).
pub const POINT_WORLD_TWO: u32 = 2;

/// The declared point set, registered in one Emit at `Sdk::init`.
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

    /// Every register id must stay below 2^16 (the `pack_state` aliasing bound
    /// documented above).
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

    /// The catalog must be accepted by the SDK's init-time validation
    /// (unique coordinates, unique names, ids within the 24-bit local space) —
    /// proven by driving `Sdk::init` over an in-process loopback-free check:
    /// the SDK validates before any transport write, so a failing transport
    /// distinguishes catalog rejection from emission.
    #[test]
    fn catalog_passes_sdk_validation() {
        struct NoTransport;
        impl hypercall_proto::Transport for NoTransport {
            type Error = ();
            fn exchange(&mut self, _req: &[u8], _resp: &mut [u8]) -> Result<usize, ()> {
                // Reached only after catalog validation passed; the declare
                // emission itself fails here, which the assertion tells apart.
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
