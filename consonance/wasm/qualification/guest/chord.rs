// SPDX-License-Identifier: AGPL-3.0-or-later
#![no_std]

#[unsafe(no_mangle)]
pub extern "C" fn harmony_chord(frame: u32) -> u32 {
    match frame {
        60..=65 | 180..=185 => 1 << 3,
        240..=245 | 432..=437 => 1,
        300..=305 | 312..=317 | 324..=329 => 1 << 4,
        504.. => (1 << 7) | 1,
        _ => 0,
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}
