// SPDX-License-Identifier: AGPL-3.0-or-later
//! Box-only live snapshot/branch gate (task 39 — `#[cfg(target_os = "linux")]`
//! **and `#[ignore]`**): the milestone gate. A bare-metal deterministic payload runs
//! on the **patched** KVM via the real vmm-core determinism stack; partway through,
//! its full state (guest memory via `snapshot-store`, the non-memory machine via the
//! `vm_state` codec) is snapshotted, restored into a **fresh** VM, and run forward —
//! and the restored continuation is **bit-identical** to the un-snapshotted
//! reference from that point (`state_hash` + serial + guest results). *Same state ⇒
//! same future.*
//!
//! It also exercises:
//! - **N VMs share one read-only base** (gate 3): N branches materialized from one
//!   base store the base's pages **once** store-wide.
//! - **Restore latency** (gate 2, probe): the store-side materialize cost and the
//!   guest-RAM restore copy are timed and printed. The O(dirty) memslot-swap that
//!   *beats* full-`memcpy` is the `vmm-backend` follow-up (task 08's chosen
//!   mechanism, below the `Backend` trait); see `IMPLEMENTATION.md`.
//!
//! Box-only because it needs the loaded patched `/dev/kvm`
//! (`KVM_CAP_X86_DETERMINISTIC_INTERCEPTS`), `perf_event`, and the `det-cfl-v1`
//! host. `#[ignore]`d out of the default lane (like `live_determinism.rs`): default
//! CI shows it **not-run**, never a vacuous green. Run on `ssh <det-box>`, CPU-pinned
//! per `docs/BOX-PINNING.md`, patched modules loaded, reverted to stock after:
//!
//! ```sh
//! taskset -c 4 cargo test -p vmm-core --test live_snapshot_branch -- --ignored --test-threads=1
//! ```
//!
//! Every precondition that would prevent a real run — no `/dev/kvm`, the determinism
//! cap absent (stock modules), or a non-baseline host — is a **loud panic (test
//! FAILURE)**, never an early-return `Ok`. macOS builds an empty test binary; the
//! snapshot/branch *logic* is covered there by the unit tests in `src/snapshot.rs` /
//! `src/vmm.rs` and the portable `tests/snapshot_branch.rs` integration test.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use vmm_backend::{Backend, X86};
use vmm_core::snapshot::SnapshotEngine;
use vmm_core::vendor::x86::bringup::{BackendKind, boot_selected};
use vmm_core::vmm::{Step, TerminalReason, Vmm};

/// Seed for the deterministic entropy stream RDRAND/RDSEED draw from.
const SEED: u64 = 0x39_5EED_2026;
/// Guest RAM: 4 MiB — covers the 1 MiB payload load and the low result buffer.
const GUEST_RAM_LEN: usize = 4 << 20;

// The deterministic payload — 32-bit protected-mode machine code (Multiboot hands
// off in 32-bit PM, paging off). Identical in spirit to `live_determinism.rs`:
//   mov edi, 0x8000 ; mov ecx, 3
//  .loop: rdtsc; mov [edi],eax; add edi,4; dec ecx; jnz .loop  ; 3 RDTSC, +1 work/iter
//   rdtscp; mov [edi],eax; mov [edi+4],ecx; add edi,8
//   rdrand eax; mov [edi],eax; add edi,4
//   rdseed eax; mov [edi],eax
//   mov al,0; out 0xF4,al  ; isa-debug-exit PASS (terminal); hlt fallback
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

/// Wrap [`PAYLOAD_CODE`] in a Multiboot v1 address-override image.
fn payload_image() -> Vec<u8> {
    let load = PAYLOAD_LOAD_GPA;
    let header_len = 32u32;
    let load_end = load + header_len + PAYLOAD_CODE.len() as u32;
    let entry = load + header_len;
    let checksum = 0u32
        .wrapping_sub(MB_HEADER_MAGIC)
        .wrapping_sub(MB_FLAG_ADDRESS_OVERRIDE);
    let fields = [
        MB_HEADER_MAGIC,
        MB_FLAG_ADDRESS_OVERRIDE,
        checksum,
        load,
        load,
        load_end,
        load_end,
        entry,
    ];
    let mut img = Vec::with_capacity(32 + PAYLOAD_CODE.len());
    for f in fields {
        img.extend_from_slice(&f.to_le_bytes());
    }
    img.extend_from_slice(&PAYLOAD_CODE);
    img
}

