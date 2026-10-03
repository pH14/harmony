// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::Serialize;

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
