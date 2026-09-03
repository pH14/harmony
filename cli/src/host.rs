// SPDX-License-Identifier: AGPL-3.0-or-later
//! Host capability detection for the support matrix in docs/DETERMINISM.md §4.
//!
//! Detection is deliberately cheap and read-only: file metadata, `/proc`
//! reads, and one `sysctl` subprocess on macOS. Anything this module cannot
//! establish is reported as unknown, and callers fail closed on unknown.

use serde::Serialize;
use std::fmt;
use std::path::Path;

/// Instruction-set architecture of the host. Run artifacts are ISA-scoped;
/// no cross-ISA byte identity is claimed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Isa {
    X86_64,
    Arm64,
    /// Compiled for an architecture the support matrix does not cover.
    Other,
}

impl fmt::Display for Isa {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Isa::X86_64 => f.write_str("x86-64"),
            Isa::Arm64 => f.write_str("arm64"),
            Isa::Other => f.write_str(std::env::consts::ARCH),
        }
    }
}

impl Isa {
    pub fn current() -> Self {
        match std::env::consts::ARCH {
            "x86_64" => Isa::X86_64,
            "aarch64" => Isa::Arm64,
            _ => Isa::Other,
        }
    }

    /// Directory name for per-ISA guest artifacts (`share/harmony/guest/<isa>/`).
    pub fn guest_dir_name(self) -> &'static str {
        match self {
            Isa::X86_64 => "x86_64",
            Isa::Arm64 => "arm64",
            Isa::Other => "unsupported",
        }
    }
}

/// The answer to a yes/no question about the host, with the third state
/// detection actually has. A probe whose sources are missing or unreadable
/// answers `Unknown`, and `Unknown` never collapses into `No`: a matrix cell
/// is claimed proven only on a positive `No`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Detected {
    Yes,
    No,
    Unknown,
}

impl From<bool> for Detected {
    fn from(yes: bool) -> Self {
        if yes { Detected::Yes } else { Detected::No }
    }
}

impl fmt::Display for Detected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Detected::Yes => "yes",
            Detected::No => "no",
            Detected::Unknown => "unknown",
        })
    }
}

/// Whether the host hypervisor is usable, and if not, why.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "kebab-case", tag = "state", content = "detail")]
pub enum Hypervisor {
    /// Linux with `/dev/kvm` present and openable read-write.
    Kvm,
    /// macOS with `kern.hv_support = 1`.
    Hvf,
    /// The hypervisor exists but this process cannot use it.
    Unavailable(String),
    /// No supported hypervisor on this OS.
    Unsupported(String),
}

impl Hypervisor {
    pub fn detect() -> Self {
        match std::env::consts::OS {
            "linux" => detect_kvm(),
            "macos" => detect_hvf(),
            other => Hypervisor::Unsupported(format!(
                "no supported hypervisor on {other}; supported hosts are Linux (KVM) and macOS (HVF)"
            )),
        }
    }

    pub fn available(&self) -> bool {
        matches!(self, Hypervisor::Kvm | Hypervisor::Hvf)
    }

    /// Why the hypervisor cannot be used, for a refusal message.
    pub fn detail(&self) -> Option<&str> {
        match self {
            Hypervisor::Kvm | Hypervisor::Hvf => None,
            Hypervisor::Unavailable(why) | Hypervisor::Unsupported(why) => Some(why),
        }
    }
}

fn detect_kvm() -> Hypervisor {
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/kvm")
    {
        Ok(_) => Hypervisor::Kvm,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Hypervisor::Unavailable(
            "/dev/kvm does not exist; enable hardware virtualization (or nested \
             virtualization in this VM)"
                .into(),
        ),
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => Hypervisor::Unavailable(
            "/dev/kvm exists but is not writable; add this user to the kvm group".into(),
        ),
        Err(err) => Hypervisor::Unavailable(format!("/dev/kvm: {err}")),
    }
}

fn detect_hvf() -> Hypervisor {
    match sysctl("kern.hv_support").as_deref() {
        Some("1") => Hypervisor::Hvf,
        Some(_) => Hypervisor::Unavailable(
            "kern.hv_support is not 1; Hypervisor.framework is unavailable on this machine".into(),
        ),
        None => Hypervisor::Unavailable("could not query kern.hv_support".into()),
    }
}

