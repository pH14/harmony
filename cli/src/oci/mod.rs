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
const BASE_INITRAMFS: &[&str] = &[
    "initramfs-oci.cpio.gz",
    "initramfs-docker.cpio.gz",
    "initramfs-postgres.cpio.gz",
];

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
    segment_sha256: String,
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
    let base_path = BASE_INITRAMFS
        .iter()
        .find_map(|name| {
            guest
                .initramfs
                .iter()
                .find(|p| p.file_name().is_some_and(|f| f == *name))
        })
        .ok_or_else(|| {
            format!(
                "no container-capable guest initramfs found (looked for {}); build one with \
                 `make -C consonance/harmony-linux <arm64-oci-image|docker-image>`",
                BASE_INITRAMFS.join(", ")
            )
        })?;

    let kernel = std::fs::read(kernel_path)?;
    let base = std::fs::read(base_path)?;

    let staging = tempfile::tempdir()?;
    eprintln!("staging {} ...", args.image);
    let staged = image::stage(&args.image, staging.path())?;
    let segment = bundle::build_segment(&staged, &args.cmd)?;
    let mut initramfs = base.clone();
    initramfs.extend_from_slice(&segment);

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
        segment_sha256: hex(&Sha256::digest(&segment)),
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
    #[test]
    fn container_rc_parses_last_marker() {
        let serial = b"noise\nHARMONY_OCI_EXIT rc=3\ntail\nHARMONY_OCI_EXIT rc=0\n";
        assert_eq!(super::parse_container_rc(serial), Some(0));
        assert_eq!(super::parse_container_rc(b"no marker"), None);
    }
}
