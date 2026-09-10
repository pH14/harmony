// SPDX-License-Identifier: AGPL-3.0-or-later
//! `harmony preflight`: report the host's support-matrix cell, hypervisor
//! availability, and installed guest artifacts, then exit 0 only if
//! `harmony oci run` would be allowed to start.
//!
//! Readiness is the conjunction of every requirement the run path checks,
//! evaluated from the same predicates the run path uses, and it fails closed:
//! anything unestablished is a blocker, and every blocker is named.

use crate::host::{Detected, HostReport, Hypervisor, Isa, MatrixCell};
use serde::Serialize;
use std::path::{Path, PathBuf};
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

    fn scan(dir: &Path) -> Self {
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
    nested: Detected,
    container: Detected,
    hypervisor: Hypervisor,
    matrix_cell: MatrixCell,
    /// Whether this build carries a drive loop for this host.
    run_loop: bool,
    guest: GuestArtifacts,
    /// The installed initramfs `harmony oci run` would inject into.
    base_initramfs: Option<PathBuf>,
    ready: bool,
    /// One entry per unmet requirement, empty when ready.
    blockers: Vec<String>,
    /// What a workload bundle declares, when one was named.
    #[serde(skip_serializing_if = "Option::is_none")]
    bundle: Option<BundleReport>,
}

/// What one workload bundle file declares, and what is missing from it.
///
/// A campaign costs hours, and an investigation can only report meanings the
/// bundle wrote down. Both are read here from the same parser the run path
/// uses, so a bundle that passes this reads the same way later.
#[derive(Serialize)]
struct BundleReport {
    path: PathBuf,
    /// The parse error, when the file is not a bundle. Every other field is
    /// then empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    nodes: Vec<String>,
    hooks: Vec<u32>,
    /// One `kind id: meaning` line per declared property.
    assertions: Vec<String>,
    diagnostics: Vec<String>,
    setup: bool,
    ready: bool,
    /// Declarations that name something the bundle does not have, plus the
    /// observability an investigation would lack. Empty means complete.
    blockers: Vec<String>,
}

/// Everything wrong with a parsed bundle, in report order.
///
/// Split from the reading so the rules are testable without a file.
fn bundle_blockers(d: &faults_workload::declarations::Declarations) -> Vec<String> {
    let mut blockers = Vec::new();
    if d.nodes.is_empty() {
        blockers.push("no `node` line: the image supervises nothing".to_string());
    }
    for assertion in &d.assertions {
        if let Some(hook) = assertion.reported_by_hook
            && !d.hooks.iter().any(|h| h.id == hook)
        {
            blockers.push(format!(
                "assertion {} is reported by hook {hook}, which the bundle does not declare",
                assertion.id
            ));
        }
        if assertion.reported_by_hook.is_none() {
            blockers.push(format!(
                "assertion {} names no `from <hook>`: a report cannot say which command                  evaluated it",
                assertion.id
            ));
        }
    }
    for hook in &d.hooks {
        if hook.description.is_none() {
            blockers.push(format!(
                "hook {} has no `describe hook {}` line: a finding cannot say what ran",
                hook.id, hook.id
            ));
        }
    }
    if d.assertions.is_empty() {
        blockers.push(
            "no `assert` line: a finding would name a property number with no claim attached"
                .to_string(),
        );
    }
    if d.diagnostics.is_empty() {
        blockers.push(
            "no `diagnostic` line: an investigation has no declared command for reading guest evidence"
                .to_string(),
        );
    }
    blockers
}

impl BundleReport {
    fn read(path: &Path) -> Self {
        let empty = |error: Option<String>| BundleReport {
            path: path.to_path_buf(),
            error,
            nodes: Vec::new(),
            hooks: Vec::new(),
            assertions: Vec::new(),
            diagnostics: Vec::new(),
            setup: false,
            ready: false,
            blockers: Vec::new(),
        };
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => return empty(Some(error.to_string())),
        };
        let declared = match faults_workload::declarations::Declarations::parse(&text) {
            Ok(declared) => declared,
            Err(error) => return empty(Some(error)),
        };
        BundleReport {
            path: path.to_path_buf(),
            error: None,
            nodes: declared.nodes.iter().map(|n| n.name.clone()).collect(),
            hooks: declared.hooks.iter().map(|h| h.id).collect(),
            assertions: declared
                .assertions
                .iter()
                .map(|a| format!("{} {}: {}", a.kind.keyword(), a.id, a.meaning))
                .collect(),
            diagnostics: declared
                .diagnostics
                .iter()
                .map(|d| d.name.clone())
                .collect(),
            setup: !declared.setup.is_empty(),
            ready: !declared.ready.is_empty(),
            blockers: bundle_blockers(&declared),
        }
    }
}

/// Every requirement `harmony oci run` checks before it can start, in report
/// order. Empty means ready. Pure in its inputs, so the readiness contract is
/// testable without depending on the executing host.
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

