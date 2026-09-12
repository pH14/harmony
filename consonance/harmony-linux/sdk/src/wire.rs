// SPDX-License-Identifier: AGPL-3.0-or-later

pub const SDK_WIRE_VERSION: u8 = 1;

pub const NS_SHIFT: u32 = 24;
pub const LOCAL_MASK: u32 = (1 << NS_SHIFT) - 1;
pub const LOCAL_MAX: u32 = LOCAL_MASK;

pub const NS_CONTROL: u8 = 0;
pub const NS_ASSERT: u8 = 1;
pub const NS_STATE: u8 = 2;
pub const NS_BUGGIFY: u8 = 3;
pub const NS_LIFECYCLE: u8 = 4;

pub const CATALOG_EVENT_ID: u32 = 0;
pub const SETUP_COMPLETE_EVENT_ID: u32 = (NS_LIFECYCLE as u32) << NS_SHIFT;
pub const FRAME_COMPLETE_EVENT_ID: u32 = ((NS_LIFECYCLE as u32) << NS_SHIFT) | 1;

pub const CATALOG_MAGIC: u32 = u32::from_le_bytes(*b"SDKC");

pub const DISP_HIT: u8 = 0;
pub const DISP_VIOLATION: u8 = 1;

pub const STATE_SET: u8 = 0;
pub const STATE_MAX: u8 = 1;

pub const KIND_ALWAYS: u8 = 0;
pub const KIND_SOMETIMES: u8 = 1;
pub const KIND_REACHABLE: u8 = 2;
pub const KIND_UNREACHABLE: u8 = 3;
pub const KIND_STATE: u8 = 4;
pub const KIND_BUGGIFY: u8 = 5;

#[inline]
pub const fn event_id(ns: u8, local: u32) -> u32 {
    ((ns as u32) << NS_SHIFT) | (local & LOCAL_MASK)
}

#[inline]
pub const fn split(event_id: u32) -> (u8, u32) {
    ((event_id >> NS_SHIFT) as u8, event_id & LOCAL_MASK)
}
