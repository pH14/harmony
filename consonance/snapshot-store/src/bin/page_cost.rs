// SPDX-License-Identifier: AGPL-3.0-or-later
//! Measure the host cost of copying, hashing, and zero-checking one guest page.
//!
//! This is an informational benchmark for the item-0 Consonance cost projection. It
//! deliberately measures wall-clock time only around the hot loops; none of the values
//! participate in snapshot state or deterministic execution.

// not order-observable: wall time is printed as benchmark evidence only.
#![allow(clippy::disallowed_methods)]

use std::hint::black_box;
use std::time::{Duration, Instant};

use snapshot_store::PAGE_SIZE;

/// Repetitions are large enough to amortize timer and loop overhead on the target host.
const REPETITIONS: u64 = 100_000;
/// Warm the instruction and data paths before starting each timed interval.
const WARMUP_REPETITIONS: u64 = 2_000;

#[derive(Debug)]
struct Measurement {
    elapsed: Duration,
    guard: u64,
}

/// Build deterministic, non-zero page contents without using host entropy.
fn fixed_page() -> [u8; PAGE_SIZE] {
    let mut page = [0u8; PAGE_SIZE];
    for (index, byte) in page.iter_mut().enumerate() {
        *byte = (index as u32)
            .wrapping_mul(251)
            .wrapping_add(17)
            .to_le_bytes()[0];
    }
    page
}

/// Format elapsed time as truncated microseconds per page with three decimals.
fn micros_per_page(elapsed: Duration, pages: u64) -> String {
    let nanos_per_page = elapsed.as_nanos() / u128::from(pages);
    format!("{}.{:03}", nanos_per_page / 1_000, nanos_per_page % 1_000)
}

/// Return a compact guard derived from a completed page operation.
fn guard_from_digest(digest: &blake3::Hash) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest.as_bytes()[..8]);
    u64::from_le_bytes(bytes)
}

fn measure_memcpy(source: &[u8; PAGE_SIZE]) -> Measurement {
    let mut destination = [0u8; PAGE_SIZE];
    for _ in 0..WARMUP_REPETITIONS {
        black_box(destination.as_mut_slice()).copy_from_slice(black_box(source.as_slice()));
    }

    let start = Instant::now();
    for _ in 0..REPETITIONS {
        black_box(destination.as_mut_slice()).copy_from_slice(black_box(source.as_slice()));
    }
    let elapsed = start.elapsed();
    let digest = blake3::hash(black_box(destination.as_slice()));
    Measurement {
        elapsed,
        guard: guard_from_digest(&digest),
    }
}

fn measure_blake3(source: &[u8; PAGE_SIZE]) -> Measurement {
    for _ in 0..WARMUP_REPETITIONS {
        black_box(blake3::hash(black_box(source.as_slice())));
    }

    let start = Instant::now();
    let mut last_digest = [0u8; 32];
    for _ in 0..REPETITIONS {
        let digest = blake3::hash(black_box(source.as_slice()));
        last_digest = *digest.as_bytes();
        black_box(&last_digest);
    }
    let elapsed = start.elapsed();
    let mut guard_bytes = [0u8; 8];
    guard_bytes.copy_from_slice(&last_digest[..8]);
    Measurement {
        elapsed,
        guard: u64::from_le_bytes(guard_bytes),
    }
}

fn measure_zero_check(zero_page: &[u8; PAGE_SIZE]) -> Measurement {
    for _ in 0..WARMUP_REPETITIONS {
        black_box(zero_page.as_slice())
            .iter()
            .all(|&byte| byte == 0);
    }

    let start = Instant::now();
    let mut is_zero = false;
    for _ in 0..REPETITIONS {
        is_zero = black_box(zero_page.as_slice())
            .iter()
            .all(|&byte| byte == 0);
        black_box(is_zero);
    }
    let elapsed = start.elapsed();
    Measurement {
        elapsed,
        guard: u64::from(is_zero),
    }
}

fn print_measurement(name: &str, measurement: Measurement) {
    println!(
        "operation={name} elapsed_ns={} us_per_page={} guard={}",
        measurement.elapsed.as_nanos(),
        micros_per_page(measurement.elapsed, REPETITIONS),
        measurement.guard
    );
}

fn main() {
    let source = fixed_page();
    let zero_page = [0u8; PAGE_SIZE];
    let measured_bytes = u64::try_from(PAGE_SIZE).unwrap_or(u64::MAX) * REPETITIONS;

    println!(
        "benchmark=page_cost version=1 page_bytes={PAGE_SIZE} repetitions={REPETITIONS} \
         warmup_repetitions={WARMUP_REPETITIONS} measured_bytes={measured_bytes}"
    );
    print_measurement("memcpy", measure_memcpy(&source));
    print_measurement("blake3", measure_blake3(&source));
    print_measurement("zero_check", measure_zero_check(&zero_page));
}

#[cfg(test)]
mod tests {
    use super::micros_per_page;
    use std::time::Duration;

    #[test]
    fn formats_truncated_microseconds_per_page() {
        assert_eq!(micros_per_page(Duration::from_nanos(123_456), 4), "30.864");
        assert_eq!(micros_per_page(Duration::ZERO, 1), "0.000");
    }
}
