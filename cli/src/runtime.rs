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
    pub runtime_qualification: Option<RuntimeQualification>,
}
impl Default for Consonance {
    fn default() -> Self {
        Self {
            backend: Backend::Auto,
            ram_mib: 1024,
            kernel: None,
            base_initramfs: None,
            uml_profile: None,
            runtime_qualification: None,
        }
    }
}
use crate::host::Hypervisor;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};

pub const RELEASE: &str = match option_env!("HARMONY_RELEASE_VERSION") {
    Some(tag) => tag,
    None => concat!("v", env!("CARGO_PKG_VERSION")),
};
const SOURCE_DIGEST: &str = env!("HARMONY_PLATFORM_SOURCE_DIGEST");

#[derive(Deserialize)]
struct RuntimeManifest {
    version: u32,
    architecture: String,
    source_digest: String,
    files: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeQualification {
    kernel_sha256: String,
    base_initramfs_sha256: String,
}

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
    let installed = crate::host_boundary::installed_binary_hint()
        .as_deref()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(|p| p.join("share/harmony/guest").join(isa));
    let directories = [Some(local), installed, Some(guest.clone())]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let kernel_name = if isa == "aarch64" { "Image" } else { "bzImage" };
    select_existing_runtime(c, &directories, kernel_name)?;
    if c.base_initramfs.is_none() || (c.backend != Backend::Uml && c.kernel.is_none()) {
        let directory = acquire("guest", isa, offline)?;
        select_acquired_runtime(c, &directory, isa, kernel_name)?;
    }
    if c.backend == Backend::Uml {
        if c.uml_profile.is_none() {
            c.uml_profile = Some(acquire("uml", isa, offline)?.join(isa));
        }
        let profile = uml::Profile::load(c.uml_profile.as_ref().unwrap())?;
        c.kernel = Some(profile.directory().join(&profile.profile().executable.name));
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

fn select_acquired_runtime(
    c: &mut Consonance,
    directory: &Path,
    isa: &str,
    kernel_name: &str,
) -> Result<()> {
    let pair = directory.join(isa);
    let kernel = pair.join(kernel_name);
    let base = pair.join("initramfs-oci.cpio.gz");
    c.runtime_qualification = Some(verify_runtime_pair(&kernel, &base, kernel_name)?);
    c.base_initramfs.get_or_insert(base);
    if c.backend != Backend::Uml {
        c.kernel.get_or_insert(kernel);
    }
    Ok(())
}

fn select_existing_runtime(
    c: &mut Consonance,
    directories: &[PathBuf],
    kernel_name: &str,
) -> Result<()> {
    let was_qualified = c.runtime_qualification.take().is_some();
    if c.backend == Backend::Uml {
        if c.base_initramfs.is_none() {
            for directory in directories {
                let kernel = directory.join(kernel_name);
                let base = directory.join("initramfs-oci.cpio.gz");
                if let Ok(qualification) = verify_runtime_pair(&kernel, &base, kernel_name) {
                    c.base_initramfs = Some(base);
                    c.runtime_qualification = Some(qualification);
                    break;
                }
            }
        } else if was_qualified {
            let base = c.base_initramfs.as_ref().ok_or("missing base initramfs")?;
            let kernel = base.with_file_name(kernel_name);
            c.runtime_qualification = Some(verify_runtime_pair(&kernel, base, kernel_name)?);
        }
        return Ok(());
    }
    match (&c.kernel, &c.base_initramfs) {
        (Some(kernel), Some(base)) => {
            if was_qualified {
                c.runtime_qualification = Some(verify_runtime_pair(kernel, base, kernel_name)?);
            }
            return Ok(());
        }
        (Some(kernel), None) => {
            let base = kernel
                .parent()
                .ok_or("kernel override has no parent directory")?
                .join("initramfs-oci.cpio.gz");
            c.runtime_qualification = Some(verify_runtime_pair(kernel, &base, kernel_name)?);
            c.base_initramfs = Some(base);
            return Ok(());
        }
        (None, Some(base)) => {
            let kernel = base
                .parent()
                .ok_or("base_initramfs override has no parent directory")?
                .join(kernel_name);
            c.runtime_qualification = Some(verify_runtime_pair(&kernel, base, kernel_name)?);
            c.kernel = Some(kernel);
            return Ok(());
        }
        (None, None) => {}
    }
    for directory in directories {
        let kernel = directory.join(kernel_name);
        let base = directory.join("initramfs-oci.cpio.gz");
        if let Ok(qualification) = verify_runtime_pair(&kernel, &base, kernel_name) {
            c.kernel = Some(kernel);
            c.base_initramfs = Some(base);
            c.runtime_qualification = Some(qualification);
            break;
        }
    }
    Ok(())
}

fn paired_files(kernel: &Path, base: &Path) -> bool {
    if !kernel.is_file() || !base.is_file() {
        return false;
    }
    match (fs::canonicalize(kernel), fs::canonicalize(base)) {
        (Ok(kernel), Ok(base)) => kernel.parent() == base.parent(),
        _ => false,
    }
}

fn verify_runtime_pair(
    kernel: &Path,
    base: &Path,
    kernel_name: &str,
) -> Result<RuntimeQualification> {
    if !paired_files(kernel, base) {
        return Err(format!(
            "runtime kernel {} and base {} must be regular files in one directory",
            kernel.display(),
            base.display()
        )
        .into());
    }
    let directory = fs::canonicalize(kernel)?;
    let directory = directory
        .parent()
        .ok_or("runtime kernel has no directory")?;
    let path = directory.join("runtime-manifest.json");
    let bytes = fs::read(&path).map_err(|error| {
        format!(
            "runtime manifest {} is missing or unreadable: {error}; remove an outdated cached guest bundle or specify both custom artifact paths",
            path.display()
        )
    })?;
    let manifest: RuntimeManifest = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid runtime manifest {}: {error}", path.display()))?;
    let architecture = if kernel_name == "Image" {
        "aarch64"
    } else {
        "x86_64"
    };
    if manifest.version != 1
        || manifest.architecture != architecture
        || manifest.source_digest != SOURCE_DIGEST
    {
        return Err(format!(
            "runtime manifest {} does not match this CLI's platform source and architecture",
            path.display()
        )
        .into());
    }
    let kernel_sha256 = manifest
        .files
        .get(kernel_name)
        .ok_or_else(|| format!("runtime manifest {} omits {kernel_name}", path.display()))?
        .clone();
    let base_initramfs_sha256 = manifest
        .files
        .get("initramfs-oci.cpio.gz")
        .ok_or_else(|| {
            format!(
                "runtime manifest {} omits initramfs-oci.cpio.gz",
                path.display()
            )
        })?
        .clone();
    for (file, expected) in [(kernel, &kernel_sha256), (base, &base_initramfs_sha256)] {
        let mut reader = fs::File::open(file)?;
        let mut sha = Sha256::new();
        let mut chunk = [0_u8; 1024 * 1024];
        loop {
            let count = reader.read(&mut chunk)?;
            if count == 0 {
                break;
            }
            sha.update(&chunk[..count]);
        }
        if format!("{:x}", sha.finalize()) != *expected {
            return Err(format!(
                "runtime artifact {} does not match {}",
                file.display(),
                path.display()
            )
            .into());
        }
    }
    Ok(RuntimeQualification {
        kernel_sha256,
        base_initramfs_sha256,
    })
}

pub fn read_pair(c: &Consonance) -> Result<(Vec<u8>, Vec<u8>)> {
    let kernel_path = c.kernel.as_ref().ok_or("missing kernel")?;
    let base_path = c.base_initramfs.as_ref().ok_or("missing base initramfs")?;
    let kernel = fs::read(kernel_path)?;
    let base = fs::read(base_path)?;
    if let Some(qualification) = &c.runtime_qualification {
        if c.backend != Backend::Uml && digest(&kernel) != qualification.kernel_sha256 {
            return Err(format!(
                "runtime kernel {} changed after selection",
                kernel_path.display()
            )
            .into());
        }
        if digest(&base) != qualification.base_initramfs_sha256 {
            return Err(format!(
                "runtime base {} changed after selection",
                base_path.display()
            )
            .into());
        }
    }
    Ok((kernel, base))
}

pub fn acquire(kind: &str, isa: &str, offline: bool) -> Result<PathBuf> {
    let version = RELEASE;
    let destination = data_dir()?.join("runtime").join(version).join(kind);
    if destination.join(".complete").is_file() {
        return Ok(destination);
    }
    if offline {
        return Err(format!("missing {kind} artifacts for {isa}; run harmony check online or set explicit artifact paths").into());
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
    fn embedded_source_digest_matches_runtime_builder() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let output = Command::new("python3")
            .arg(root.join("consonance/harmony-linux/scripts/runtime-artifacts.py"))
            .args(["source-key", "--repo"])
            .arg(root)
            .args(["--architecture", "x86_64"])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().trim(),
            SOURCE_DIGEST
        );
    }

    fn write_pair(directory: &Path, kernel: &[u8], base: &[u8], source: &str, isa: &str) {
        fs::create_dir_all(directory).unwrap();
        fs::write(directory.join("bzImage"), kernel).unwrap();
        fs::write(directory.join("initramfs-oci.cpio.gz"), base).unwrap();
        let manifest = serde_json::json!({
            "version": 1,
            "architecture": isa,
            "source_digest": source,
            "files": {
                "bzImage": digest(kernel),
                "initramfs-oci.cpio.gz": digest(base),
                "fixture/index.json": digest(b"release manifest includes more files than its archive")
            }
        });
        fs::write(
            directory.join("runtime-manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn discovered_runtime_uses_complete_pair_from_one_directory() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("old");
        let partial = root.path().join("partial");
        let complete = root.path().join("complete");
        for directory in [&old, &partial, &complete] {
            fs::create_dir(directory).unwrap();
        }
        fs::write(old.join("bzImage"), b"old kernel").unwrap();
        fs::write(partial.join("initramfs-oci.cpio.gz"), b"new base").unwrap();
        write_pair(
            &complete,
            b"new kernel",
            b"new base",
            SOURCE_DIGEST,
            "x86_64",
        );
        let mut config = Consonance {
            backend: Backend::Kvm,
            ..Consonance::default()
        };

        select_existing_runtime(&mut config, &[old, partial, complete.clone()], "bzImage").unwrap();

        assert_eq!(config.kernel, Some(complete.join("bzImage")));
        assert_eq!(
            config.base_initramfs,
            Some(complete.join("initramfs-oci.cpio.gz"))
        );
    }

    #[test]
    fn partial_explicit_runtime_uses_its_sibling_artifact() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("old");
        let current = root.path().join("current");
        fs::create_dir(&old).unwrap();
        fs::create_dir(&current).unwrap();
        let old_kernel = old.join("bzImage");
        write_pair(&old, b"old kernel", b"old base", SOURCE_DIGEST, "x86_64");
        let old_base = old.join("initramfs-oci.cpio.gz");
        write_pair(
            &current,
            b"new kernel",
            b"new base",
            SOURCE_DIGEST,
            "x86_64",
        );
        let mut explicit_kernel = Consonance {
            backend: Backend::Kvm,
            kernel: Some(old_kernel.clone()),
            ..Consonance::default()
        };
        let mut explicit_base = Consonance {
            backend: Backend::Kvm,
            base_initramfs: Some(old_base.clone()),
            ..Consonance::default()
        };

        let candidates = [current];
        select_existing_runtime(&mut explicit_kernel, &candidates, "bzImage").unwrap();
        select_existing_runtime(&mut explicit_base, &candidates, "bzImage").unwrap();

        assert_eq!(explicit_kernel.base_initramfs, Some(old_base));
        assert_eq!(explicit_base.kernel, Some(old_kernel));
    }

    #[test]
    fn partial_explicit_runtime_without_sibling_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("old");
        let current = root.path().join("current");
        fs::create_dir(&old).unwrap();
        fs::create_dir(&current).unwrap();
        fs::write(old.join("bzImage"), b"old kernel").unwrap();
        fs::write(current.join("bzImage"), b"new kernel").unwrap();
        fs::write(current.join("initramfs-oci.cpio.gz"), b"new base").unwrap();
        let mut config = Consonance {
            backend: Backend::Kvm,
            kernel: Some(old.join("bzImage")),
            ..Consonance::default()
        };

        assert!(select_existing_runtime(&mut config, &[current], "bzImage").is_err());
        assert!(config.base_initramfs.is_none());
    }

    #[test]
    fn stale_complete_runtime_falls_back_to_current_pair() {
        let root = tempfile::tempdir().unwrap();
        let stale = root.path().join("stale");
        let current = root.path().join("current");
        write_pair(
            &stale,
            b"old kernel",
            b"old base",
            &"0".repeat(64),
            "x86_64",
        );
        write_pair(
            &current,
            b"new kernel",
            b"new base",
            SOURCE_DIGEST,
            "x86_64",
        );
        let mut config = Consonance {
            backend: Backend::Kvm,
            ..Consonance::default()
        };

        select_existing_runtime(&mut config, &[stale, current.clone()], "bzImage").unwrap();

        assert_eq!(config.kernel, Some(current.join("bzImage")));
        assert_eq!(
            config.base_initramfs,
            Some(current.join("initramfs-oci.cpio.gz"))
        );
    }

    #[test]
    fn missing_or_incompatible_manifest_falls_back_to_current_pair() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("missing");
        let wrong_arch = root.path().join("wrong_arch");
        let wrong_version = root.path().join("wrong_version");
        let current = root.path().join("current");
        write_pair(
            &missing,
            b"old kernel",
            b"old base",
            SOURCE_DIGEST,
            "x86_64",
        );
        fs::remove_file(missing.join("runtime-manifest.json")).unwrap();
        write_pair(
            &wrong_arch,
            b"old kernel",
            b"old base",
            SOURCE_DIGEST,
            "aarch64",
        );
        write_pair(
            &wrong_version,
            b"old kernel",
            b"old base",
            SOURCE_DIGEST,
            "x86_64",
        );
        let manifest_path = wrong_version.join("runtime-manifest.json");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest["version"] = 2.into();
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        write_pair(
            &current,
            b"new kernel",
            b"new base",
            SOURCE_DIGEST,
            "x86_64",
        );
        let mut config = Consonance {
            backend: Backend::Kvm,
            ..Consonance::default()
        };

        select_existing_runtime(
            &mut config,
            &[missing, wrong_arch, wrong_version, current.clone()],
            "bzImage",
        )
        .unwrap();

        assert_eq!(config.kernel, Some(current.join("bzImage")));
    }

