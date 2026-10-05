// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::config::{Backend, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Consonance {
    pub backend: Backend,
    pub ram_mib: u32,
    pub kernel: Option<PathBuf>,
    pub base_initramfs: Option<PathBuf>,
    pub uml_profile: Option<PathBuf>,
}
impl Default for Consonance {
    fn default() -> Self {
        Self {
            backend: Backend::Auto,
            ram_mib: 1024,
            kernel: None,
            base_initramfs: None,
            uml_profile: None,
        }
    }
}
use crate::host::Hypervisor;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

pub const RELEASE: &str = match option_env!("HARMONY_RELEASE_VERSION") {
    Some(tag) => tag,
    None => concat!("v", env!("CARGO_PKG_VERSION")),
};

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn data_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("HARMONY_DATA_DIR") {
        return Ok(path.into());
    }
    if let Some(path) = std::env::var_os("XDG_DATA_HOME") {
        return Ok(PathBuf::from(path).join("harmony"));
    }
    let home = std::env::var_os("HOME").ok_or("set HARMONY_DATA_DIR when HOME is unavailable")?;
    Ok(PathBuf::from(home).join(if cfg!(target_os = "macos") {
        "Library/Application Support/Harmony"
    } else {
        ".local/share/harmony"
    }))
}

pub fn choose(c: &Consonance) -> Result<Backend> {
    choose_for(c, &Hypervisor::detect(), cfg!(target_os = "linux"))
}

fn choose_for(c: &Consonance, host: &Hypervisor, linux: bool) -> Result<Backend> {
    let selected = if c.backend != Backend::Auto {
        c.backend
    } else if c.uml_profile.is_some() {
        Backend::Uml
    } else {
        match host {
            Hypervisor::Kvm => Backend::Kvm,
            Hypervisor::Hvf => Backend::Hvf,
            _ if linux => Backend::Uml,
            _ => return Err("no compatible execution backend is available on this host".into()),
        }
    };
    match selected {
        Backend::Kvm if matches!(host, Hypervisor::Kvm) => Ok(selected),
        Backend::Hvf if matches!(host, Hypervisor::Hvf) => Ok(selected),
        Backend::Uml if linux => Ok(selected),
        _ => Err(format!("backend {selected:?} is unavailable for this input on this host").into()),
    }
}

pub fn resolve(c: &mut Consonance, offline: bool) -> Result<()> {
    c.backend = choose(c)?;
    eprintln!("backend: {:?}", c.backend);
    let isa = std::env::consts::ARCH;
    let root = data_dir()?.join("runtime").join(RELEASE);
    let guest = root.join("guest").join(isa);
    let local = PathBuf::from("consonance/harmony-linux/build").join(isa);
    let installed = std::env::current_exe()?
        .parent()
        .and_then(Path::parent)
        .map(|p| p.join("share/harmony/guest").join(isa));
    for directory in [Some(local), installed, Some(guest.clone())]
        .into_iter()
        .flatten()
    {
        if c.kernel.is_none() && c.backend != Backend::Uml {
            c.kernel = ["bzImage", "Image"]
                .iter()
                .map(|name| directory.join(name))
                .find(|p| p.is_file());
        }
        if c.base_initramfs.is_none() {
            let p = directory.join("initramfs-oci.cpio.gz");
            if p.is_file() {
                c.base_initramfs = Some(p);
            }
        }
    }
    if c.base_initramfs.is_none() || (c.backend != Backend::Uml && c.kernel.is_none()) {
        let directory = acquire("guest", isa, offline)?;
        c.base_initramfs
            .get_or_insert(directory.join(isa).join("initramfs-oci.cpio.gz"));
        if c.backend != Backend::Uml {
            c.kernel
                .get_or_insert(directory.join(isa).join(if isa == "aarch64" {
                    "Image"
                } else {
                    "bzImage"
                }));
        }
    }
    if c.backend == Backend::Uml {
        if c.uml_profile.is_none() {
            c.uml_profile = Some(acquire("uml", isa, offline)?.join(isa));
        }
        c.kernel = Some(uml::Profile::load(c.uml_profile.as_ref().unwrap())?.executable());
    }
    for path in [&mut c.kernel, &mut c.base_initramfs, &mut c.uml_profile]
        .into_iter()
        .flatten()
    {
        *path = fs::canonicalize(&*path)
            .map_err(|e| format!("runtime artifact {}: {e}", path.display()))?;
    }
    Ok(())
}

