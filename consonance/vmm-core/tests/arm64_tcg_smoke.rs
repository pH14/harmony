// SPDX-License-Identifier: AGPL-3.0-or-later
//! M3 TCG smoke (`tasks/112`) — **liveness/shape only, no counts**.
//!
//! Boots the `Image`+DTB **boot artifacts this vendor produces** on QEMU's own
//! emulated aarch64 machine (`-M virt,gic-version=3`) to a console marker,
//! proving the guest image is well-formed and boots. It is `#[ignore]`d by
//! default and runs only via `cargo test -- --ignored` (like the public-api
//! harness), because it needs `clang` + `llvm-objcopy` + `qemu-system-aarch64`;
//! when any is absent it **skips loudly** rather than failing, so a plain
//! `cargo nextest` on a stable-only box stays green.
//!
//! What this proves, and what it does **not**:
//! - it proves the arm64 `Image` header ([`image_loader`]) is recognized and
//!   booted by a real aarch64 boot loader (QEMU), that the entry lands at the
//!   image's first instruction, and that the [`dtb`] this vendor emits is a
//!   structurally valid FDT QEMU accepts;
//! - it does **not** exercise `Arm64KvmBackend` (QEMU is its *own* VMM, not our
//!   backend — that path talks to `/dev/kvm` and is M4's, arrival-day) and it
//!   says **nothing** about `BR_RETIRED` counts, PMIs, or skid — those are
//!   silicon's (`docs/ARM-ALTRA.md` AA-1/AA-3).
//!
//! Evidence integrity (`docs/ARM-ALTRA.md` §1): every constituent RC is
//! propagated — a nonzero assemble/objcopy/QEMU status, a missing marker, or a
//! non-clean poweroff fails the test. A done-marker is never a success
//! condition; the success condition is `marker present AND QEMU exited 0` (the
//! payload's PSCI `SYSTEM_OFF`).

use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use vmm_core::vendor::arm64::board;
use vmm_core::vendor::arm64::{dtb, image_loader};

/// The console marker the payload prints; its presence in QEMU's serial output
/// is half of the success condition (the other half is a clean PSCI poweroff).
const MARKER: &str = "HARMONY-ARM64-BOOT";

/// The **body** of the boot payload — everything *after* the 64-byte `Image`
/// header. It writes [`MARKER`] to the PL011 console at the board's UART address
/// and powers off via PSCI `SYSTEM_OFF`. The header (magic, sizes, and the
/// `code0` branch over itself) is prepended by the production
/// [`image_loader::wrap_image`], so this smoke boots the **exact** header the
/// vendor emits — one hand-rolled header fewer to drift from [`image_loader`]'s
/// field layout. The board PL011 address is spliced in as a literal so the
/// payload and [`board::PL011`] can never disagree.
fn payload_body_source() -> String {
    let uart_hi = (board::PL011.0 >> 16) as u16; // 0x0900 for 0x0900_0000
    assert_eq!(
        u64::from(uart_hi) << 16,
        board::PL011.0,
        "PL011 base must be a `movz #imm, lsl #16` literal for the smoke payload"
    );
    format!(
        r#"
    .section .text
    .global _start
_start:
    movz    x1, #{uart_hi:#06x}, lsl #16
    adr     x2, msg
1:  ldrb    w0, [x2], #1
    cbz     w0, 2f
    str     w0, [x1]
    b       1b
2:  movz    x0, #0x0008
    movk    x0, #0x8400, lsl #16
    hvc     #0
3:  wfi
    b       3b
msg:
    .asciz  "{MARKER}\n"
    .balign 8
"#
    )
}

/// Locate `llvm-objcopy`: on PATH, else the rustlib `llvm-tools` component next
/// to the active toolchain. Returns `None` if neither is found.
fn find_objcopy() -> Option<PathBuf> {
    if Command::new("llvm-objcopy")
        .arg("--version")
        .output()
        .is_ok()
    {
        return Some(PathBuf::from("llvm-objcopy"));
    }
    let sysroot = Command::new("rustc")
        .arg("--print")
        .arg("sysroot")
        .output()
        .ok()?;
    let sysroot = String::from_utf8(sysroot.stdout).ok()?;
    let host = Command::new("rustc").arg("-vV").output().ok()?;
    let host = String::from_utf8(host.stdout).ok()?;
    let host = host.lines().find_map(|l| l.strip_prefix("host: "))?;
    let p = PathBuf::from(sysroot.trim())
        .join("lib/rustlib")
        .join(host)
        .join("bin/llvm-objcopy");
    p.exists().then_some(p)
}

fn tool_present(name: &str) -> bool {
    Command::new(name).arg("--version").output().is_ok()
}

/// The `timeout`/`gtimeout` binary, if present (coreutils). The payload
/// self-exits via PSCI in well under a second, so this is a belt-and-braces
/// hang guard, not the primary exit.
fn timeout_cmd() -> Option<&'static str> {
    ["timeout", "gtimeout"]
        .into_iter()
        .find(|t| Command::new(t).arg("--version").output().is_ok())
}

