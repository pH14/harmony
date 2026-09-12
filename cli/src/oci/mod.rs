// SPDX-License-Identifier: AGPL-3.0-or-later

use oci_support::{bundle, image};
mod runner;

use crate::host::{HostReport, MatrixCell};
use crate::preflight::GuestArtifacts;
use clap::Args;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

#[derive(Args)]
pub struct RunArgs {
    pub image: String,

    #[arg(long, default_value_t = 0)]
    pub seed: u64,

    #[arg(long)]
    pub out: Option<PathBuf>,

    #[arg(long, default_value_t = 512)]
    pub ram_mib: usize,

    #[arg(long, default_value_t = 900)]
    pub timeout: u64,

    #[arg(long)]
    pub console: bool,

    #[arg(long)]
    pub allow_untested: bool,

    #[arg(last = true)]
    pub cmd: Vec<String>,
}

pub const BASE_INITRAMFS: &[&str] = &["initramfs-oci.cpio.gz"];

pub use runner::{HOST_SUPPORTED, SUPPORTED_HOSTS};

pub fn select_base_initramfs(installed: &[PathBuf]) -> Option<&PathBuf> {
    BASE_INITRAMFS.iter().find_map(|name| {
        installed
            .iter()
            .find(|p| p.file_name().is_some_and(|f| f == *name))
    })
}

pub fn missing_base_initramfs() -> String {
    format!(
        "no container-capable guest initramfs found (looked for {}); build the \
         platform runtime artifact",
        BASE_INITRAMFS.join(", ")
    )
}

#[derive(serde::Serialize)]
struct RunRecord {
    image: String,
    seed: u64,
    isa: String,
    os: &'static str,
    cmdline: &'static str,
    kernel_sha256: String,
    base_initramfs: String,
    base_initramfs_sha256: String,
    rootfs_segment_sha256: String,
    control_segment_sha256: String,
    execution_identity: String,
    execution_json_sha256: String,
    guest_ram_mib: usize,
    steps: u64,
    terminal: String,
    container_rc: Option<i32>,
    serial_sha256: String,
}

pub fn run(args: RunArgs) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let host = HostReport::detect();
    if !host.hypervisor.available() {
        return Err("hypervisor unavailable: run `harmony preflight`".into());
    }
    if !HOST_SUPPORTED {
        return Err(format!(
            "no run loop for this host in this build; wired hosts are {SUPPORTED_HOSTS}"
        )
        .into());
    }
    match host.cell {
        MatrixCell::Proven => {}
        MatrixCell::Expected if args.allow_untested => {}
        MatrixCell::Expected => {
            return Err("this host is an untested support-matrix cell \
                        (docs/DETERMINISM.md §4); pass --allow-untested to proceed"
                .into());
        }
        MatrixCell::Unsupported => return Err("unsupported host: run `harmony preflight`".into()),
    }

    let guest = GuestArtifacts::locate(host.isa);
    let Some(kernel_path) = guest.kernel.as_ref() else {
        return Err("guest artifacts not found: run `harmony preflight`".into());
    };
    let base_path = select_base_initramfs(&guest.initramfs).ok_or_else(missing_base_initramfs)?;

    let kernel = std::fs::read(kernel_path)?;
    let base = std::fs::read(base_path)?;

    let prepared = prepare_for(&args.image, &bundle::LaunchRequest::new(args.cmd.clone()))?;
    let initramfs = prepared.initramfs(&base);

    let spec = runner::RunSpec {
        kernel: &kernel,
        initramfs: &initramfs,
        cmdline: runner::cmdline(),
        guest_ram_len: args.ram_mib << 20,
        seed: args.seed,
        wall_budget: Duration::from_secs(args.timeout),
        stream: if args.console {
            runner::StreamMode::Full
        } else {
            runner::StreamMode::Container
        },
    };
    eprintln!("booting ({} MiB RAM, seed {}) ...", args.ram_mib, args.seed);
    let outcome = match runner::execute(&spec) {
        Ok(outcome) => outcome,
        Err(error) => {
            #[cfg(any(
                all(target_os = "linux", target_arch = "x86_64"),
                all(target_os = "linux", target_arch = "aarch64"),
                all(target_os = "macos", target_arch = "aarch64")
            ))]
            if let runner::RunError::WallBudget { serial, .. } = &error {
                let out_dir = args.out.clone().unwrap_or_else(|| {
                    std::env::temp_dir().join(format!("harmony-run-{}", std::process::id()))
                });
                std::fs::create_dir_all(&out_dir)?;
                std::fs::write(out_dir.join("serial.log"), serial)?;
                eprintln!(
                    "partial serial log: {}",
                    out_dir.join("serial.log").display()
                );
            }
            return Err(error.into());
        }
    };

    let container_rc = parse_container_rc(&outcome.serial);
    let execution_json = prepared.execution.to_vec()?;
    let record = RunRecord {
        image: args.image.clone(),
        seed: args.seed,
        isa: host.isa.to_string(),
        os: host.os,
        cmdline: runner::cmdline(),
        kernel_sha256: hex(&Sha256::digest(&kernel)),
        base_initramfs: base_path.display().to_string(),
        base_initramfs_sha256: hex(&Sha256::digest(&base)),
        rootfs_segment_sha256: hex(&Sha256::digest(&prepared.rootfs_segment)),
        control_segment_sha256: hex(&Sha256::digest(&prepared.control_segment)),
        execution_identity: prepared.identity_hex(),
        execution_json_sha256: hex(&Sha256::digest(&execution_json)),
        guest_ram_mib: args.ram_mib,
        steps: outcome.steps,
        terminal: outcome.reason.clone(),
        container_rc,
        serial_sha256: hex(&Sha256::digest(&outcome.serial)),
    };

    let out_dir = match &args.out {
        Some(dir) => dir.clone(),
        None => std::env::temp_dir().join(format!("harmony-run-{}", std::process::id())),
    };
    std::fs::create_dir_all(&out_dir)?;
    std::fs::write(out_dir.join("serial.log"), &outcome.serial)?;
    std::fs::write(
        out_dir.join("run.json"),
        serde_json::to_vec_pretty(&record)?,
    )?;

    println!("digest      {}", record.serial_sha256);
    match container_rc {
        Some(rc) => println!("container   exited rc={rc}"),
        None => println!("container   no exit marker (terminal: {})", record.terminal),
    }
    println!("artifact    {}", out_dir.display());
    Ok(match container_rc {
        Some(0) => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    })
}

