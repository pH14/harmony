// SPDX-License-Identifier: AGPL-3.0-or-later
//! `harmony preflight`: report the host's support-matrix cell, hypervisor
//! availability, and installed guest artifacts, then exit 0 only if
//! `harmony oci run` would be allowed to start.

use crate::host::{HostReport, Hypervisor, Isa, MatrixCell};
use serde::Serialize;
use std::path::PathBuf;
use std::process::ExitCode;

/// Where the per-ISA guest artifacts (kernel + initramfs) were found, if
/// anywhere. Searched in order: `$HARMONY_GUEST_DIR`, `../share/harmony/guest/<isa>`
/// relative to the executable (the brew layout), then the in-repo dev build
/// tree.
#[derive(Serialize)]
pub struct GuestArtifacts {
    pub dir: Option<PathBuf>,
    pub kernel: Option<PathBuf>,
    pub initramfs: Vec<PathBuf>,
}

impl GuestArtifacts {
    pub fn locate(isa: Isa) -> Self {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Ok(dir) = std::env::var("HARMONY_GUEST_DIR") {
            candidates.push(PathBuf::from(dir));
        }
        if let Some(prefix) = std::env::current_exe()
            .ok()
            .as_deref()
            .and_then(|exe| exe.parent())
            .and_then(|bin| bin.parent())
        {
            candidates.push(
                prefix
                    .join("share/harmony/guest")
                    .join(isa.guest_dir_name()),
            );
        }
        // Dev fallback: running from a repo checkout.
        candidates.push(PathBuf::from("consonance/harmony-linux/build").join(isa.guest_dir_name()));

        for dir in candidates {
            let found = Self::scan(&dir);
            if found.kernel.is_some() {
                return found;
            }
        }
        GuestArtifacts {
            dir: None,
            kernel: None,
            initramfs: Vec::new(),
        }
    }

    fn scan(dir: &std::path::Path) -> Self {
        // arm64 uses the container-capable postgres-profile kernel; the
        // minimal Image lacks BINFMT_SCRIPT and namespaces.
        let kernel = ["Image-postgres", "bzImage"]
            .iter()
            .map(|n| dir.join(n))
            .find(|p| p.is_file());
        let mut initramfs: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("initramfs") && n.ends_with(".cpio.gz"))
            })
            .collect();
        initramfs.sort();
        GuestArtifacts {
            dir: Some(dir.to_path_buf()),
            kernel,
            initramfs,
        }
    }
}

#[derive(Serialize)]
struct Report {
    os: &'static str,
    isa: Isa,
    nested: bool,
    hypervisor: Hypervisor,
    matrix_cell: MatrixCell,
    guest: GuestArtifacts,
    ready: bool,
}

pub fn run(json: bool) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let host = HostReport::detect();
    let guest = GuestArtifacts::locate(host.isa);
    let ready = host.hypervisor.available()
        && matches!(host.cell, MatrixCell::Proven)
        && guest.kernel.is_some();

    let report = Report {
        os: host.os,
        isa: host.isa,
        nested: host.nested,
        hypervisor: host.hypervisor,
        matrix_cell: host.cell,
        guest,
        ready,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_text(&report);
    }
    Ok(if ready {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn print_text(r: &Report) {
    println!(
        "host        {} / {}{}",
        r.os,
        r.isa,
        if r.nested { " (inside a VM)" } else { "" }
    );
    match &r.hypervisor {
        Hypervisor::Kvm => println!("hypervisor  KVM (/dev/kvm) available"),
        Hypervisor::Hvf => println!("hypervisor  HVF (Hypervisor.framework) available"),
        Hypervisor::Unavailable(why) => println!("hypervisor  UNAVAILABLE: {why}"),
        Hypervisor::Unsupported(why) => println!("hypervisor  UNSUPPORTED: {why}"),
    }
    match r.matrix_cell {
        MatrixCell::Proven => println!("determinism proven cell (docs/DETERMINISM.md §4)"),
        MatrixCell::Expected => println!(
            "determinism UNTESTED cell: the design covers this host but no committed \
             evidence exists (docs/DETERMINISM.md §4)"
        ),
        MatrixCell::Unsupported => println!("determinism unsupported host"),
    }
    match (&r.guest.dir, &r.guest.kernel) {
        (Some(dir), Some(kernel)) => {
            println!(
                "guest       {} (kernel {})",
                dir.display(),
                kernel.display()
            );
            for i in &r.guest.initramfs {
                if let Some(name) = i.file_name().and_then(|n| n.to_str()) {
                    println!("            {name}");
                }
            }
        }
        _ => println!(
            "guest       NOT FOUND: set HARMONY_GUEST_DIR or reinstall (expected \
             share/harmony/guest/<isa>/ next to this binary)"
        ),
    }
    println!("ready       {}", if r.ready { "yes" } else { "no" });
}

#[cfg(test)]
mod tests {
    use super::GuestArtifacts;

    #[test]
    fn scan_finds_kernel_and_filters_initramfs_names() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "bzImage",
            "initramfs-oci.cpio.gz",
            "initramfs-notes.txt",
            "other.cpio.gz",
        ] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        let found = GuestArtifacts::scan(dir.path());
        assert_eq!(found.kernel, Some(dir.path().join("bzImage")));
        assert_eq!(found.initramfs, [dir.path().join("initramfs-oci.cpio.gz")]);

        // Image-postgres outranks bzImage (the arm64 container kernel).
        std::fs::write(dir.path().join("Image-postgres"), b"x").unwrap();
        let found = GuestArtifacts::scan(dir.path());
        assert_eq!(found.kernel, Some(dir.path().join("Image-postgres")));
    }
}