/// One `sysctl -n <name>` read. The subprocess avoids an unsafe
/// `sysctlbyname` call; detection is not on any hot path.
fn sysctl(name: &str) -> Option<String> {
    let out = std::process::Command::new("sysctl")
        .args(["-n", name])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!value.is_empty()).then_some(value)
}

/// One row+column cell of the DETERMINISM.md §4 support matrix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MatrixCell {
    /// Committed cross-host evidence exists for this cell.
    Proven,
    /// The design covers this cell but no committed evidence exists yet.
    /// Untested per the contract; hypervisor verbs refuse it without
    /// `--allow-untested`.
    Expected,
    /// Outside the matrix entirely.
    Unsupported,
}

/// Where this host falls in the support matrix.
pub struct HostReport {
    pub isa: Isa,
    pub os: &'static str,
    pub hypervisor: Hypervisor,
    pub nested: Detected,
    pub container: Detected,
    pub cell: MatrixCell,
}

impl HostReport {
    pub fn detect() -> Self {
        let isa = Isa::current();
        let os = std::env::consts::OS;
        let hypervisor = Hypervisor::detect();
        let nested = detect_nested();
        let container = detect_container();
        let cell = classify(isa, os, nested, container);
        HostReport {
            isa,
            os,
            hypervisor,
            nested,
            container,
            cell,
        }
    }
}

/// DMI vendor strings that name a virtual machine. Read on ACPI-booted arm64
/// machines, which have neither the x86 CPUID flag nor a device tree.
const VM_DMI_VENDORS: &[&str] = &[
    "QEMU",
    "VMware",
    "Xen",
    "Microsoft Corporation",
    "Amazon EC2",
    "Google",
    "Parallels",
    "innotek",
    "Oracle Corporation",
    "KVM",
];

/// Whether the host itself runs inside a VM.
///
/// x86 reads the `hypervisor` CPUID flag out of /proc/cpuinfo. arm64 has no
/// such flag, so the sources are the device tree (a virtualized machine
/// carries a `/hypervisor` node) and, on ACPI-booted machines, the DMI vendor
/// strings. macOS reads `kern.hv_vmm_present`. A source that is unreadable
/// leaves the answer unknown rather than claiming bare metal.
fn detect_nested() -> Detected {
    match (std::env::consts::OS, Isa::current()) {
        ("linux", Isa::X86_64) => match std::fs::read_to_string("/proc/cpuinfo") {
            Ok(info) => info
                .lines()
                .any(|l| l.starts_with("flags") && l.contains(" hypervisor"))
                .into(),
            Err(_) => Detected::Unknown,
        },
        ("linux", Isa::Arm64) => detect_nested_arm64(),
        ("macos", _) => match sysctl("kern.hv_vmm_present") {
            Some(value) => (value == "1").into(),
            None => Detected::Unknown,
        },
        _ => Detected::Unknown,
    }
}

fn detect_nested_arm64() -> Detected {
    if Path::new("/proc/device-tree/hypervisor").exists() {
        return Detected::Yes;
    }
    // A device-tree machine that advertises no hypervisor node is bare metal.
    if Path::new("/proc/device-tree/compatible").exists() {
        return Detected::No;
    }
    match std::fs::read_to_string("/sys/class/dmi/id/sys_vendor") {
        Ok(vendor) => VM_DMI_VENDORS.iter().any(|v| vendor.contains(v)).into(),
        Err(_) => Detected::Unknown,
    }
}

/// Container-engine cgroup paths, as they appear in PID 1's cgroup line.
const CONTAINER_CGROUPS: &[&str] = &[
    "/docker",
    "/kubepods",
    "/lxc",
    "/libpod",
    "/containerd",
    "/podman",
    "/garden",
];

/// Process names a host's PID 1 is allowed to have. Anything else means the
/// process tree was started by something that is not a host init, which the
/// container question cannot resolve from here.
const HOST_INIT_NAMES: &[&str] = &[
    "systemd",
    "init",
    "launchd",
    "openrc-init",
    "runit",
    "s6-svscan",
];

