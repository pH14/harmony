// SPDX-License-Identifier: AGPL-3.0-or-later
//! Live Apple-HVF composition probe for the arm64 M2 control memslot.

#[cfg(all(target_os = "macos", target_arch = "aarch64", not(miri)))]
fn main() -> std::process::ExitCode {
    use vmm_core::vendor::arm64::bringup;

    const RAM: usize = 128 * 1024 * 1024;

    let mut args = std::env::args_os().skip(1);
    let (Some(image_path), Some(initramfs_path), None) = (args.next(), args.next(), args.next())
    else {
        eprintln!("usage: hvf_control_probe <Image> <initramfs.cpio.gz>");
        return std::process::ExitCode::from(2);
    };
    let image = match std::fs::read(&image_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("cannot read {image_path:?}: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let initramfs = match std::fs::read(&initramfs_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("cannot read {initramfs_path:?}: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let vmm = match bringup::boot_hvf_control(
        &image,
        &initramfs,
        "console=ttyAMA0 earlycon=pl011,0x09000000 rdinit=/init nohlt",
        RAM,
    ) {
        Ok(vmm) => vmm,
        Err(error) => {
            eprintln!("HVF control composition failed: {error:?}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let components = vmm.state_components();
    if !components.iter().any(|(name, _)| *name == "doorbell") {
        eprintln!("HVF control composition omitted the doorbell state component");
        return std::process::ExitCode::FAILURE;
    }
    println!(
        "HVF_CONTROL_MAP_OK bytes=16384 component=doorbell state_hash={}",
        vmm.state_hash()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    std::process::ExitCode::SUCCESS
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64", not(miri))))]
fn main() -> std::process::ExitCode {
    eprintln!("hvf_control_probe requires an Apple Silicon macOS host outside Miri");
    std::process::ExitCode::from(2)
}