    #[test]
    fn tampered_runtime_file_falls_back_to_current_pair() {
        for name in ["bzImage", "initramfs-oci.cpio.gz"] {
            let root = tempfile::tempdir().unwrap();
            let tampered = root.path().join("tampered");
            let current = root.path().join("current");
            write_pair(
                &tampered,
                b"old kernel",
                b"old base",
                SOURCE_DIGEST,
                "x86_64",
            );
            fs::write(tampered.join(name), b"other bytes").unwrap();
            write_pair(
                &current,
                b"new kernel",
                b"new base",
                SOURCE_DIGEST,
                "x86_64",
            );
            let mut config = Consonance {
                backend: Backend::Kvm,
                ..Consonance::default()
            };

            select_existing_runtime(&mut config, &[tampered, current.clone()], "bzImage").unwrap();

            assert_eq!(config.kernel, Some(current.join("bzImage")));
        }
    }

    #[test]
    fn single_explicit_artifact_requires_current_adjacent_pair() {
        let root = tempfile::tempdir().unwrap();
        write_pair(
            root.path(),
            b"old kernel",
            b"old base",
            &"0".repeat(64),
            "x86_64",
        );
        let mut kernel_only = Consonance {
            backend: Backend::Kvm,
            kernel: Some(root.path().join("bzImage")),
            ..Consonance::default()
        };
        let mut base_only = Consonance {
            backend: Backend::Kvm,
            base_initramfs: Some(root.path().join("initramfs-oci.cpio.gz")),
            ..Consonance::default()
        };
        let mut both_explicit = Consonance {
            backend: Backend::Kvm,
            kernel: kernel_only.kernel.clone(),
            base_initramfs: base_only.base_initramfs.clone(),
            ..Consonance::default()
        };

        assert!(select_existing_runtime(&mut kernel_only, &[], "bzImage").is_err());
        assert!(select_existing_runtime(&mut base_only, &[], "bzImage").is_err());
        assert!(select_existing_runtime(&mut both_explicit, &[], "bzImage").is_ok());
        assert!(both_explicit.runtime_qualification.is_none());
        assert_eq!(
            read_pair(&both_explicit).unwrap(),
            (b"old kernel".to_vec(), b"old base".to_vec())
        );
        let mut forged = both_explicit.clone();
        forged.runtime_qualification = Some(RuntimeQualification {
            kernel_sha256: digest(b"old kernel"),
            base_initramfs_sha256: digest(b"old base"),
        });
        fs::remove_file(root.path().join("runtime-manifest.json")).unwrap();
        assert!(select_existing_runtime(&mut forged, &[], "bzImage").is_err());
        assert!(forged.runtime_qualification.is_none());
    }

