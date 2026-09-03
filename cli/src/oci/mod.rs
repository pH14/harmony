// SPDX-License-Identifier: AGPL-3.0-or-later
//! `harmony oci run`: boot an OCI image inside the deterministic hypervisor
//! and run it to completion.
//!
//! Pipeline: acquire the image ([`image`]), build the injected initramfs
//! segment ([`bundle`]), append it to the stock guest initramfs, boot and
//! drive the guest ([`runner`]), then write the run artifact and print the
//! digest. Identical inputs produce an identical digest; that claim is
//! ISA-scoped (docs/DETERMINISM.md §4).

mod bundle;
mod cache;
mod cpio;
mod image;
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
    /// OCI image: a registry reference (`postgres:16`, exported via docker or
    /// podman) or a path to a `docker save` tarball / OCI layout directory.
    pub image: String,

    /// Schedule seed. The same seed, image, and guest artifacts produce a
    /// byte-identical run and digest.
    #[arg(long, default_value_t = 0)]
    pub seed: u64,

    /// Directory to write the run artifact (serial.log + run.json) into.
    #[arg(long)]
    pub out: Option<PathBuf>,

    /// Guest RAM in MiB.
    #[arg(long, default_value_t = 512)]
    pub ram_mib: usize,

    /// Wall-clock budget in seconds before the run is abandoned.
    #[arg(long, default_value_t = 900)]
    pub timeout: u64,

    /// Stream the full serial console (kernel log included) instead of just
    /// the container's output.
    #[arg(long)]
    pub console: bool,

    /// Proceed on a support-matrix cell that is expected but has no
    /// committed evidence (docs/DETERMINISM.md §4).
    #[arg(long)]
    pub allow_untested: bool,

    /// Override the image's entrypoint/cmd (everything after `--`).
    #[arg(last = true)]
    pub cmd: Vec<String>,
}

/// Base initramfs variants that can host an injected bundle, best first:
/// the dedicated oci runner, then the container-class images that carry
/// busybox (+ runc).
pub const BASE_INITRAMFS: &[&str] = &[
    "initramfs-oci.cpio.gz",
    "initramfs-docker.cpio.gz",
    "initramfs-postgres.cpio.gz",
];

/// Whether a drive loop for this host exists in this build, and the hosts
/// that have one. `harmony preflight` reports readiness against the same
/// predicate `execute` is compiled under.
pub use runner::{HOST_SUPPORTED, SUPPORTED_HOSTS};

/// The best base initramfs among the installed ones, in `BASE_INITRAMFS`
/// order. `None` means no installed initramfs can host an injected bundle.
pub fn select_base_initramfs(installed: &[PathBuf]) -> Option<&PathBuf> {
    BASE_INITRAMFS.iter().find_map(|name| {
        installed
            .iter()
            .find(|p| p.file_name().is_some_and(|f| f == *name))
    })
}

/// The refusal naming what a host without an accepted base initramfs is
/// missing.
pub fn missing_base_initramfs() -> String {
    format!(
        "no container-capable guest initramfs found (looked for {}); build one with \
         `make -C consonance/harmony-linux <arm64-oci-image|docker-image>`",
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
    // Refuse before staging: acquiring the image costs a registry pull.
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

    let (rootfs_segment, config) = rootfs_segment_for(&args.image)?;
    let control_segment = bundle::build_control_segment(&config, &args.cmd)?;
    let mut initramfs = base.clone();
    initramfs.extend_from_slice(&rootfs_segment);
    initramfs.extend_from_slice(&control_segment);

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
    let outcome = runner::execute(&spec)?;

    let container_rc = parse_container_rc(&outcome.serial);
    let record = RunRecord {
        image: args.image.clone(),
        seed: args.seed,
        isa: host.isa.to_string(),
        os: host.os,
        cmdline: runner::cmdline(),
        kernel_sha256: hex(&Sha256::digest(&kernel)),
        base_initramfs: base_path.display().to_string(),
        base_initramfs_sha256: hex(&Sha256::digest(&base)),
        rootfs_segment_sha256: hex(&Sha256::digest(&rootfs_segment)),
        control_segment_sha256: hex(&Sha256::digest(&control_segment)),
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

/// The cached rootfs segment for `image`, or a fresh stage. Path inputs
/// (docker-save tarballs, OCI layouts) have no content-addressed ID and are
/// always staged.
fn rootfs_segment_for(
    image: &str,
) -> Result<(Vec<u8>, image::RuntimeConfig), Box<dyn std::error::Error>> {
    let key = if std::path::Path::new(image).exists() {
        None
    } else {
        image::ensure_local(image);
        cache::key(image)
    };
    let cache_dir = cache::dir();
    if let (Some(key), Some(dir)) = (&key, &cache_dir)
        && let Some((segment, config)) = cache::load(dir, key)
    {
        eprintln!("staging {image} (cached segment) ...");
        return Ok((segment, config));
    }
    eprintln!("staging {image} ...");
    let staging = tempfile::tempdir()?;
    let staged = image::stage(image, staging.path())?;
    let segment = bundle::build_rootfs_segment(&staged.rootfs)?;
    if let (Some(key), Some(dir)) = (&key, &cache_dir) {
        cache::store(dir, key, &segment, &staged.config);
    }
    Ok((segment, staged.config))
}

/// The injected init prints `HARMONY_OCI_EXIT rc=<n>` before powering off.
fn parse_container_rc(serial: &[u8]) -> Option<i32> {
    let text = String::from_utf8_lossy(serial);
    text.lines()
        .rev()
        .find_map(|l| l.trim().strip_prefix("HARMONY_OCI_EXIT rc="))
        .and_then(|rc| rc.trim().parse().ok())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The base initramfs is chosen by preference order, not by the order
    /// the guest directory listed its files.
    #[test]
    fn base_initramfs_follows_preference_order() {
        let dir = std::path::Path::new("/g");
        let installed: Vec<PathBuf> = ["initramfs-postgres.cpio.gz", "initramfs-oci.cpio.gz"]
            .iter()
            .map(|n| dir.join(n))
            .collect();
        assert_eq!(
            select_base_initramfs(&installed),
            Some(&dir.join("initramfs-oci.cpio.gz"))
        );
        let only_postgres = vec![dir.join("initramfs-postgres.cpio.gz")];
        assert_eq!(
            select_base_initramfs(&only_postgres),
            Some(&dir.join("initramfs-postgres.cpio.gz"))
        );
    }

    /// An installed initramfs that is not a container-capable base is not a
    /// base: `harmony oci run` cannot inject a bundle into it.
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
        let serial = b"noise\nHARMONY_OCI_EXIT rc=3\ntail\nHARMONY_OCI_EXIT rc=0\n";
        assert_eq!(super::parse_container_rc(serial), Some(0));
        assert_eq!(super::parse_container_rc(b"no marker"), None);
    }

    #[test]
    fn hex_is_lowercase_zero_padded() {
        assert_eq!(super::hex(&[0x00, 0x0f, 0xab]), "000fab");
    }
}