fn prepare_for(
    image: &str,
    request: &bundle::LaunchRequest,
) -> Result<bundle::PreparedExecution, Box<dyn std::error::Error>> {
    if !std::path::Path::new(image).exists() {
        image::ensure_local(image);
    }
    eprintln!("staging {image} ...");
    let staging = tempfile::tempdir()?;
    let staged = image::stage(image, staging.path())?;
    Ok(bundle::prepare(&staged, request)?)
}

fn parse_container_rc(serial: &[u8]) -> Option<i32> {
    let text = String::from_utf8_lossy(serial);
    text.lines()
        .rev()
        .find_map(|l| l.trim().strip_prefix("HARMONY_OCI_APP_EXIT rc="))
        .and_then(|rc| rc.trim().parse().ok())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_initramfs_requires_the_platform_runtime_name() {
        let dir = std::path::Path::new("/g");
        let installed: Vec<PathBuf> = ["initramfs-oci.cpio.gz", "initramfs-alternate.cpio.gz"]
            .iter()
            .map(|n| dir.join(n))
            .collect();
        assert_eq!(
            select_base_initramfs(&installed),
            Some(&dir.join("initramfs-oci.cpio.gz"))
        );
        let workload_runtime = vec![dir.join("initramfs-alternate.cpio.gz")];
        assert_eq!(select_base_initramfs(&workload_runtime), None);
    }

    #[test]
    fn base_initramfs_rejects_unaccepted_names() {
        let installed = vec![
            PathBuf::from("/g/initramfs.cpio.gz"),
            PathBuf::from("/g/initramfs-minimal.cpio.gz"),
        ];
        assert_eq!(select_base_initramfs(&installed), None);
        assert_eq!(select_base_initramfs(&[]), None);
        assert!(missing_base_initramfs().contains("initramfs-oci.cpio.gz"));
    }

    #[test]
    fn container_rc_parses_last_marker() {
        let serial = b"noise\nHARMONY_OCI_APP_EXIT rc=3\ntail\nHARMONY_OCI_APP_EXIT rc=0\n";
        assert_eq!(super::parse_container_rc(serial), Some(0));
        assert_eq!(
            super::parse_container_rc(b"HARMONY_OCI_APP_EXIT rc=127\n"),
            Some(127)
        );
        assert_eq!(super::parse_container_rc(b"no marker"), None);
    }

    #[test]
    fn platform_init_has_one_fixed_launch_and_separate_failure_markers() {
        let init = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../consonance/harmony-linux/runtime/init.sh"
        ));
        assert_eq!(
            init.matches("/usr/bin/runc run --no-pivot --bundle")
                .count(),
            1
        );
        assert!(init.contains("[ -x \"$RUNC\" ] || startup_failure 127"));
        assert!(init.contains("HARMONY_OCI_STARTUP_EXIT"));
        assert!(init.contains("HARMONY_OCI_APP_EXIT rc=$runc_status"));
        assert!(!init.contains("chroot"));
        assert!(!init.contains("runc --version"));
    }

    #[test]
    fn hex_is_lowercase_zero_padded() {
        assert_eq!(super::hex(&[0x00, 0x0f, 0xab]), "000fab");
    }
}
