// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::config::Result;
use std::path::PathBuf;

pub fn strict_identity() -> Result<String> {
    #[cfg(target_os = "linux")]
    {
        Ok(host_code_identity::sealed_static_executable_identity()?)
    }
    #[cfg(target_os = "macos")]
    {
        Ok(host_code_identity::strict_macos_executable_identity()?)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err("Consonance requires a supported immutable host runtime".into())
    }
}

pub fn require_strict_host() -> Result<()> {
    strict_identity().map(|_| ())
}

pub fn admit_execution(kind: &str) -> Result<()> {
    if kind == "consonance" {
        return Err("Consonance execution requires the packaged strict host".into());
    }
    Ok(())
}

pub fn installed_binary_hint() -> Option<PathBuf> {
    std::env::current_exe().ok()
}
