// SPDX-License-Identifier: AGPL-3.0-or-later
//! Box-only P6 determinism gate (`#[cfg(target_os = "linux")]` **and `#[ignore]`**):
//! the whole task-21 point. A bare-metal payload executes
//! `RDTSC`/`RDTSCP`/`RDRAND`/`RDSEED` on the **patched** KVM via
//! [`PatchedKvmBackend`], driven through the real vmm-core determinism stack
//! (`boot_selected(BackendKind::Patched, …)` → V-time TSC + seeded RNG), and:
//!
//! - **deterministic twice** — two runs from the same seed produce a
//!   bit-identical `state_hash` and identical guest memory;
//! - **RDTSC/RDTSCP** read a **V-time** TSC (`VClock::guest_ticks(work)` = `2·work`
//!   ticks here): strictly monotonic, constant per-branch delta, never the host
//!   TSC (which would be ~10¹³, not single digits); `RDTSCP`'s ECX is the
//!   contract `IA32_TSC_AUX`;
//! - **RDRAND/RDSEED** read the **seeded** stream the `Entropy` hypercall uses;
//! - **snapshot/restore mid-run resumes both clocks exactly** — a run that
//!   snapshots V-time + entropy after two reads, restores them (perf counter
//!   reset to 0, `vns_base`/stream-position restored), and continues produces a
//!   guest-memory image **identical** to the un-snapshotted reference run.
//!
//! Box-only because it needs the loaded patched `/dev/kvm`
//! (`KVM_CAP_X86_DETERMINISTIC_INTERCEPTS`), `perf_event`, and the `det-cfl-v1`
//! host. `#[ignore]`d out of the default lane (like `live_m1_m2.rs`): default CI
//! shows it **not-run**, never a vacuous green. Run on `ssh <det-box>`, CPU-pinned
//! per `docs/BOX-PINNING.md`, with the patched modules loaded:
//!
//! ```sh
//! taskset -c 2 cargo test -p vmm-core --test live_determinism -- --ignored --test-threads=1
//! ```
//!
//! Every precondition that would prevent a real run — no `/dev/kvm`, the
//! determinism cap absent (stock modules), or a non-baseline host — is a **loud
//! panic (test FAILURE)**, never an early-return `Ok`. macOS builds an empty test
//! binary; the determinism *logic* is covered there by the `MockBackend` +
//! `ScriptedWork` unit tests in `src/vmm.rs`.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use vmm_core::vendor::x86::bringup::{BackendKind, boot_selected};
use vmm_core::vmm::{Step, TerminalReason};

/// Seed for the deterministic entropy stream RDRAND/RDSEED draw from.
const SEED: u64 = 0x5EED_D31E_2026;
/// Guest RAM: 4 MiB — covers the 1 MiB payload load and the low result buffer.
const GUEST_RAM_LEN: usize = 4 << 20;
/// Where the payload writes its results (a low page, below the boot-info at
/// `0x9000` and the 1 MiB load address).
const RESULTS_GPA: usize = 0x8000;

// The deterministic payload — 32-bit protected-mode machine code (Multiboot
// hands off in 32-bit PM, paging off, flat segments; see `entry.rs`). Verified
// by disassembly (objdump -b binary -m i386):
//
//   mov edi, 0x8000      ; results pointer
//   mov ecx, 3
//  .loop:
//   rdtsc                ; EDX:EAX = V-time TSC
//   mov [edi], eax       ; store low 32 bits
//   add edi, 4
//   dec ecx
//   jnz .loop            ; the ONLY conditional branch → +1 work per iteration
//   rdtscp               ; EDX:EAX, ECX = IA32_TSC_AUX
//   mov [edi], eax       ; store TSC
//   mov [edi+4], ecx     ; store aux
//   add edi, 8
//   rdrand eax           ; seeded stream word 0 (low 32)
//   mov [edi], eax
//   add edi, 4
//   rdseed eax           ; seeded stream word 1 (low 32)
//   mov [edi], eax
//   mov al, 0
//   out 0xF4, al         ; isa-debug-exit PASS (terminal)
//   hlt                  ; fallback
#[rustfmt::skip]
const PAYLOAD_CODE: [u8; 49] = [
    0xBF, 0x00, 0x80, 0x00, 0x00, // mov edi, 0x8000
    0xB9, 0x03, 0x00, 0x00, 0x00, // mov ecx, 3
    0x0F, 0x31,                   // rdtsc
    0x89, 0x07,                   // mov [edi], eax
    0x83, 0xC7, 0x04,             // add edi, 4
    0x49,                         // dec ecx
    0x75, 0xF6,                   // jnz .loop
    0x0F, 0x01, 0xF9,             // rdtscp
    0x89, 0x07,                   // mov [edi], eax
    0x89, 0x4F, 0x04,             // mov [edi+4], ecx
    0x83, 0xC7, 0x08,             // add edi, 8
    0x0F, 0xC7, 0xF0,             // rdrand eax
    0x89, 0x07,                   // mov [edi], eax
    0x83, 0xC7, 0x04,             // add edi, 4
    0x0F, 0xC7, 0xF8,             // rdseed eax
    0x89, 0x07,                   // mov [edi], eax
    0xB0, 0x00,                   // mov al, 0
    0xE6, 0xF4,                   // out 0xF4, al
    0xF4,                         // hlt
];