/// Whether this process runs inside a container — its own support-matrix row
/// (docs/DETERMINISM.md §4), because a container on a nested host inherits
/// the host's virtualization signals and would otherwise be reported as the
/// host's row. Any positive signal answers yes; only a recognized host init
/// answers no.
fn detect_container() -> Detected {
    match std::env::consts::OS {
        "linux" => detect_container_linux(),
        // Hypervisor.framework is not reachable from a Linux container, so
        // the HVF rows have no container variant.
        "macos" => Detected::No,
        _ => Detected::Unknown,
    }
}

fn detect_container_linux() -> Detected {
    // Marker files written by the docker and podman runtimes.
    if Path::new("/.dockerenv").exists() || Path::new("/run/.containerenv").exists() {
        return Detected::Yes;
    }
    // PID 1's cgroup names the engine under cgroup v1, and under cgroup v2
    // whenever the container did not get its own cgroup namespace.
    let Ok(cgroup) = std::fs::read_to_string("/proc/1/cgroup") else {
        return Detected::Unknown;
    };
    if CONTAINER_CGROUPS
        .iter()
        .any(|engine| cgroup.contains(engine))
    {
        return Detected::Yes;
    }
    // PID 1's name: `comm` on any kernel that exposes it, `sched` (whose
    // first line starts with the same name) as the fallback.
    match std::fs::read_to_string("/proc/1/comm")
        .or_else(|_| std::fs::read_to_string("/proc/1/sched"))
    {
        Ok(name) => host_init_name(&name),
        Err(_) => Detected::Unknown,
    }
}

/// Whether PID 1's name is a host init. `/proc/1/comm` holds the name alone;
/// `/proc/1/sched` starts its first line with it.
fn host_init_name(pid1: &str) -> Detected {
    let Some(name) = pid1.split_whitespace().next() else {
        return Detected::Unknown;
    };
    if HOST_INIT_NAMES.contains(&name) {
        Detected::No
    } else {
        Detected::Unknown
    }
}

