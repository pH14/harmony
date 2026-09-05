// SPDX-License-Identifier: AGPL-3.0-or-later
//! Measure the host cost of one arm64 KVM doorbell MMIO round trip.
//!
//! The payload is deliberately tiny and deterministic: it stores a zero-length
//! request at the board doorbell GPA forever. The VMM answers that malformed
//! request with the normal framed transport error, so every timed `step` is a
//! complete guest-store → `KVM_EXIT_MMIO` → host-dispatch → MMIO-completion
//! round trip without requiring a Linux image or an initramfs.

#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
use sha2::{Digest, Sha256};

#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
use vmm_core::vmm::Step;

#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
const DEFAULT_ITERATIONS: u64 = 1_000;

#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
const DEFAULT_SEED: u64 = 0;

#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
const GUEST_RAM: usize = 16 * 1024 * 1024;

#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
const DOORBELL_VALUE: u32 = 0;

#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
fn parse_u64(value: &str, name: &str) -> Result<u64, String> {
    let (radix, digits) = value
        .strip_prefix("0x")
        .map_or((10, value), |digits| (16, digits));
    if digits.is_empty() {
        return Err(format!("{name} must be a non-empty integer"));
    }
    u64::from_str_radix(digits, radix).map_err(|_| format!("{name} must be an integer: {value}"))
}

#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
fn parse_source_commit(value: &str) -> Result<String, String> {
    const MAX_SOURCE_COMMIT_LEN: usize = 128;
    if value.is_empty()
        || value.len() > MAX_SOURCE_COMMIT_LEN
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/'))
    {
        return Err(
            "source_commit must be one non-empty safe token (ASCII letters, digits, and -_./)"
                .to_string(),
        );
    }
    Ok(value.to_string())
}

#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
fn arguments() -> Result<(u64, u64, String), String> {
    let mut args = std::env::args();
    let _program = args.next();
    let iterations = match args.next() {
        Some(value) => {
            let iterations = parse_u64(&value, "iterations")?;
            if iterations == 0 {
                return Err("iterations must be positive".to_string());
            }
            iterations
        }
        None => DEFAULT_ITERATIONS,
    };
    let seed = match args.next() {
        Some(value) => parse_u64(&value, "seed")?,
        None => DEFAULT_SEED,
    };
    let source_commit = match args.next() {
        Some(value) => parse_source_commit(&value)?,
        None => "builtin".to_string(),
    };
    if args.next().is_some() {
        return Err(
            "usage: kvm_arm64_doorbell_bench [iterations] [seed|0xseed] [source_commit]"
                .to_string(),
        );
    }
    Ok((iterations, seed, source_commit))
}

#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
fn guest_image() -> Vec<u8> {
    // The Image loader enters at code0, which branches over the 64-byte Image
    // header. The payload then sets x0 = 0x0a00_0000, writes w1 == 0 to the
    // doorbell, and branches back to that store forever. These are fixed
    // AArch64 encodings; no assembler or host-derived bytes are involved.
    const CODE: [u32; 4] = [
        0xd2a1_4000, // movz x0, #0x0a00, lsl #16
        0x5280_0001, // mov  w1, #0
        0xb900_0001, // str  w1, [x0]
        0x17ff_ffff, // b    #-4 (back to str)
    ];
    let mut code = Vec::with_capacity(CODE.len() * 4);
    for word in CODE {
        code.extend_from_slice(&word.to_le_bytes());
    }
    vmm_core::vendor::arm64::image_loader::wrap_image(&code, 0, 0)
}