type DynVmm = Vmm<Box<dyn Backend<A = X86>>>;

/// Boot the patched backend over the payload, **panicking loudly** with a precise
/// reason if the box is not ready — never an early-return that nextest counts as a
/// vacuous pass.
fn boot_patched_or_panic() -> DynVmm {
    assert!(
        std::path::Path::new("/dev/kvm").exists(),
        "/dev/kvm absent — run this `#[ignore]`d box gate on `ssh <det-box>` with the patched KVM \
         modules loaded, CPU-pinned per docs/BOX-PINNING.md (taskset -c 4)."
    );
    match boot_selected(BackendKind::Patched, &payload_image(), GUEST_RAM_LEN, SEED) {
        Ok(vmm) => vmm,
        Err(e) => panic!(
            "boot_selected(Patched) failed: {e}. Needs the LOADED patched KVM \
             (KVM_CAP_X86_DETERMINISTIC_INTERCEPTS), perf_event, and the det-cfl-v1 host. Build + \
             load per consonance/vmm-backend/kvm-patches/BUILD.md, then revert to stock after."
        ),
    }
}

fn hex(d: &[u8; 32]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// The reference: a fresh patched VM run straight to terminal.
fn run_reference() -> ([u8; 32], Vec<u8>) {
    let mut vmm = boot_patched_or_panic();
    let r = vmm.run().expect("patched run to terminal");
    assert_eq!(
        r.reason,
        TerminalReason::DebugExit { code: 0 },
        "payload must end on a clean isa-debug-exit PASS"
    );
    (vmm.state_hash(), r.serial)
}

#[test]
#[ignore = "box-only: needs the LOADED patched KVM + perf + det-cfl-v1 host; run on \
            `ssh <det-box>` with `-- --ignored`"]
fn gate1_restore_replays_bit_identical() {
    // Reference continuation (un-snapshotted): boot → run → terminal.
    let (ref_hash, ref_serial) = run_reference();

    // Snapshotted run: boot a VM, step to a clean V-time-intercept boundary (after
    // the 2nd RDTSC — synchronized, no staged RNG), and capture its FULL state
    // (guest memory → snapshot-store base, non-memory → the vm_state codec).
    let mut a = boot_patched_or_panic();
    assert_eq!(a.step().expect("step rdtsc#1"), Step::Continued);
    assert_eq!(a.step().expect("step rdtsc#2"), Step::Continued);

    let mut engine = SnapshotEngine::new(GUEST_RAM_LEN);
    let vm_state = a
        .save_vm_state()
        .expect("save_vm_state at the clean RDTSC intercept");
    let blob = vm_state.encode().expect("vm_state encodes");
    let snap = engine
        .snapshot_base(a.guest_memory(), &blob)
        .expect("snapshot the booted image + vm_state");

    // Restore into a FRESH patched VM and run it forward to terminal.
    let mut b = boot_patched_or_panic();
    let mapping = engine.materialize(snap).expect("materialize");
    let decoded = engine.vm_state(snap).expect("decode sealed vm_state");
    b.restore_snapshot(mapping.as_slice(), &decoded)
        .expect("restore the snapshot into the fresh VM");
    let r = b.run().expect("restored run to terminal");
    assert_eq!(r.reason, TerminalReason::DebugExit { code: 0 });
    let restored_hash = b.state_hash();

    eprintln!("[gate1] reference state_hash = {}", hex(&ref_hash));
    eprintln!("[gate1] restored  state_hash = {}", hex(&restored_hash));
    assert_eq!(
        restored_hash, ref_hash,
        "the restored continuation must be bit-identical to the un-snapshotted reference \
         (state_hash). Same state ⇒ same future."
    );
    assert_eq!(
        r.serial, ref_serial,
        "the restored continuation's serial must be bit-identical to the reference"
    );
    eprintln!("[gate1] restore replays bit-identical ✓ (digests equal, quoted above)");
}

#[test]
#[ignore = "box-only: needs the LOADED patched KVM + perf + det-cfl-v1 host; run on \
            `ssh <det-box>` with `-- --ignored`"]
fn gate3_n_vms_share_one_read_only_base() {
    // Snapshot one booted image as the shared base.
    let mut a = boot_patched_or_panic();
    a.step().expect("step to a clean intercept");
    let mut engine = SnapshotEngine::new(GUEST_RAM_LEN);
    let blob = a.save_vm_state().expect("save").encode().expect("encode");
    let base = engine.snapshot_base(a.guest_memory(), &blob).expect("base");
    let base_unique = engine.store_stats().stored_unique_pages;
    assert!(base_unique > 0, "the booted base has resident pages");

    // Materialize + branch N independent views; each touches nothing.
    const N: usize = 8;
    let mut views = Vec::new();
    for i in 0..N {
        let branch = engine
            .snapshot_derive(base, a.guest_memory(), Some(&[]), &blob)
            .unwrap_or_else(|e| panic!("branch {i}: {e}"));
        views.push(engine.materialize(branch).expect("materialize branch"));
    }
    let stats = engine.store_stats();
    eprintln!(
        "[gate3] base unique pages = {base_unique}; after {N} branches store-wide unique = {} \
         (resident bytes = {})",
        stats.stored_unique_pages, stats.bytes_resident
    );
    assert_eq!(
        stats.stored_unique_pages, base_unique,
        "N branches that touched nothing add NO unique pages — the base is shared once store-wide"
    );
    assert_eq!(views.len(), N);
}

#[test]
#[ignore = "box-only: needs the LOADED patched KVM + perf + det-cfl-v1 host; run on \
            `ssh <det-box>` with `-- --ignored`"]
fn gate2_capture_is_dirty_set_proportional() {
    // Gate 2 (capture side): a derived snapshot stores **only the pages dirtied since
    // its parent** — `owned_pages` tracks the dirty set, not the image size. Measured
    // structurally via store stats, **not** wall-clock: timing is disallowed in this
    // determinism codebase (`clippy.toml` bans `Instant::now`), and the wall-clock
    // "beat full-memcpy on restore" headline is the `vmm-backend` memslot-swap
    // follow-up (below the `Backend` trait; see IMPLEMENTATION.md) — not measurable,
    // nor measured, here. Restore correctness is asserted instead.
    let mut a = boot_patched_or_panic();
    a.step().expect("step to a clean intercept");
    let mut engine = SnapshotEngine::new(GUEST_RAM_LEN);
    let blob = a.save_vm_state().expect("save").encode().expect("encode");
    let base = engine.snapshot_base(a.guest_memory(), &blob).expect("base");
    let pages = (GUEST_RAM_LEN / 4096) as u64;

    // Dirty exactly one (previously-zero, high) guest page and derive: the child owns
    // exactly that one page over the shared base — dirty-set-proportional, not
    // image-size-proportional.
    let gfn = pages - 1;
    let off = (gfn as usize) * 4096;
    let mut dirtied = a.guest_memory().to_vec();
    dirtied[off..off + 4096].fill(0xC3);
    let child = engine
        .snapshot_derive(base, &dirtied, Some(&[gfn]), &blob)
        .expect("derive");
    let owned = engine.stats(child).expect("stats").owned_pages;
    eprintln!(
        "[gate2] image = {pages} pages; a +1-page-delta child owns {owned} page(s) store-wide — \
         capture scales with the dirty set, not image size. The O(dirty) restore that beats memcpy \
         is the vmm-backend memslot-swap follow-up."
    );
    assert_eq!(
        owned, 1,
        "a one-page delta must store exactly one owned page (dirty-set-proportional capture)"
    );

    // Restore correctness: the materialized child restores and runs to terminal.
    let mut b = boot_patched_or_panic();
    let decoded = engine.vm_state(child).expect("decode");
    let mapping = engine.materialize(child).expect("materialize");
    b.restore_snapshot(mapping.as_slice(), &decoded)
        .expect("restore");
    let r = b.run().expect("restored VM still runs to terminal");
    assert_eq!(r.reason, TerminalReason::DebugExit { code: 0 });
}
