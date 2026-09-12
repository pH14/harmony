// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::Serialize;
use std::fmt;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Isa {
    X86_64,
    Arm64,
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

    pub fn guest_dir_name(self) -> &'static str {
        match self {
            Isa::X86_64 => "x86_64",
            Isa::Arm64 => "arm64",
            Isa::Other => "unsupported",
        }
    }
}

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

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "kebab-case", tag = "state", content = "detail")]
pub enum Hypervisor {
    Kvm,
    Hvf,
    Unavailable(String),
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

fn sysctl(name: &str) -> Option<String> {
    let out = std::process::Command::new("sysctl")
        .args(["-n", name])
        .output()
        .ok()?;
    sysctl_value(out.status.success(), &out.stdout)
}

fn sysctl_value(success: bool, stdout: &[u8]) -> Option<String> {
    if !success {
        return None;
    }
    let value = String::from_utf8_lossy(stdout).trim().to_string();
    (!value.is_empty()).then_some(value)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MatrixCell {
    Proven,
    Expected,
    Unsupported,
}

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

fn detect_nested() -> Detected {
    match (std::env::consts::OS, Isa::current()) {
        ("linux", Isa::X86_64) => match std::fs::read_to_string("/proc/cpuinfo") {
            Ok(info) => cpuinfo_nesting(&info),
            Err(_) => Detected::Unknown,
        },
        ("linux", Isa::Arm64) => arm64_nesting(
            Path::new("/proc/device-tree/hypervisor").exists(),
            std::fs::read_to_string("/sys/class/dmi/id/sys_vendor")
                .ok()
                .as_deref(),
        ),
        ("macos", _) => match sysctl("kern.hv_vmm_present") {
            Some(value) => (value == "1").into(),
            None => Detected::Unknown,
        },
        _ => Detected::Unknown,
    }
}

fn cpuinfo_nesting(cpuinfo: &str) -> Detected {
    let mut flags = cpuinfo.lines().filter(|l| l.starts_with("flags"));
    let Some(line) = flags.next() else {
        return Detected::Unknown;
    };
    line.contains(" hypervisor").into()
}

fn arm64_nesting(hypervisor_node: bool, dmi_vendor: Option<&str>) -> Detected {
    if hypervisor_node {
        return Detected::Yes;
    }
    match dmi_vendor {
        Some(vendor) if VM_DMI_VENDORS.iter().any(|v| vendor.contains(v)) => Detected::Yes,
        _ => Detected::Unknown,
    }
}

const CONTAINER_CGROUPS: &[&str] = &[
    "/docker",
    "/kubepods",
    "/lxc",
    "/libpod",
    "/containerd",
    "/podman",
    "/garden",
];

const HOST_INIT_NAMES: &[&str] = &[
    "systemd",
    "init",
    "launchd",
    "openrc-init",
    "runit",
    "s6-svscan",
];

fn detect_container() -> Detected {
    detect_container_for(std::env::consts::OS, detect_container_linux)
}

fn detect_container_for(os: &str, linux: impl FnOnce() -> Detected) -> Detected {
    match os {
        "linux" => linux(),
        "macos" => Detected::No,
        _ => Detected::Unknown,
    }
}

fn detect_container_linux() -> Detected {
    detect_container_linux_at(Path::new("/"))
}

fn detect_container_linux_at(root: &Path) -> Detected {
    if root.join(".dockerenv").exists() || root.join("run/.containerenv").exists() {
        return Detected::Yes;
    }
    let Ok(cgroup) = std::fs::read_to_string(root.join("proc/1/cgroup")) else {
        return Detected::Unknown;
    };
    if CONTAINER_CGROUPS
        .iter()
        .any(|engine| cgroup.contains(engine))
    {
        return Detected::Yes;
    }
    match std::fs::read_to_string(root.join("proc/1/comm"))
        .or_else(|_| std::fs::read_to_string(root.join("proc/1/sched")))
    {
        Ok(name) => host_init_name(&name),
        Err(_) => Detected::Unknown,
    }
}

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

fn classify(isa: Isa, os: &str, nested: Detected, container: Detected) -> MatrixCell {
    match (os, isa) {
        ("linux", Isa::X86_64 | Isa::Arm64) | ("macos", Isa::Arm64) => {}
        _ => return MatrixCell::Unsupported,
    }
    if container != Detected::No {
        return MatrixCell::Expected;
    }
    match (os, isa, nested) {
        ("linux", Isa::X86_64, Detected::Yes) => MatrixCell::Proven,
        ("linux", Isa::Arm64, Detected::No) => MatrixCell::Proven,
        ("macos", Isa::Arm64, Detected::No) => MatrixCell::Proven,
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
            assert_eq!(
                classify(Isa::X86_64, "macos", Detected::No, container),
                MatrixCell::Unsupported
            );
        }
    }

    #[test]
    fn arm64_nesting_answers_only_from_positive_signals() {
        assert_eq!(arm64_nesting(true, None), Detected::Yes);
        assert_eq!(
            arm64_nesting(true, Some("Raspberry Pi Foundation")),
            Detected::Yes
        );
        for vendor in ["QEMU", "Amazon EC2", "Microsoft Corporation", "KVM"] {
            assert_eq!(
                arm64_nesting(false, Some(vendor)),
                Detected::Yes,
                "{vendor}"
            );
        }
        for vendor in [
            "Raspberry Pi Foundation",
            "Ampere(R)",
            "Some Unlisted Hypervisor Inc.",
            "",
        ] {
            assert_eq!(
                arm64_nesting(false, Some(vendor)),
                Detected::Unknown,
                "{vendor}"
            );
        }
        assert_eq!(arm64_nesting(false, None), Detected::Unknown);
    }

    #[test]
    fn cpuinfo_nesting_reads_the_hypervisor_flag() {
        assert_eq!(
            cpuinfo_nesting("processor\t: 0\nflags\t\t: fpu vme hypervisor lm\n"),
            Detected::Yes
        );
        assert_eq!(
            cpuinfo_nesting("processor\t: 0\nflags\t\t: fpu vme lm\n"),
            Detected::No
        );
        assert_eq!(
            cpuinfo_nesting("processor\t: 0\nFeatures\t: fp asimd\n"),
            Detected::Unknown
        );
        assert_eq!(cpuinfo_nesting(""), Detected::Unknown);
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

#[cfg(test)]
mod acceptance_regressions {
    use super::*;

    #[test]
    fn sysctl_output_handles_status_whitespace_and_empty_values() {
        assert_eq!(sysctl_value(true, b" 17\n"), Some("17".into()));
        assert_eq!(sysctl_value(false, b"17"), None);
        assert_eq!(sysctl_value(true, b" \n"), None);
        let key = if cfg!(target_os = "macos") {
            "kern.ostype"
        } else {
            "kernel.ostype"
        };
        let expected = if cfg!(target_os = "macos") {
            "Darwin"
        } else {
            "Linux"
        };
        assert_eq!(sysctl(key).as_deref(), Some(expected));
        assert_eq!(sysctl("harmony.nonexistent.acceptance_key"), None);
    }

    #[test]
    fn container_dispatch_preserves_each_platform_answer() {
        for answer in [Detected::Yes, Detected::No, Detected::Unknown] {
            assert_eq!(detect_container_for("linux", || answer), answer);
        }
        assert_eq!(
            detect_container_for("macos", || panic!("Linux probe on macOS")),
            Detected::No
        );
        assert_eq!(
            detect_container_for("other", || panic!("Linux probe on unsupported OS")),
            Detected::Unknown
        );
        assert_eq!(
            detect_container(),
            detect_container_for(std::env::consts::OS, detect_container_linux)
        );
    }

    #[test]
    fn either_container_marker_is_sufficient_without_proc() {
        for marker in [".dockerenv", "run/.containerenv"] {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(root.path().join("run")).unwrap();
            assert_eq!(detect_container_linux_at(root.path()), Detected::Unknown);
            std::fs::write(root.path().join(marker), b"").unwrap();
            assert_eq!(detect_container_linux_at(root.path()), Detected::Yes);
        }
    }
}
