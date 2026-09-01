// SPDX-License-Identifier: AGPL-3.0-or-later
//! Host capability detection for the support matrix in docs/DETERMINISM.md §4.
//!
//! Detection is deliberately cheap and read-only: file metadata, `/proc`
//! reads, and one `sysctl` subprocess on macOS. Anything this module cannot
//! establish is reported as unknown, and callers fail closed on unknown.

use serde::Serialize;
use std::fmt;

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
    // `sysctl -n kern.hv_support` avoids an unsafe sysctlbyname call; preflight
    // is not on any hot path.
    let out = std::process::Command::new("sysctl")
        .args(["-n", "kern.hv_support"])
        .output();
    match out {
        Ok(out) if out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "1" => {
            Hypervisor::Hvf
        }
        Ok(_) => Hypervisor::Unavailable(
            "kern.hv_support is not 1; Hypervisor.framework is unavailable on this machine".into(),
        ),
        Err(err) => Hypervisor::Unavailable(format!("could not query kern.hv_support: {err}")),
    }
}

/// One row+column cell of the DETERMINISM.md §4 support matrix.
#[derive(Clone, Debug, Serialize)]
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
    pub nested: bool,
    pub cell: MatrixCell,
}

impl HostReport {
    pub fn detect() -> Self {
        let isa = Isa::current();
        let os = std::env::consts::OS;
        let hypervisor = Hypervisor::detect();
        let nested = detect_nested();
        let cell = classify(isa, os, nested);
        HostReport {
            isa,
            os,
            hypervisor,
            nested,
            cell,
        }
    }
}

/// True when the host itself runs inside a VM (Linux: `hypervisor` flag in
/// /proc/cpuinfo; macOS: `kern.hv_vmm_present`).
fn detect_nested() -> bool {
    match std::env::consts::OS {
        "linux" => std::fs::read_to_string("/proc/cpuinfo")
            .map(|s| {
                s.lines()
                    .any(|l| l.starts_with("flags") && l.contains(" hypervisor"))
            })
            .unwrap_or(false),
        "macos" => std::process::Command::new("sysctl")
            .args(["-n", "kern.hv_vmm_present"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "1")
            .unwrap_or(false),
        _ => false,
    }
}

/// docs/DETERMINISM.md §4, encoded. Rows are (os, nested); columns are ISA.
fn classify(isa: Isa, os: &str, nested: bool) -> MatrixCell {
    match (os, isa, nested) {
        // Linux KVM nested-in-a-VM: proven on x86 (Intel and AMD, X2/X3).
        ("linux", Isa::X86_64, true) => MatrixCell::Proven,
        // Linux KVM bare metal arm64: proven (M4–M5 on metal).
        ("linux", Isa::Arm64, false) => MatrixCell::Proven,
        // Linux KVM bare-metal x86 and nested arm64: expected, untested.
        ("linux", Isa::X86_64, false) | ("linux", Isa::Arm64, true) => MatrixCell::Expected,
        // macOS HVF on Apple silicon: proven bare metal (M0–M6); nested
        // macOS-in-macOS is expected only.
        ("macos", Isa::Arm64, false) => MatrixCell::Proven,
        ("macos", Isa::Arm64, true) => MatrixCell::Expected,
        _ => MatrixCell::Unsupported,
    }
}

#[cfg(test)]
mod tests {
    use super::{Hypervisor, Isa, MatrixCell, classify};

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
    }

    /// Every cell of the DETERMINISM.md §4 matrix, including the fallthrough.
    #[test]
    fn matrix_cells() {
        let cell = |isa, os, nested| format!("{:?}", classify(isa, os, nested));
        assert_eq!(cell(Isa::X86_64, "linux", true), "Proven");
        assert_eq!(cell(Isa::Arm64, "linux", false), "Proven");
        assert_eq!(cell(Isa::X86_64, "linux", false), "Expected");
        assert_eq!(cell(Isa::Arm64, "linux", true), "Expected");
        assert_eq!(cell(Isa::Arm64, "macos", false), "Proven");
        assert_eq!(cell(Isa::Arm64, "macos", true), "Expected");
        assert_eq!(cell(Isa::X86_64, "macos", false), "Unsupported");
        assert_eq!(cell(Isa::Other, "linux", false), "Unsupported");
    }

    #[test]
    fn matrix_cell_is_serializable() {
        let s = serde_json::to_string(&MatrixCell::Proven).unwrap();
        assert_eq!(s, "\"proven\"");
    }
}