pub fn run(json: bool, bundle: Option<&Path>) -> Result<ExitCode, Box<dyn std::error::Error>> {
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

    let bundle = bundle.map(BundleReport::read);
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
        bundle,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_text(&report);
    }
    let bundle_ready = report
        .bundle
        .as_ref()
        .is_none_or(|b| b.error.is_none() && b.blockers.is_empty());
    Ok(if report.ready && bundle_ready {
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
    if let Some(b) = &r.bundle {
        println!("bundle      {}", b.path.display());
        if let Some(error) = &b.error {
            println!("            UNREADABLE: {error}");
            return;
        }
        println!(
            "            nodes {}",
            if b.nodes.is_empty() {
                "none".to_string()
            } else {
                b.nodes.join(", ")
            }
        );
        println!(
            "            hooks {}",
            if b.hooks.is_empty() {
                "none".to_string()
            } else {
                b.hooks
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        );
        for assertion in &b.assertions {
            println!("            assert {assertion}");
        }
        for diagnostic in &b.diagnostics {
            println!("            diagnostic {diagnostic}");
        }
        println!(
            "            setup {} / ready {}",
            if b.setup { "declared" } else { "none" },
            if b.ready { "declared" } else { "none" }
        );
        println!(
            "            complete {}",
            if b.blockers.is_empty() { "yes" } else { "no" }
        );
        for blocker in &b.blockers {
            println!("            - {blocker}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use faults_workload::declarations::Declarations;

    const COMPLETE: &str = "\
node postgres /opt/harmony/node.sh
setup /opt/harmony/setup.sh
ready /opt/harmony/ready.sh
hook 3 /opt/harmony/hooks.sh 3
describe hook 3 pg_amcheck --heapallindexed against cic_k_idx
assert always 2 from 3 every heap tuple has a matching index tuple
diagnostic amcheck /usr/bin/pg_amcheck --heapallindexed
";

    #[test]
    fn a_complete_bundle_has_no_blockers() {
        let declared = Declarations::parse(COMPLETE).expect("parse");
        assert!(bundle_blockers(&declared).is_empty(), "{declared:?}");
    }

    /// A property whose hook is not declared would produce a finding naming a
    /// command the workspace cannot describe.
    #[test]
    fn an_assertion_pointing_at_a_missing_hook_blocks() {
        let text = COMPLETE.replace("from 3", "from 9");
        let declared = Declarations::parse(&text).expect("parse");
        let found = bundle_blockers(&declared);
        assert!(found.iter().any(|b| b.contains("hook 9")), "{found:?}");
    }

    /// Silence from a failure-only check is not a pass. A bundle that declares
    /// no property at all gives a finding nothing to claim.
    #[test]
    fn a_bundle_without_properties_or_diagnostics_blocks() {
        let declared = Declarations::parse("node a /bin/a\nhook 1 /bin/h\n").expect("parse");
        let found = bundle_blockers(&declared);
        assert!(found.iter().any(|b| b.contains("`assert`")), "{found:?}");
        assert!(
            found.iter().any(|b| b.contains("`diagnostic`")),
            "{found:?}"
        );
        assert!(
            found.iter().any(|b| b.contains("describe hook 1")),
            "{found:?}"
        );
    }

    /// The report names what the bundle declares, including whether it has a
    /// setup and a readiness command: a workload with neither starts its nodes
    /// against an unprepared filesystem.
    #[test]
    fn the_report_names_what_the_bundle_declares() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bundle");
        std::fs::write(&path, COMPLETE).unwrap();
        let report = BundleReport::read(&path);
        assert_eq!(report.error, None);
        assert_eq!(report.nodes, ["postgres"]);
        assert_eq!(report.hooks, [3]);
        assert_eq!(report.diagnostics, ["amcheck"]);
        assert!(report.setup, "the bundle declares a setup command");
        assert!(report.ready, "and a readiness command");
        assert!(report.blockers.is_empty(), "{:?}", report.blockers);

        let bare = dir.path().join("bare");
        std::fs::write(&bare, "node a /bin/a\n").unwrap();
        let report = BundleReport::read(&bare);
        assert!(!report.setup, "this one declares neither");
        assert!(!report.ready);
    }

    #[test]
    fn a_bundle_that_does_not_parse_is_reported_as_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bundle");
        std::fs::write(&path, b"node\n").unwrap();
        let report = BundleReport::read(&path);
        assert!(report.error.is_some(), "a malformed bundle names its error");
    }

    #[test]
    fn a_missing_bundle_file_is_reported_rather_than_panicking() {
        let report = BundleReport::read(Path::new("/nonexistent/bundle"));
        assert!(report.error.is_some());
    }

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

        // Image-postgres outranks bzImage (the arm64 container kernel).
        std::fs::write(dir.path().join("Image-postgres"), b"x").unwrap();
        let found = GuestArtifacts::scan(dir.path());
        assert_eq!(found.kernel, Some(dir.path().join("Image-postgres")));
    }

    #[test]
    fn a_host_meeting_every_requirement_is_ready() {
        let (hv, cell, run_loop, kernel, base) = ready_inputs();
        assert!(blockers(&hv, cell, run_loop, Some(&kernel), Some(&base)).is_empty());
    }

    /// A proven cell with a kernel and a hypervisor is still not ready
    /// without an accepted base initramfs: there is nothing to inject the
    /// container bundle into.
    #[test]
    fn missing_base_initramfs_blocks_readiness() {
        let (hv, cell, run_loop, kernel, _) = ready_inputs();
        let found = blockers(&hv, cell, run_loop, Some(&kernel), None);
        assert_eq!(found.len(), 1);
        assert!(found[0].contains("initramfs-oci.cpio.gz"), "{found:?}");
    }

    /// Linux/arm64 bare metal is a proven cell with no drive loop compiled
    /// for it. Readiness follows the run loop, not the matrix cell alone.
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