pub fn acquire(kind: &str, isa: &str, offline: bool) -> Result<PathBuf> {
    let version = RELEASE;
    let destination = data_dir()?.join("runtime").join(version).join(kind);
    if destination.join(".complete").is_file() {
        return Ok(destination);
    }
    if offline {
        return Err(format!("missing {kind} artifacts for {isa}; run harmony doctor online or set explicit artifact paths").into());
    }
    let parent = destination
        .parent()
        .ok_or("runtime directory has no parent")?;
    fs::create_dir_all(parent)?;
    let staging = tempfile::tempdir_in(parent)?;
    let asset = format!("harmony-{kind}-{version}-{isa}.tar.gz");
    let base = format!("https://github.com/pH14/harmony/releases/download/{version}");
    let archive = staging.path().join(&asset);
    fetch(&format!("{base}/{asset}"), &archive)?;
    let checksum = staging.path().join("checksum");
    fetch(&format!("{base}/{asset}.sha256"), &checksum)?;
    let expected = fs::read_to_string(checksum)?
        .split_whitespace()
        .next()
        .ok_or("empty release checksum")?
        .to_owned();
    let unpacked = staging.path().join("unpacked");
    unpack_verified(&archive, &expected, &unpacked)?;
    fs::write(unpacked.join(".complete"), &expected)?;
    match fs::rename(&unpacked, &destination) {
        Ok(()) => {}
        Err(_) if destination.join(".complete").is_file() => {}
        Err(error) => return Err(error.into()),
    }
    Ok(destination)
}

fn unpack_verified(archive: &Path, expected: &str, destination: &Path) -> Result<()> {
    let bytes = fs::read(archive)?;
    if digest(&bytes) != expected {
        return Err("release artifact checksum mismatch".into());
    }
    fs::create_dir_all(destination)?;
    let decoder = flate2::read::GzDecoder::new(bytes.as_slice());
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let kind = entry.header().entry_type();
        if !kind.is_file() && !kind.is_dir() {
            return Err("runtime archives may contain only files and directories".into());
        }
        if !entry.unpack_in(destination)? {
            return Err("runtime archive contains an escaping path".into());
        }
    }
    Ok(())
}

fn fetch(url: &str, path: &Path) -> Result<()> {
    eprintln!("fetching {url}");
    let status = Command::new("curl")
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--connect-timeout",
            "20",
            "--max-time",
            "600",
            "--output",
        ])
        .arg(path)
        .arg(url)
        .status()?;
    if !status.success() {
        return Err(format!(
            "could not fetch {url}; supply local runtime paths for an unreleased build"
        )
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_uses_input_and_actual_host_capabilities() {
        let mut c = Consonance::default();
        let absent = Hypervisor::Unavailable("no device".into());
        assert_eq!(choose_for(&c, &absent, true).unwrap(), Backend::Uml);
        assert_eq!(
            choose_for(&c, &Hypervisor::Kvm, true).unwrap(),
            Backend::Kvm
        );
        assert_eq!(
            choose_for(&c, &Hypervisor::Hvf, false).unwrap(),
            Backend::Hvf
        );
        c.backend = Backend::Hvf;
        assert!(choose_for(&c, &absent, true).is_err());
    }
    #[test]
    fn verified_release_files_keep_executable_permissions() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("runtime.tar.gz");
        let gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut tar = tar::Builder::new(gzip);
        let mut header = tar::Header::new_gnu();
        header.set_size(7);
        header.set_mode(0o755);
        header.set_cksum();
        tar.append_data(&mut header, "aarch64/linux", &b"runtime"[..])
            .unwrap();
        let bytes = tar.into_inner().unwrap().finish().unwrap();
        fs::File::create(&archive)
            .unwrap()
            .write_all(&bytes)
            .unwrap();
        let out = dir.path().join("unpacked");
        unpack_verified(&archive, &digest(&bytes), &out).unwrap();
        assert_eq!(fs::read(out.join("aarch64/linux")).unwrap(), b"runtime");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(
                fs::metadata(out.join("aarch64/linux"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o111,
                0
            );
        }
    }

    #[test]
    fn a_wrong_release_checksum_never_unpacks() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("archive");
        fs::write(&archive, b"bad archive").unwrap();
        let out = dir.path().join("out");
        assert!(unpack_verified(&archive, &"0".repeat(64), &out).is_err());
        assert!(!out.exists());
    }
}
