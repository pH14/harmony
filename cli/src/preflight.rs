// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::host::{Detected, HostReport, Hypervisor, Isa, MatrixCell};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

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

    fn scan(dir: &Path) -> Self {
        let kernel = ["Image", "bzImage"]
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
    nested: Detected,
    container: Detected,
    hypervisor: Hypervisor,
    matrix_cell: MatrixCell,
    run_loop: bool,
    guest: GuestArtifacts,
    base_initramfs: Option<PathBuf>,
    ready: bool,
    blockers: Vec<String>,
}

fn blockers(
    hypervisor: &Hypervisor,
    cell: MatrixCell,
    run_loop: bool,
    kernel: Option<&PathBuf>,
    base_initramfs: Option<&PathBuf>,
) -> Vec<String> {
    let mut blockers = Vec::new();
    if !hypervisor.available() {
        blockers.push(format!(
            "hypervisor unavailable: {}",
            hypervisor.detail().unwrap_or("no detail")
        ));
    }
    match cell {
        MatrixCell::Proven => {}
        MatrixCell::Expected => blockers.push(
            "support-matrix cell is untested: `harmony oci run` refuses it without \
             --allow-untested (docs/DETERMINISM.md §4)"
                .to_string(),
        ),
        MatrixCell::Unsupported => {
            blockers
                .push("host is outside the support matrix (docs/DETERMINISM.md §4)".to_string());
        }
    }
    if !run_loop {
        blockers.push(format!(
            "no run loop for this host in this build; wired hosts are {}",
            crate::oci::SUPPORTED_HOSTS
        ));
    }
    if kernel.is_none() {
        blockers.push(
            "no guest kernel: set HARMONY_GUEST_DIR or reinstall (expected \
             share/harmony/guest/<isa>/ next to this binary)"
                .to_string(),
        );
    }
    if base_initramfs.is_none() {
        blockers.push(crate::oci::missing_base_initramfs());
    }
    blockers
}

pub fn run(json: bool) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let host = HostReport::detect();
    let guest = GuestArtifacts::locate(host.isa);
    let base_initramfs = crate::oci::select_base_initramfs(&guest.initramfs).cloned();
    let blockers = blockers(
        &host.hypervisor,
        host.cell,
        crate::oci::HOST_SUPPORTED,
        guest.kernel.as_ref(),
        base_initramfs.as_ref(),
    );

    let report = Report {
        os: host.os,
        isa: host.isa,
        nested: host.nested,
        container: host.container,
        hypervisor: host.hypervisor,
        matrix_cell: host.cell,
        run_loop: crate::oci::HOST_SUPPORTED,
        guest,
        base_initramfs,
        ready: blockers.is_empty(),
        blockers,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_text(&report);
    }
    Ok(if report.ready {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn print_text(r: &Report) {
    println!(
        "host        {} / {} (in a VM: {}, in a container: {})",
        r.os, r.isa, r.nested, r.container
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
    match &r.base_initramfs {
        Some(base) => println!("base        {}", base.display()),
        None => println!(
            "base        NONE of {}",
            crate::oci::BASE_INITRAMFS.join(", ")
        ),
    }
    println!("ready       {}", if r.ready { "yes" } else { "no" });
    for blocker in &r.blockers {
        println!("            - {blocker}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready_inputs() -> (Hypervisor, MatrixCell, bool, PathBuf, PathBuf) {
        (
            Hypervisor::Kvm,
            MatrixCell::Proven,
            true,
            PathBuf::from("/g/bzImage"),
            PathBuf::from("/g/initramfs-oci.cpio.gz"),
        )
    }

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

        std::fs::write(dir.path().join("Image"), b"x").unwrap();
        let found = GuestArtifacts::scan(dir.path());
        assert_eq!(found.kernel, Some(dir.path().join("Image")));
    }

    #[test]
    fn a_host_meeting_every_requirement_is_ready() {
        let (hv, cell, run_loop, kernel, base) = ready_inputs();
        assert!(blockers(&hv, cell, run_loop, Some(&kernel), Some(&base)).is_empty());
    }

    #[test]
    fn missing_base_initramfs_blocks_readiness() {
        let (hv, cell, run_loop, kernel, _) = ready_inputs();
        let found = blockers(&hv, cell, run_loop, Some(&kernel), None);
        assert_eq!(found.len(), 1);
        assert!(found[0].contains("initramfs-oci.cpio.gz"), "{found:?}");
    }

    #[test]
    fn missing_run_loop_blocks_a_proven_cell() {
        let (hv, cell, _, kernel, base) = ready_inputs();
        let found = blockers(&hv, cell, false, Some(&kernel), Some(&base));
        assert_eq!(found.len(), 1);
        assert!(
            found[0].starts_with("no run loop for this host"),
            "{found:?}"
        );
    }

    #[test]
    fn untested_and_unsupported_cells_block_readiness() {
        let (hv, _, run_loop, kernel, base) = ready_inputs();
        for cell in [MatrixCell::Expected, MatrixCell::Unsupported] {
            let found = blockers(&hv, cell, run_loop, Some(&kernel), Some(&base));
            assert_eq!(found.len(), 1, "{cell:?}");
        }
    }

    #[test]
    fn every_unmet_requirement_is_reported_at_once() {
        let hv = Hypervisor::Unavailable("/dev/kvm does not exist".into());
        let found = blockers(&hv, MatrixCell::Unsupported, false, None, None);
        assert_eq!(found.len(), 5, "{found:?}");
        assert!(found[0].contains("/dev/kvm does not exist"));
    }

    #[test]
    fn a_missing_kernel_blocks_readiness() {
        let (hv, cell, run_loop, _, base) = ready_inputs();
        let found = blockers(&hv, cell, run_loop, None, Some(&base));
        assert_eq!(found.len(), 1);
        assert!(found[0].contains("no guest kernel"), "{found:?}");
    }
}