/// docs/DETERMINISM.md §4, encoded. Rows are (os, container, nested);
/// columns are ISA. Unknown detection never yields `Proven`.
fn classify(isa: Isa, os: &str, nested: Detected, container: Detected) -> MatrixCell {
    match (os, isa) {
        ("linux", Isa::X86_64 | Isa::Arm64) | ("macos", Isa::Arm64) => {}
        _ => return MatrixCell::Unsupported,
    }
    // "Inside a container with /dev/kvm" is its own row, expected on every
    // ISA. An unresolved container answer takes the same untested row rather
    // than inheriting the bare-metal or nested one.
    if container != Detected::No {
        return MatrixCell::Expected;
    }
    match (os, isa, nested) {
        // Linux KVM nested-in-a-VM: proven on x86 (Intel and AMD, X2/X3).
        ("linux", Isa::X86_64, Detected::Yes) => MatrixCell::Proven,
        // Linux KVM bare metal arm64: proven (M4–M5 on metal).
        ("linux", Isa::Arm64, Detected::No) => MatrixCell::Proven,
        // macOS HVF on Apple silicon: proven bare metal (M0–M6).
        ("macos", Isa::Arm64, Detected::No) => MatrixCell::Proven,
        // Linux bare-metal x86, nested arm64, nested macOS, and every host
        // whose nesting could not be established: expected, untested.
        _ => MatrixCell::Expected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isa_display_names() {
        assert_eq!(Isa::X86_64.to_string(), "x86-64");
        assert_eq!(Isa::Arm64.to_string(), "arm64");
    }

    #[test]
    fn guest_dir_names() {
        assert_eq!(Isa::X86_64.guest_dir_name(), "x86_64");
        assert_eq!(Isa::Arm64.guest_dir_name(), "arm64");
        assert_eq!(Isa::Other.guest_dir_name(), "unsupported");
    }

    #[test]
    fn hypervisor_availability() {
        assert!(Hypervisor::Kvm.available());
        assert!(Hypervisor::Hvf.available());
        assert!(!Hypervisor::Unavailable("x".into()).available());
        assert!(!Hypervisor::Unsupported("x".into()).available());
        assert_eq!(Hypervisor::Kvm.detail(), None);
        assert_eq!(Hypervisor::Unavailable("why".into()).detail(), Some("why"));
        assert_eq!(Hypervisor::Unsupported("why".into()).detail(), Some("why"));
    }

    /// Every cell of the DETERMINISM.md §4 matrix, including the fallthrough.
    /// Pure in its inputs, so the whole matrix is checked off the executing
    /// host.
    #[test]
    fn matrix_cells() {
        let cell = |isa, os, nested| classify(isa, os, nested, Detected::No);
        assert_eq!(
            cell(Isa::X86_64, "linux", Detected::Yes),
            MatrixCell::Proven
        );
        assert_eq!(cell(Isa::Arm64, "linux", Detected::No), MatrixCell::Proven);
        assert_eq!(cell(Isa::Arm64, "macos", Detected::No), MatrixCell::Proven);
        assert_eq!(
            cell(Isa::X86_64, "linux", Detected::No),
            MatrixCell::Expected
        );
        assert_eq!(
            cell(Isa::Arm64, "linux", Detected::Yes),
            MatrixCell::Expected
        );
        assert_eq!(
            cell(Isa::Arm64, "macos", Detected::Yes),
            MatrixCell::Expected
        );
        assert_eq!(
            cell(Isa::X86_64, "macos", Detected::No),
            MatrixCell::Unsupported
        );
        assert_eq!(
            cell(Isa::Other, "linux", Detected::No),
            MatrixCell::Unsupported
        );
        assert_eq!(
            cell(Isa::X86_64, "windows", Detected::No),
            MatrixCell::Unsupported
        );
    }

    /// Unknown nesting must not be read as bare metal: every host whose
    /// nesting is unresolved is untested, never proven.
    #[test]
    fn unknown_nesting_is_never_proven() {
        for (isa, os) in [
            (Isa::X86_64, "linux"),
            (Isa::Arm64, "linux"),
            (Isa::Arm64, "macos"),
        ] {
            assert_eq!(
                classify(isa, os, Detected::Unknown, Detected::No),
                MatrixCell::Expected,
                "{os}/{isa}"
            );
        }
    }

    /// The container row is expected on every ISA, and an unresolved
    /// container answer takes that row instead of the host's. A container on
    /// a nested x86 host inherits the host's `hypervisor` CPUID flag, so
    /// without this the proven nested row would be claimed for it.
    #[test]
    fn container_row_is_never_proven() {
        for container in [Detected::Yes, Detected::Unknown] {
            assert_eq!(
                classify(Isa::X86_64, "linux", Detected::Yes, container),
                MatrixCell::Expected
            );
            assert_eq!(
                classify(Isa::Arm64, "linux", Detected::No, container),
                MatrixCell::Expected
            );
            // Still outside the matrix, container or not.
            assert_eq!(
                classify(Isa::X86_64, "macos", Detected::No, container),
                MatrixCell::Unsupported
            );
        }
    }

    #[test]
    fn pid1_name_resolves_only_known_host_inits() {
        assert_eq!(host_init_name("systemd\n"), Detected::No);
        assert_eq!(host_init_name("systemd (1, #threads: 1)\n"), Detected::No);
        assert_eq!(host_init_name("init (1, #threads: 1)\n"), Detected::No);
        assert_eq!(host_init_name("bash (1, #threads: 1)\n"), Detected::Unknown);
        assert_eq!(host_init_name(""), Detected::Unknown);
    }

    #[test]
    fn detected_serializes_as_kebab_case() {
        assert_eq!(serde_json::to_string(&Detected::Yes).unwrap(), "\"yes\"");
        assert_eq!(
            serde_json::to_string(&Detected::Unknown).unwrap(),
            "\"unknown\""
        );
        assert_eq!(Detected::from(true), Detected::Yes);
        assert_eq!(Detected::from(false), Detected::No);
        assert_eq!(Detected::Unknown.to_string(), "unknown");
    }

    #[test]
    fn matrix_cell_is_serializable() {
        let s = serde_json::to_string(&MatrixCell::Proven).unwrap();
        assert_eq!(s, "\"proven\"");
    }
}