    #[test]
    fn uml_auto_base_skips_stale_pair() {
        let root = tempfile::tempdir().unwrap();
        let stale = root.path().join("stale");
        let current = root.path().join("current");
        write_pair(
            &stale,
            b"old kernel",
            b"old base",
            &"0".repeat(64),
            "x86_64",
        );
        write_pair(
            &current,
            b"new kernel",
            b"new base",
            SOURCE_DIGEST,
            "x86_64",
        );
        let mut config = Consonance {
            backend: Backend::Uml,
            ..Consonance::default()
        };

        select_existing_runtime(&mut config, &[stale, current.clone()], "bzImage").unwrap();

        assert_eq!(
            config.base_initramfs,
            Some(current.join("initramfs-oci.cpio.gz"))
        );
    }

    #[test]
    fn acquired_cached_guest_pair_requires_current_manifest() {
        let root = tempfile::tempdir().unwrap();
        let pair = root.path().join("x86_64");
        write_pair(&pair, b"kernel", b"base", SOURCE_DIGEST, "x86_64");
        fs::write(root.path().join(".complete"), b"release archive digest").unwrap();
        for source in [None, Some("0".repeat(64))] {
            write_pair(
                &pair,
                b"kernel",
                b"base",
                source.as_deref().unwrap_or(SOURCE_DIGEST),
                "x86_64",
            );
            if source.is_none() {
                fs::remove_file(pair.join("runtime-manifest.json")).unwrap();
            }
            let mut rejected = Consonance {
                backend: Backend::Kvm,
                ..Consonance::default()
            };
            let error = select_acquired_runtime(&mut rejected, root.path(), "x86_64", "bzImage")
                .unwrap_err()
                .to_string();
            assert!(error.contains("runtime manifest"), "{error}");
            if source.is_none() {
                assert!(error.contains("outdated cached guest bundle"), "{error}");
            }
            assert!(rejected.kernel.is_none());
            assert!(rejected.base_initramfs.is_none());
            assert!(rejected.runtime_qualification.is_none());
        }

        write_pair(&pair, b"kernel", b"base", SOURCE_DIGEST, "x86_64");
        let mut current = Consonance {
            backend: Backend::Kvm,
            ..Consonance::default()
        };
        select_acquired_runtime(&mut current, root.path(), "x86_64", "bzImage").unwrap();
        assert_eq!(current.kernel, Some(pair.join("bzImage")));
        assert_eq!(
            current.base_initramfs,
            Some(pair.join("initramfs-oci.cpio.gz"))
        );
        assert!(current.runtime_qualification.is_some());
    }