const PAYLOAD_LOAD_GPA: u32 = 0x0010_0000;
const MB_HEADER_MAGIC: u32 = 0x1BAD_B002;
const MB_FLAG_ADDRESS_OVERRIDE: u32 = 1 << 16;

/// Wrap [`PAYLOAD_CODE`] in a Multiboot v1 address-override image: a 32-byte
/// header at the start of the loaded region (`header_addr == load_addr`), the
/// code immediately after, entry pointing past the header.
fn payload_image() -> Vec<u8> {
    let load = PAYLOAD_LOAD_GPA;
    let header_len = 32u32;
    let load_end = load + header_len + PAYLOAD_CODE.len() as u32;
    let entry = load + header_len; // execution starts at the code, past the header
    let checksum = 0u32
        .wrapping_sub(MB_HEADER_MAGIC)
        .wrapping_sub(MB_FLAG_ADDRESS_OVERRIDE);
    let fields = [
        MB_HEADER_MAGIC,
        MB_FLAG_ADDRESS_OVERRIDE,
        checksum,
        load,     // header_addr == load_addr ⇒ file_off = 0
        load,     // load_addr
        load_end, // load_end_addr
        load_end, // bss_end_addr (no bss)
        entry,    // entry_addr
    ];
    let mut img = Vec::with_capacity(32 + PAYLOAD_CODE.len());
    for f in fields {
        img.extend_from_slice(&f.to_le_bytes());
    }
    img.extend_from_slice(&PAYLOAD_CODE);
    img
}

/// Read a little-endian `u32` at guest-physical `gpa` out of a `state_blob`
/// (whose first chunk is `b"MEM\0" ‖ len(u64 LE) ‖ raw guest RAM`).
fn read_u32_at_gpa(blob: &[u8], gpa: usize) -> u32 {
    assert_eq!(
        &blob[0..4],
        b"MEM\0",
        "state_blob must start with the MEM chunk"
    );
    let ram = &blob[12..]; // 4 (tag) + 8 (len)
    u32::from_le_bytes(ram[gpa..gpa + 4].try_into().unwrap())
}

/// The six result words the payload writes (3×RDTSC, RDTSCP-tsc, RDTSCP-aux,
/// RDRAND, RDSEED) — except aux/rng we pull individually; here the four TSC reads.
#[derive(Debug, PartialEq, Eq)]
struct Results {
    tsc: [u32; 4], // rdtsc ×3 then rdtscp
    aux: u32,      // RDTSCP ECX = IA32_TSC_AUX
    rdrand: u32,
    rdseed: u32,
}

fn parse_results(blob: &[u8]) -> Results {
    Results {
        tsc: [
            read_u32_at_gpa(blob, RESULTS_GPA),
            read_u32_at_gpa(blob, RESULTS_GPA + 4),
            read_u32_at_gpa(blob, RESULTS_GPA + 8),
            read_u32_at_gpa(blob, RESULTS_GPA + 12),
        ],
        aux: read_u32_at_gpa(blob, RESULTS_GPA + 16),
        rdrand: read_u32_at_gpa(blob, RESULTS_GPA + 20),
        rdseed: read_u32_at_gpa(blob, RESULTS_GPA + 24),
    }
}

/// The two consecutive 4-byte draws from the seeded stream (the `Entropy`
/// hypercall convention: opcode 1, a `u32` count), recomputed independently.
fn expected_rng(seed: u64) -> (u32, u32) {
    use hypercall_proto::{SeededEntropy, Service, Status};
    let mut e = SeededEntropy::new(seed);
    let mut draw = || {
        let mut b = [0u8; 4];
        assert_eq!(e.handle(1, &4u32.to_le_bytes(), &mut b), (Status::Ok, 4));
        u32::from_le_bytes(b)
    };
    (draw(), draw())
}

/// Boot the patched backend over the payload, **panicking loudly** with a precise
/// reason if the box is not ready (no patched `/dev/kvm`, no perf, non-baseline
/// host) — never an early-return that nextest counts as a vacuous pass.
fn boot_patched_or_panic() -> vmm_core::vmm::Vmm<Box<dyn vmm_backend::Backend<A = vmm_backend::X86>>>
{
    assert!(
        std::path::Path::new("/dev/kvm").exists(),
        "/dev/kvm absent — run this `#[ignore]`d box gate on `ssh <det-box>` with the patched KVM \
         modules loaded (see consonance/vmm-backend/kvm-patches/BUILD.md), CPU-pinned per docs/BOX-PINNING.md."
    );
    match boot_selected(BackendKind::Patched, &payload_image(), GUEST_RAM_LEN, SEED) {
        Ok(vmm) => vmm,
        Err(e) => panic!(
            "boot_selected(Patched) failed: {e}. Needs the LOADED patched KVM \
             (KVM_CAP_X86_DETERMINISTIC_INTERCEPTS), perf_event, and the det-cfl-v1 host. \
             Build + load per consonance/vmm-backend/kvm-patches/BUILD.md, then revert to stock after."
        ),
    }
}