#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
fn guard_hash(
    seed: u64,
    iterations: u64,
    source_commit: &str,
    doorbell_exits: u64,
    image: &[u8],
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"harmony-arm64-doorbell-bench-v1\0");
    hasher.update(seed.to_le_bytes());
    hasher.update(iterations.to_le_bytes());
    hasher.update(
        u64::try_from(source_commit.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    hasher.update(source_commit.as_bytes());
    hasher.update(doorbell_exits.to_le_bytes());
    hasher.update(vmm_core::vendor::arm64::board::DOORBELL.0.to_le_bytes());
    hasher.update(DOORBELL_VALUE.to_le_bytes());
    hasher.update(image);
    hasher.finalize().into()
}

#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
fn format_micros(nanoseconds: u128) -> String {
    format!("{}.{:03}", nanoseconds / 1_000, nanoseconds % 1_000)
}

#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
fn main() -> std::process::ExitCode {
    let (iterations, seed, source_commit) = match arguments() {
        Ok(values) => values,
        Err(error) => {
            eprintln!("{error}");
            return std::process::ExitCode::from(2);
        }
    };
    let image = guest_image();
    let mut vmm =
        match vmm_core::vendor::arm64::bringup::boot_selected_control(&image, &[], "", GUEST_RAM) {
            Ok(vmm) => vmm,
            Err(error) => {
                eprintln!("KVM arm64 doorbell benchmark setup failed: {error:?}");
                return std::process::ExitCode::FAILURE;
            }
        };
    if let Err(error) = vmm.reseed_entropy(seed) {
        eprintln!("KVM arm64 doorbell benchmark seed setup failed: {error}");
        return std::process::ExitCode::FAILURE;
    }
    let initial_doorbell_exits = vmm.doorbell_exits();

    // This is host-only measurement. It brackets no composition, state-hash,
    // or guard work; the wall clock never enters the VMM or guest state.
    #[allow(clippy::disallowed_methods)]
    let started = std::time::Instant::now();
    for iteration in 0..iterations {
        match vmm.step() {
            Ok(Step::Continued) => {}
            Ok(step) => {
                eprintln!("benchmark stopped at iteration {iteration}: {step:?}");
                return std::process::ExitCode::FAILURE;
            }
            Err(error) => {
                eprintln!("benchmark failed at iteration {iteration}: {error}");
                return std::process::ExitCode::FAILURE;
            }
        }
    }
    #[allow(clippy::disallowed_methods)]
    let elapsed = started.elapsed();

    let counts = vmm.exit_counts();
    let final_doorbell_exits = vmm.doorbell_exits();
    let Some(doorbell_exits) = final_doorbell_exits.checked_sub(initial_doorbell_exits) else {
        eprintln!(
            "doorbell exit counter regressed: before={initial_doorbell_exits} \
             after={final_doorbell_exits}"
        );
        return std::process::ExitCode::FAILURE;
    };
    if doorbell_exits != iterations || counts.mmio != iterations || counts.total() != iterations {
        eprintln!(
            "doorbell exit-count mismatch: expected={iterations} doorbell={doorbell_exits} \
             mmio={} total={}",
            counts.mmio,
            counts.total()
        );
        return std::process::ExitCode::FAILURE;
    }
    let guard = guard_hash(seed, iterations, &source_commit, doorbell_exits, &image);
    let state_hash = vmm.state_hash();
    let elapsed_ns = elapsed.as_nanos();
    let elapsed_us = format_micros(elapsed_ns);
    let per_exit_us = format_micros(elapsed_ns / u128::from(iterations));
    println!(
        "format=consonance.arm64-doorbell-bench.v1 seed={seed} \
         source_commit={source_commit} iterations={iterations} exits={} \
         doorbell_exits={doorbell_exits} mmio_exits={} elapsed_us={elapsed_us} \
         microseconds_per_exit={per_exit_us} doorbell_gpa={:#x} request_len={} guard_hash={} \
         state_hash={}",
        counts.total(),
        counts.mmio,
        vmm_core::vendor::arm64::board::DOORBELL.0,
        DOORBELL_VALUE,
        hex(&guard),
        hex(&state_hash),
    );
    std::process::ExitCode::SUCCESS
}

#[cfg(not(all(target_os = "linux", target_arch = "aarch64", not(miri))))]
fn main() -> std::process::ExitCode {
    eprintln!("kvm_arm64_doorbell_bench requires a Linux/aarch64 host with /dev/kvm outside Miri");
    std::process::ExitCode::from(2)
}