#[test]
#[ignore = "needs clang + llvm-objcopy + qemu-system-aarch64; runs via `cargo test -- --ignored`"]
fn image_and_dtb_boot_on_qemu_tcg() {
    // --- skip loudly if the local oracle's toolchain is absent -------------
    if !tool_present("clang") {
        eprintln!("SKIP: arm64 TCG smoke — clang not found");
        return;
    }
    let Some(objcopy) = find_objcopy() else {
        eprintln!(
            "SKIP: arm64 TCG smoke — llvm-objcopy not found (rustup component add llvm-tools)"
        );
        return;
    };
    if !tool_present("qemu-system-aarch64") {
        eprintln!("SKIP: arm64 TCG smoke — qemu-system-aarch64 not found (brew install qemu)");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let s_path = dir.path().join("payload.s");
    let o_path = dir.path().join("payload.o");
    let body_path = dir.path().join("payload-body.bin");
    let image_path = dir.path().join("harmony.Image");
    let dtb_path = dir.path().join("harmony.dtb");

    // --- assemble the payload BODY (no header) -----------------------------
    std::fs::write(&s_path, payload_body_source()).expect("write payload.s");
    let asm = Command::new("clang")
        .args(["--target=aarch64-linux-gnu", "-c"])
        .arg(&s_path)
        .arg("-o")
        .arg(&o_path)
        .output()
        .expect("run clang");
    assert!(
        asm.status.success(),
        "clang failed to assemble the payload:\n{}",
        String::from_utf8_lossy(&asm.stderr)
    );
    let oc = Command::new(&objcopy)
        .args(["-O", "binary", "--only-section=.text"])
        .arg(&o_path)
        .arg(&body_path)
        .output()
        .expect("run llvm-objcopy");
    assert!(
        oc.status.success(),
        "llvm-objcopy failed:\n{}",
        String::from_utf8_lossy(&oc.stderr)
    );
    let body = std::fs::read(&body_path).expect("read payload body");

    // --- prepend the PRODUCTION Image header with the vendor's own helper ---
    // wrap_image builds the 64-byte header (magic, sizes) and the `code0` branch
    // over the header onto the body — so QEMU boots the *exact* artifact the M4
    // KVM path will produce, code0 branch included (review r7: the branch is the
    // whole point — without it the entry executes the header word, not the body).
    let image = image_loader::wrap_image(&body, 0, 0xA /* 4K page bits */);
    std::fs::write(&image_path, &image).expect("write wrapped Image");

    // --- cross-check: OUR loader accepts the exact artifact QEMU will boot --
    let hdr = image_loader::parse_header(&image)
        .expect("the vendor's own Image loader must accept the boot artifact");
    assert_eq!(hdr.text_offset, 0);
    assert_eq!(hdr.image_size, image.len() as u64);

    // --- build the DTB this vendor emits, and cross-check it round-trips ----
    // (QEMU virt places RAM at RAM_BASE with 512 MiB; the reserved pvclock page
    // GPA is nominal for the smoke — the payload does not read the DTB, it
    // proves the DTB is a valid FDT QEMU accepts.)
    let dtb_bytes = dtb::build(
        512 * 1024 * 1024,
        board::RAM_BASE + 0x0100_0000,
        "console=ttyAMA0",
    );
    dtb::parse(&dtb_bytes).expect("the vendor's DTB must round-trip through its own parser");
    let mut f = std::fs::File::create(&dtb_path).expect("create dtb");
    f.write_all(&dtb_bytes).expect("write dtb");
    drop(f);

    // --- boot on QEMU's own machine (NOT our backend) ----------------------
    let mut cmd;
    if let Some(t) = timeout_cmd() {
        cmd = Command::new(t);
        cmd.arg("60").arg("qemu-system-aarch64");
    } else {
        cmd = Command::new("qemu-system-aarch64");
    }
    cmd.args([
        "-machine",
        "virt,gic-version=3",
        "-cpu",
        "cortex-a57",
        "-m",
        "512",
        "-nographic",
        "-no-reboot",
        "-kernel",
    ])
    .arg(&image_path)
    .arg("-dtb")
    .arg(&dtb_path);
    let run = cmd.output().expect("run qemu-system-aarch64");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);

    // --- the success condition: marker present AND clean PSCI poweroff -----
    assert!(
        stdout.contains(MARKER),
        "the guest never reached the console marker {MARKER:?} — the Image+DTB did not boot.\n\
         QEMU stdout:\n{stdout}\nQEMU stderr:\n{stderr}"
    );
    assert!(
        run.status.success(),
        "QEMU did not exit cleanly (a clean PSCI SYSTEM_OFF exits 0); status = {:?}.\n\
         QEMU stderr:\n{stderr}",
        run.status.code()
    );
    eprintln!("arm64 TCG smoke: booted Image+DTB to marker {MARKER:?}, clean PSCI poweroff");
}
