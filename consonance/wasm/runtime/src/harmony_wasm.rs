// SPDX-License-Identifier: AGPL-3.0-or-later
pub use crate::core::wasm::*;
fn canonical_f32(value: f32) -> f32 {
    if value.is_nan() {
        f32::from_bits(0x7fc00000)
    } else {
        value
    }
}
pub fn f32_add(lhs: f32, rhs: f32) -> f32 {
    canonical_f32(crate::core::wasm::f32_add(lhs, rhs))
}
pub fn f32_sub(lhs: f32, rhs: f32) -> f32 {
    canonical_f32(crate::core::wasm::f32_sub(lhs, rhs))
}
pub fn f32_mul(lhs: f32, rhs: f32) -> f32 {
    canonical_f32(crate::core::wasm::f32_mul(lhs, rhs))
}
pub fn f32_div(lhs: f32, rhs: f32) -> f32 {
    canonical_f32(crate::core::wasm::f32_div(lhs, rhs))
}
pub fn f32_min(lhs: f32, rhs: f32) -> f32 {
    canonical_f32(crate::core::wasm::f32_min(lhs, rhs))
}
pub fn f32_max(lhs: f32, rhs: f32) -> f32 {
    canonical_f32(crate::core::wasm::f32_max(lhs, rhs))
}
pub fn f32_ceil(value: f32) -> f32 {
    canonical_f32(crate::core::wasm::f32_ceil(value))
}
pub fn f32_floor(value: f32) -> f32 {
    canonical_f32(crate::core::wasm::f32_floor(value))
}
pub fn f32_trunc(value: f32) -> f32 {
    canonical_f32(crate::core::wasm::f32_trunc(value))
}
pub fn f32_nearest(value: f32) -> f32 {
    canonical_f32(crate::core::wasm::f32_nearest(value))
}
pub fn f32_sqrt(value: f32) -> f32 {
    canonical_f32(crate::core::wasm::f32_sqrt(value))
}
fn canonical_f64(value: f64) -> f64 {
    if value.is_nan() {
        f64::from_bits(0x7ff8000000000000)
    } else {
        value
    }
}
pub fn f64_add(lhs: f64, rhs: f64) -> f64 {
    canonical_f64(crate::core::wasm::f64_add(lhs, rhs))
}
pub fn f64_sub(lhs: f64, rhs: f64) -> f64 {
    canonical_f64(crate::core::wasm::f64_sub(lhs, rhs))
}
pub fn f64_mul(lhs: f64, rhs: f64) -> f64 {
    canonical_f64(crate::core::wasm::f64_mul(lhs, rhs))
}
pub fn f64_div(lhs: f64, rhs: f64) -> f64 {
    canonical_f64(crate::core::wasm::f64_div(lhs, rhs))
}
pub fn f64_min(lhs: f64, rhs: f64) -> f64 {
    canonical_f64(crate::core::wasm::f64_min(lhs, rhs))
}
pub fn f64_max(lhs: f64, rhs: f64) -> f64 {
    canonical_f64(crate::core::wasm::f64_max(lhs, rhs))
}
pub fn f64_ceil(value: f64) -> f64 {
    canonical_f64(crate::core::wasm::f64_ceil(value))
}
pub fn f64_floor(value: f64) -> f64 {
    canonical_f64(crate::core::wasm::f64_floor(value))
}
pub fn f64_trunc(value: f64) -> f64 {
    canonical_f64(crate::core::wasm::f64_trunc(value))
}
pub fn f64_nearest(value: f64) -> f64 {
    canonical_f64(crate::core::wasm::f64_nearest(value))
}
pub fn f64_sqrt(value: f64) -> f64 {
    canonical_f64(crate::core::wasm::f64_sqrt(value))
}
pub fn f32_demote_f64(value: f64) -> f32 {
    if value.is_nan() {
        f32::from_bits(0x7fc00000)
    } else {
        crate::core::wasm::f32_demote_f64(value)
    }
}
pub fn f64_promote_f32(value: f32) -> f64 {
    if value.is_nan() {
        f64::from_bits(0x7ff8000000000000)
    } else {
        crate::core::wasm::f64_promote_f32(value)
    }
}
