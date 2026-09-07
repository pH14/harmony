// SPDX-License-Identifier: AGPL-3.0-or-later
//! Search package selection and execution backend dispatch.
use clap::ValueEnum;
use nes_workload::package::{SearchOptions, search_native};
use std::{error::Error, path::PathBuf, process::ExitCode};

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Package {
    Nes,
}
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Backend {
    Native,
    Consonance,
}
#[derive(clap::Args)]
pub struct Args {
    /// ROM workload input, interpreted by the selected package.
    input: PathBuf,
    #[arg(long, value_enum)]
    package: Package,
    /// Execution backend; NES defaults to native.
    #[arg(long, value_enum)]
    backend: Option<Backend>,
    #[arg(long, default_value_t = 0)]
    seed: u64,
    #[arg(long, default_value_t = 1)]
    workers: u32,
    /// Number of search executions to admit.
    #[arg(long, default_value_t = 1000)]
    executions: u64,
    /// Maximum actions in a candidate.
    #[arg(long, default_value_t = 128)]
    actions: usize,
    #[arg(long, default_value = "harmony-search")]
    out: PathBuf,
    /// Pinned native QuickNES library; defaults to HARMONY_QUICKNES_CORE.
    #[arg(long)]
    core: Option<PathBuf>,
    /// Controlled guest kernel; defaults to installed guest artifacts.
    #[arg(long)]
    kernel: Option<PathBuf>,
    /// NES base initramfs; preparation adds the ROM.
    #[arg(long)]
    base_initramfs: Option<PathBuf>,
}
pub fn run(args: Args) -> Result<ExitCode, Box<dyn Error>> {
    let backend = args.backend.unwrap_or(match args.package {
        Package::Nes => Backend::Native,
    });
    if matches!(backend, Backend::Consonance) {
        require_supported_linux(cfg!(target_os = "linux"))?;
        match crate::host::Hypervisor::detect() {
            crate::host::Hypervisor::Kvm => {}
            crate::host::Hypervisor::Unavailable(reason)
            | crate::host::Hypervisor::Unsupported(reason) => return Err(reason.into()),
            crate::host::Hypervisor::Hvf => {
                return Err("Consonance search requires Linux KVM".into());
            }
        }
    }
    let options = SearchOptions {
        seed: args.seed,
        workers: args.workers,
        executions: args.executions,
        actions: args.actions,
        output: args.out,
    };
    match (args.package, backend) {
        (Package::Nes, Backend::Native) => {
            let core = args
                .core
                .or_else(|| std::env::var_os("HARMONY_QUICKNES_CORE").map(PathBuf::from))
                .ok_or("native NES search requires --core or HARMONY_QUICKNES_CORE")?;
            search_native(&std::fs::read(args.input)?, &core, &options)?;
        }
        (Package::Nes, Backend::Consonance) => {
            run_nes_consonance(&args.input, args.kernel, args.base_initramfs, &options)?
        }
    }
    println!("artifacts   {}", options.output.display());
    Ok(ExitCode::SUCCESS)
}

fn require_supported_linux(is_linux: bool) -> Result<(), Box<dyn Error>> {
    if !is_linux {
        return Err("Consonance search requires a supported Linux KVM host".into());
    }
    Ok(())
}

fn run_nes_consonance(
    input: &std::path::Path,
    kernel: Option<PathBuf>,
    base: Option<PathBuf>,
    options: &SearchOptions,
) -> Result<(), Box<dyn Error>> {
    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    {
        let installed =
            crate::preflight::GuestArtifacts::locate(crate::host::HostReport::detect().isa);
        let kernel = kernel
            .or(installed.kernel)
            .ok_or("controlled guest kernel missing: use --kernel or HARMONY_GUEST_DIR")?;
        let base = base
            .or_else(|| select_nes_base_initramfs(&installed.initramfs))
            .ok_or(
                "NES base image missing: use --base-initramfs or install initramfs-nes.cpio.gz",
            )?;
        let rom = std::fs::read(input)?;
        let image = nes_workload::prepare::prepare(&rom, &std::fs::read(base)?)?;
        nes_workload::package::search_consonance(&rom, &std::fs::read(kernel)?, &image, options)
    }
    #[cfg(not(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )))]
    {
        let _ = (input, kernel, base, options);
        Err("NES Consonance execution requires a supported Linux KVM host".into())
    }
}

#[cfg(any(
    test,
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )
))]
fn select_nes_base_initramfs(paths: &[PathBuf]) -> Option<PathBuf> {
    paths
        .iter()
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name == "initramfs-nes.cpio.gz")
        })
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> SearchOptions {
        SearchOptions {
            seed: 1,
            workers: 1,
            executions: 1,
            actions: 1,
            output: PathBuf::from("missing-output"),
        }
    }

    fn args(package: Package, backend: Backend) -> Args {
        Args {
            input: PathBuf::from("missing-input"),
            package,
            backend: Some(backend),
            seed: 1,
            workers: 1,
            executions: 1,
            actions: 1,
            out: PathBuf::from("missing-output"),
            core: None,
            kernel: None,
            base_initramfs: None,
        }
    }

    #[test]
    fn linux_gate_accepts_and_rejects_explicit_platform_values() {
        assert!(require_supported_linux(true).is_ok());
        assert!(require_supported_linux(false).is_err());
    }

    #[test]
    fn missing_native_nes_input_is_rejected() {
        let mut args = args(Package::Nes, Backend::Native);
        args.core = Some(PathBuf::from("missing-quicknes-core"));
        let error = run(args).expect_err("a missing native input must fail");
        assert!(!error.to_string().is_empty());
    }

    #[test]
    fn nes_consonance_requires_explicit_artifacts_before_execution() {
        let error = run_nes_consonance(
            std::path::Path::new("missing-rom"),
            Some(PathBuf::from("missing-kernel")),
            Some(PathBuf::from("missing-initramfs")),
            &options(),
        )
        .expect_err("missing NES guest artifacts must fail");
        assert!(!error.to_string().is_empty());
    }

    #[test]
    fn nes_base_image_selection_requires_the_exact_filename() {
        let wrong = PathBuf::from("initramfs-other.cpio.gz");
        let expected = PathBuf::from("initramfs-nes.cpio.gz");
        assert_eq!(
            select_nes_base_initramfs(&[wrong.clone(), expected.clone()]),
            Some(expected)
        );
        assert_eq!(select_nes_base_initramfs(&[wrong]), None);
    }
}