    #[test]
    fn selected_runtime_rejects_files_changed_before_consumption() {
        for name in ["bzImage", "initramfs-oci.cpio.gz"] {
            let root = tempfile::tempdir().unwrap();
            write_pair(root.path(), b"kernel", b"base", SOURCE_DIGEST, "x86_64");
            let mut config = Consonance {
                backend: Backend::Kvm,
                ..Consonance::default()
            };
            select_existing_runtime(&mut config, &[root.path().to_path_buf()], "bzImage").unwrap();
            assert!(config.runtime_qualification.is_some());
            let mut runner = crate::config::Runner::default();
            crate::runners::store_consonance(&mut runner, &config).unwrap();
            let config = crate::runners::consonance(&runner).unwrap();
            fs::write(root.path().join(name), b"changed after selection").unwrap();

            assert!(read_pair(&config).is_err(), "accepted changed {name}");
        }
    }

    #[test]
    fn qualified_pair_is_reverified_when_resolved_again() {
        let root = tempfile::tempdir().unwrap();
        write_pair(root.path(), b"kernel", b"base", SOURCE_DIGEST, "x86_64");
        let mut config = Consonance {
            backend: Backend::Kvm,
            ..Consonance::default()
        };
        select_existing_runtime(&mut config, &[root.path().to_path_buf()], "bzImage").unwrap();
        assert!(config.runtime_qualification.is_some());
        select_existing_runtime(&mut config, &[], "bzImage").unwrap();
        assert!(config.runtime_qualification.is_some());
        fs::write(root.path().join("bzImage"), b"changed kernel").unwrap();
        assert!(select_existing_runtime(&mut config, &[], "bzImage").is_err());
        assert!(config.runtime_qualification.is_none());
    }

    #[test]
    fn uml_auto_base_keeps_its_hash_after_profile_kernel_selection() {
        let root = tempfile::tempdir().unwrap();
        write_pair(
            root.path(),
            b"guest kernel",
            b"base",
            SOURCE_DIGEST,
            "x86_64",
        );
        let profile = root.path().join("linux");
        fs::write(&profile, b"profile kernel").unwrap();
        let mut config = Consonance {
            backend: Backend::Uml,
            ..Consonance::default()
        };
        select_existing_runtime(&mut config, &[root.path().to_path_buf()], "bzImage").unwrap();
        config.kernel = Some(profile);
        select_existing_runtime(&mut config, &[], "bzImage").unwrap();
        assert!(config.runtime_qualification.is_some());
        assert_eq!(
            read_pair(&config).unwrap(),
            (b"profile kernel".to_vec(), b"base".to_vec())
        );
        fs::write(root.path().join("initramfs-oci.cpio.gz"), b"changed base").unwrap();
        assert!(read_pair(&config).is_err());
    }
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