/// Run one fresh patched VM to terminal; return (state_hash, parsed results).
fn run_once() -> ([u8; 32], Results) {
    let mut vmm = boot_patched_or_panic();
    let r = vmm.run().expect("patched run to terminal");
    assert_eq!(
        r.reason,
        TerminalReason::DebugExit { code: 0 },
        "payload must end on a clean isa-debug-exit PASS"
    );
    let blob = vmm.state_blob();
    (vmm.state_hash(), parse_results(&blob))
}

#[test]
#[ignore = "box-only: needs the LOADED patched KVM + perf + det-cfl-v1 host; run on \
            `ssh <det-box>` with `-- --ignored`"]
fn p6_rdtsc_rng_are_deterministic_and_vtime_backed() {
    let (hash_a, res_a) = run_once();
    let (hash_b, res_b) = run_once();

    // Headline: bit-identical across two runs.
    assert_eq!(hash_a, hash_b, "two runs must produce identical state_hash");
    assert_eq!(
        res_a, res_b,
        "two runs must produce identical guest results"
    );

    eprintln!("[p6] tsc reads = {:?}  aux = {:#x}", res_a.tsc, res_a.aux);
    eprintln!(
        "[p6] rdrand = {:#010x}  rdseed = {:#010x}",
        res_a.rdrand, res_a.rdseed
    );

    // RDTSC/RDTSCP are V-time, not host TSC: small, strictly monotonic, constant
    // per-branch delta (VClock::guest_ticks(work) = 2·work; one branch between reads).
    let t = res_a.tsc;
    assert!(
        t.windows(2).all(|w| w[1] > w[0]),
        "RDTSC must be strictly monotonic, got {t:?}"
    );
    assert!(
        t[3] < 1_000,
        "a V-time TSC is tiny here ({t:?}); a value this large is a leaked HOST TSC"
    );
    let d0 = t[1] - t[0];
    assert!(
        d0 > 0 && t[2] - t[1] == d0 && t[3] - t[2] == d0,
        "deltas must be constant (one retired branch = {d0} ticks each), got {t:?}"
    );
    assert_eq!(d0, 2, "V-time formula: 1 branch = 1 ns = 2 ticks at 2 GHz");

    // RDTSCP ECX = the contract IA32_TSC_AUX (the guest never WRMSR'd it → 0).
    assert_eq!(res_a.aux, 0, "RDTSCP aux must be the contract IA32_TSC_AUX");

    // RDRAND/RDSEED are consecutive words of the seeded stream — never host RNG.
    let (exp_rdrand, exp_rdseed) = expected_rng(SEED);
    assert_eq!(
        res_a.rdrand, exp_rdrand,
        "RDRAND must equal the seeded stream"
    );
    assert_eq!(
        res_a.rdseed, exp_rdseed,
        "RDSEED must equal the next seeded word"
    );
}

#[test]
#[ignore = "box-only: needs the LOADED patched KVM + perf + det-cfl-v1 host; run on \
            `ssh <det-box>` with `-- --ignored`"]
fn p6_snapshot_restore_resumes_both_clocks_exactly() {
    // Reference: an un-snapshotted run.
    let (_hash_ref, ref_res) = run_once();

    // Snapshotted: step through the first two RDTSC exits, snapshot V-time +
    // entropy, restore them (perf counter reset to 0, vns_base / stream position
    // restored), then run to terminal in the SAME VM.
    let mut vmm = boot_patched_or_panic();
    assert_eq!(vmm.step().expect("step rdtsc#1"), Step::Continued);
    assert_eq!(vmm.step().expect("step rdtsc#2"), Step::Continued);

    let snap = vmm
        .save_vtime()
        .expect("save_vtime")
        .expect("V-time is wired on the patched backend");
    vmm.restore_vtime(&snap).expect("restore_vtime");

    let r = vmm.run().expect("run to terminal after restore");
    assert_eq!(r.reason, TerminalReason::DebugExit { code: 0 });
    let snap_res = parse_results(&vmm.state_blob());

    // The restored timeline is bit-identical to the un-snapshotted reference:
    // the V-time clock and the RNG stream both resumed exactly (INTEGRATION §4).
    assert_eq!(
        snap_res, ref_res,
        "snapshot/restore must resume both clocks exactly (restored == reference)"
    );
    eprintln!("[p6] snapshot/restore transparent: results = {snap_res:?}");
}
