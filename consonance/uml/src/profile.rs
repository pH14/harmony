// SPDX-License-Identifier: AGPL-3.0-or-later

use std::fs::File;
#[cfg(target_os = "linux")]
use std::io::Write;
use std::io::{self, Read};
#[cfg(target_os = "linux")]
use std::mem::size_of;
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
#[cfg(target_os = "linux")]
use std::os::unix::fs::FileExt;
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::Command;
#[cfg(target_os = "linux")]
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const SCHEMA: u32 = 4;
const PT_INTERP: u32 = 3;
#[cfg(target_os = "linux")]
const KERNEL_FD: i32 = 240;
#[cfg(target_os = "linux")]
const ROOTFS_FD: i32 = 241;
#[cfg(target_os = "linux")]
const INITRAMFS_FD: i32 = 242;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub name: String,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub schema: u32,
    pub architecture: String,
    pub kernel_version: String,
    pub userspace: String,
    pub patch_series_sha256: String,
    pub host_libraries: Vec<Artifact>,
    pub executable: Artifact,
    pub config: Artifact,
    pub rootfs: Artifact,
    pub virtual_time: VirtualTimeCosts,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VirtualTimeCosts {
    pub syscall_vns: u64,
    pub clock_read_vns: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostIdentity {
    pub architecture: String,
    pub processor: usize,
    pub cpu_model: String,
    pub cpu_features: String,
    pub boot_cpuinfo: String,
    pub auxv: HostAuxv,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostAuxv {
    pub hwcap: u64,
    pub hwcap2: u64,
    pub minsigstksz: u64,
    pub platform: String,
}

#[derive(Clone, Debug)]
pub struct VerifiedProfile {
    directory: PathBuf,
    profile: Profile,
    identity_sha256: String,
    #[cfg(target_os = "linux")]
    pinned: Option<Arc<PinnedArtifacts>>,
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct PinnedArtifacts {
    executable: PinnedImage,
    _config: PinnedImage,
    rootfs: PinnedImage,
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
pub(crate) struct PinnedImage {
    fd: OwnedFd,
}

#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("cannot read {path}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("profile.json is malformed: {0}")]
    Malformed(#[from] serde_json::Error),
    #[error("profile schema {0} is unsupported")]
    Schema(u32),
    #[error("profile userspace mode {0:?} is unsupported; Harmony requires seccomp")]
    Userspace(String),
    #[error("profile architecture {profile} does not match host architecture {host}")]
    Architecture { profile: String, host: String },
    #[error("artifact name {0:?} must be a plain file name")]
    ArtifactName(String),
    #[error("{name} sha256 is {actual}, profile records {expected}")]
    Digest {
        name: String,
        expected: String,
        actual: String,
    },
    #[error("the executable must be statically linked; profile lists {0} host libraries")]
    HostLibraries(usize),
    #[error("{0} is not a static 64-bit little-endian ELF executable")]
    NotStatic(String),
    #[error("no reviewed UML profile exists for host architecture {0}")]
    UnsupportedArchitecture(String),
    #[error("the built-in reviewed UML profile is malformed: {0}")]
    ReviewedCatalog(serde_json::Error),
    #[error("profile does not match the reviewed {0} UML bundle")]
    UnreviewedProfile(String),
    #[error("cannot pin profile artifact {name}: {source}")]
    Pin { name: String, source: io::Error },
}

impl Profile {
    pub fn load(directory: &Path) -> Result<VerifiedProfile, ProfileError> {
        let reviewed = reviewed_profile()?;
        Self::load_with_reviewed(directory, &reviewed)
    }

    fn load_with_reviewed(
        directory: &Path,
        reviewed: &Profile,
    ) -> Result<VerifiedProfile, ProfileError> {
        let path = directory.join("profile.json");
        let text = std::fs::read(&path).map_err(|source| ProfileError::Read {
            path: path.clone(),
            source,
        })?;
        let profile: Profile = serde_json::from_slice(&text)?;
        profile.verify(directory)?;
        if &profile != reviewed {
            return Err(ProfileError::UnreviewedProfile(
                profile.architecture.clone(),
            ));
        }
        let identity_sha256 = hex(&Sha256::digest(serde_json::to_vec(&profile)?));
        #[cfg(target_os = "linux")]
        let pinned = Some(Arc::new(PinnedArtifacts::new(directory, &profile)?));
        Ok(VerifiedProfile {
            directory: directory.to_path_buf(),
            profile,
            identity_sha256,
            #[cfg(target_os = "linux")]
            pinned,
        })
    }

    fn verify(&self, directory: &Path) -> Result<(), ProfileError> {
        if self.schema != SCHEMA {
            return Err(ProfileError::Schema(self.schema));
        }
        if self.userspace != "seccomp" {
            return Err(ProfileError::Userspace(self.userspace.clone()));
        }
        if self.architecture != std::env::consts::ARCH {
            return Err(ProfileError::Architecture {
                profile: self.architecture.clone(),
                host: std::env::consts::ARCH.to_owned(),
            });
        }
        if !self.host_libraries.is_empty() {
            return Err(ProfileError::HostLibraries(self.host_libraries.len()));
        }
        for artifact in [&self.executable, &self.config, &self.rootfs] {
            let path = artifact_path(directory, artifact)?;
            let actual = sha256_file(&path).map_err(|source| ProfileError::Read {
                path: path.clone(),
                source,
            })?;
            if actual != artifact.sha256 {
                return Err(ProfileError::Digest {
                    name: artifact.name.clone(),
                    expected: artifact.sha256.clone(),
                    actual,
                });
            }
        }
        let executable = artifact_path(directory, &self.executable)?;
        let bytes = std::fs::read(&executable).map_err(|source| ProfileError::Read {
            path: executable.clone(),
            source,
        })?;
        if !is_static_elf64(&bytes) {
            return Err(ProfileError::NotStatic(self.executable.name.clone()));
        }
        Ok(())
    }
}

fn reviewed_profile() -> Result<Profile, ProfileError> {
    let catalog = match std::env::consts::ARCH {
        "x86_64" => include_str!("../reviewed-profiles/x86_64.json"),
        "aarch64" => include_str!("../reviewed-profiles/aarch64.json"),
        other => return Err(ProfileError::UnsupportedArchitecture(other.to_owned())),
    };
    serde_json::from_str(catalog).map_err(ProfileError::ReviewedCatalog)
}

impl VerifiedProfile {
    #[cfg(test)]
    pub(crate) fn for_test(directory: PathBuf, profile: Profile, identity_sha256: String) -> Self {
        Self {
            directory,
            profile,
            identity_sha256,
            #[cfg(target_os = "linux")]
            pinned: None,
        }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    pub fn identity_sha256(&self) -> &str {
        &self.identity_sha256
    }

    pub(crate) fn executable(&self) -> PathBuf {
        #[cfg(target_os = "linux")]
        if self.pinned.is_some() {
            return child_path(KERNEL_FD);
        }
        self.directory.join(&self.profile.executable.name)
    }

    pub(crate) fn rootfs(&self) -> PathBuf {
        #[cfg(target_os = "linux")]
        if self.pinned.is_some() {
            return child_path(ROOTFS_FD);
        }
        self.directory.join(&self.profile.rootfs.name)
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn pass_artifacts(&self, command: &mut Command) {
        let Some(pinned) = self.pinned.clone() else {
            return;
        };
        // SAFETY: the closure only calls dup2(2) and fcntl(2) on descriptors
        // held by the captured artifacts. Process-lifetime reservations keep
        // the fixed targets unavailable to Command's spawn error pipe. The
        // sources stay open until the command is dropped and cannot equal
        // the fixed targets, which are reserved before pinning.
        unsafe {
            command.pre_exec(move || {
                for (source, target) in [
                    (pinned.executable.fd.as_raw_fd(), KERNEL_FD),
                    (pinned.rootfs.fd.as_raw_fd(), ROOTFS_FD),
                ] {
                    if libc::dup2(source, target) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    let flags = libc::fcntl(target, libc::F_GETFD);
                    if flags < 0 || libc::fcntl(target, libc::F_SETFD, inherited_flags(flags)) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
    }
}

#[cfg(target_os = "linux")]
impl PinnedArtifacts {
    fn new(directory: &Path, profile: &Profile) -> Result<Self, ProfileError> {
        reserved_child_fds().map_err(|source| ProfileError::Pin {
            name: "launcher descriptors".to_owned(),
            source,
        })?;
        Ok(Self {
            executable: pin_artifact(directory, &profile.executable, c"harmony-uml-kernel", true)?,
            _config: pin_artifact(directory, &profile.config, c"harmony-uml-config", false)?,
            rootfs: pin_artifact(directory, &profile.rootfs, c"harmony-uml-rootfs", false)?,
        })
    }
}

#[cfg(target_os = "linux")]
impl PinnedImage {
    pub(crate) fn from_path(
        path: &Path,
        name: &std::ffi::CStr,
        executable: bool,
    ) -> io::Result<Self> {
        reserved_child_fds()?;
        let (fd, _) = pin_path(path, name, executable)?;
        Ok(Self { fd })
    }

    pub(crate) fn path(&self) -> PathBuf {
        child_path(INITRAMFS_FD)
    }

    fn parent_path(&self) -> PathBuf {
        child_path(self.fd.as_raw_fd())
    }

    pub(crate) fn bytes(&self) -> io::Result<Vec<u8>> {
        std::fs::read(self.parent_path())
    }

    pub(crate) fn pass(self: &Arc<Self>, command: &mut Command) {
        let image = Arc::clone(self);
        // SAFETY: the closure owns an Arc to the sealed source until the
        // command is dropped and calls only dup2(2) and fcntl(2) on live
        // descriptors in the child before exec.
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(image.fd.as_raw_fd(), INITRAMFS_FD) < 0 {
                    return Err(io::Error::last_os_error());
                }
                let flags = libc::fcntl(INITRAMFS_FD, libc::F_GETFD);
                if flags < 0 || libc::fcntl(INITRAMFS_FD, libc::F_SETFD, inherited_flags(flags)) < 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
}

#[cfg(target_os = "linux")]
fn inherited_flags(flags: i32) -> i32 {
    flags & !libc::FD_CLOEXEC
}

#[cfg(target_os = "linux")]
fn child_path(fd: i32) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{fd}"))
}

#[cfg(target_os = "linux")]
fn reserved_child_fds() -> io::Result<&'static [OwnedFd; 3]> {
    static RESERVED: OnceLock<Result<[OwnedFd; 3], String>> = OnceLock::new();
    RESERVED
        .get_or_init(|| {
            let source = File::open("/dev/null").map_err(|error| error.to_string())?;
            let mut reserved = Vec::new();
            for target in [KERNEL_FD, ROOTFS_FD, INITRAMFS_FD] {
                // SAFETY: fcntl(2) duplicates a live source descriptor, and
                // from_raw_fd takes ownership only after a successful call.
                let duplicate =
                    unsafe { libc::fcntl(source.as_raw_fd(), libc::F_DUPFD_CLOEXEC, target) };
                if duplicate < 0 {
                    return Err(io::Error::last_os_error().to_string());
                }
                // SAFETY: F_DUPFD_CLOEXEC returned a fresh descriptor owned
                // only by this function.
                let owned = unsafe { OwnedFd::from_raw_fd(duplicate) };
                if duplicate != target {
                    return Err(format!("launcher descriptor {target} is already in use"));
                }
                reserved.push(owned);
            }
            reserved
                .try_into()
                .map_err(|_| "launcher descriptor reservation is incomplete".to_owned())
        })
        .as_ref()
        .map_err(|error| io::Error::other(error.clone()))
}

#[cfg(target_os = "linux")]
fn pin_artifact(
    directory: &Path,
    artifact: &Artifact,
    name: &std::ffi::CStr,
    executable: bool,
) -> Result<PinnedImage, ProfileError> {
    let path = artifact_path(directory, artifact)?;
    let (fd, actual) = pin_path(&path, name, executable).map_err(|source| ProfileError::Pin {
        name: artifact.name.clone(),
        source,
    })?;
    if actual != artifact.sha256 {
        return Err(ProfileError::Digest {
            name: artifact.name.clone(),
            expected: artifact.sha256.clone(),
            actual,
        });
    }
    Ok(PinnedImage { fd })
}

#[cfg(target_os = "linux")]
fn pin_path(path: &Path, name: &std::ffi::CStr, executable: bool) -> io::Result<(OwnedFd, String)> {
    let mut source = File::open(path)?;
    let mut flags = libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING;
    if executable {
        flags |= libc::MFD_EXEC;
    }
    let raw = loop {
        // SAFETY: `name` is NUL-terminated and a successful call transfers a
        // fresh descriptor to this function.
        let raw = unsafe { libc::memfd_create(name.as_ptr(), flags) };
        if raw >= 0 {
            break raw;
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EINVAL) && flags & libc::MFD_EXEC != 0 {
            flags &= !libc::MFD_EXEC;
            continue;
        }
        return Err(error);
    };
    // SAFETY: memfd_create succeeded and transferred a fresh descriptor.
    let mut pinned = unsafe { File::from_raw_fd(raw) };
    let mut digest = Sha256::new();
    let mut block = [0_u8; 1 << 16];
    loop {
        let read = source.read(&mut block)?;
        if read == 0 {
            break;
        }
        digest.update(&block[..read]);
        pinned.write_all(&block[..read])?;
    }
    let actual = hex(&digest.finalize());
    let seals = libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
    // SAFETY: fcntl(2) only updates the fresh memfd's seals and descriptor
    // flags; the owned File keeps it open for both operations.
    let duplicate = unsafe {
        if libc::fcntl(pinned.as_raw_fd(), libc::F_ADD_SEALS, seals) < 0 {
            return Err(io::Error::last_os_error());
        }
        libc::fcntl(pinned.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 10)
    };
    if duplicate < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: F_DUPFD_CLOEXEC returned a new descriptor owned by this function.
    Ok((unsafe { OwnedFd::from_raw_fd(duplicate) }, actual))
}

impl HostIdentity {
    pub fn identity_sha256(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(b"harmony-uml-host-v2\0");
        digest.update((self.processor as u64).to_le_bytes());
        for field in [
            &self.architecture,
            &self.cpu_model,
            &self.cpu_features,
            &self.boot_cpuinfo,
            &self.auxv.platform,
        ] {
            digest.update((field.len() as u64).to_le_bytes());
            digest.update(field.as_bytes());
        }
        for value in [self.auxv.hwcap, self.auxv.hwcap2, self.auxv.minsigstksz] {
            digest.update(value.to_le_bytes());
        }
        hex(&digest.finalize())
    }

    pub fn current() -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        let selected = Some(selected_processor()?);
        #[cfg(not(target_os = "linux"))]
        let selected = None;
        let cpuinfo = std::fs::read_to_string("/proc/cpuinfo")?;
        let mut host = Self::from_cpuinfo(&cpuinfo, selected)?;
        host.auxv = HostAuxv::current()?;
        Ok(host)
    }

    #[cfg(target_os = "linux")]
    pub fn for_processor(processor: usize) -> io::Result<Self> {
        let mut host =
            Self::from_cpuinfo(&std::fs::read_to_string("/proc/cpuinfo")?, Some(processor))?;
        host.auxv = HostAuxv::current()?;
        Ok(host)
    }

    fn from_cpuinfo(cpuinfo: &str, selected: Option<usize>) -> io::Result<Self> {
        let index = |block: &str| {
            block.lines().find_map(|line| {
                let (name, value) = line.split_once(':')?;
                (name.trim() == "processor")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
        };
        let first = cpuinfo
            .split("\n\n")
            .find(|block| selected.is_none_or(|cpu| index(block) == Some(cpu)))
            .ok_or_else(|| io::Error::other("selected processor is absent from /proc/cpuinfo"))?;
        let (model_keys, features_key): (&[&str], &str) = match std::env::consts::ARCH {
            "aarch64" => (
                &[
                    "CPU implementer",
                    "CPU architecture",
                    "CPU variant",
                    "CPU part",
                    "CPU revision",
                ],
                "Features",
            ),
            _ => (
                &["vendor_id", "cpu family", "model", "stepping", "model name"],
                "flags",
            ),
        };
        let value = |key: &str| {
            first.lines().find_map(|line| {
                let (name, value) = line.split_once(':')?;
                (name.trim() == key).then(|| value.trim().to_owned())
            })
        };
        let processor = index(first)
            .ok_or_else(|| io::Error::other("/proc/cpuinfo processor has no valid index"))?;
        Ok(Self {
            architecture: std::env::consts::ARCH.to_owned(),
            processor,
            cpu_model: model_keys
                .iter()
                .filter_map(|key| value(key).map(|value| format!("{key}={value}")))
                .collect::<Vec<_>>()
                .join(";"),
            cpu_features: value(features_key).unwrap_or_default(),
            boot_cpuinfo: boot_cpuinfo(cpuinfo),
            auxv: HostAuxv::default(),
        })
    }
}

impl HostAuxv {
    #[cfg(target_os = "linux")]
    fn current() -> io::Result<Self> {
        const WORD: usize = size_of::<usize>();
        const ENTRY: usize = 2 * WORD;
        let bytes = std::fs::read("/proc/self/auxv")?;
        if bytes.len() % ENTRY != 0 {
            return Err(io::Error::other(
                "host auxiliary vector has a partial entry",
            ));
        }
        let mut hwcap = None;
        let mut hwcap2 = None;
        let mut minsigstksz = None;
        let mut platform = None;
        let mut terminated = false;
        for entry in bytes.chunks_exact(ENTRY) {
            let kind = usize::from_ne_bytes(entry[..WORD].try_into().unwrap());
            let value = usize::from_ne_bytes(entry[WORD..].try_into().unwrap());
            if kind == 0 {
                terminated = true;
                break;
            }
            let slot = match kind {
                16 => &mut hwcap,
                26 => &mut hwcap2,
                51 => &mut minsigstksz,
                15 => &mut platform,
                _ => continue,
            };
            if slot.replace(value).is_some() {
                return Err(io::Error::other(
                    "host auxiliary vector repeats an imported key",
                ));
            }
        }
        if !terminated {
            return Err(io::Error::other("host auxiliary vector has no terminator"));
        }
        let platform = platform
            .filter(|address| *address != 0)
            .ok_or_else(|| io::Error::other("host auxiliary vector has no platform"))?;
        let memory = File::open("/proc/self/mem")?;
        let mut bytes = [0u8; 64];
        let mut length = 0;
        while length < bytes.len() {
            let address = platform
                .checked_add(length)
                .ok_or_else(|| io::Error::other("host auxiliary platform address overflows"))?;
            let read = memory.read_at(
                &mut bytes[length..],
                u64::try_from(address).map_err(io::Error::other)?,
            )?;
            if read == 0 {
                return Err(io::Error::other("host auxiliary platform is unreadable"));
            }
            if let Some(end) = bytes[length..length + read]
                .iter()
                .position(|byte| *byte == 0)
            {
                length += end;
                let platform =
                    String::from_utf8(bytes[..length].to_vec()).map_err(io::Error::other)?;
                return Ok(Self {
                    hwcap: hwcap.unwrap_or(0) as u64,
                    hwcap2: hwcap2.unwrap_or(0) as u64,
                    minsigstksz: minsigstksz.unwrap_or(0) as u64,
                    platform,
                });
            }
            length += read;
        }
        Err(io::Error::other("host auxiliary platform exceeds 63 bytes"))
    }

    #[cfg(not(target_os = "linux"))]
    fn current() -> io::Result<Self> {
        Ok(Self::default())
    }
}

fn boot_cpuinfo(cpuinfo: &str) -> String {
    let mut imported = String::new();
    #[cfg(target_arch = "x86_64")]
    let mut matches = 0;
    for line in cpuinfo.split_inclusive('\n') {
        #[cfg(target_arch = "x86_64")]
        {
            let flags = line.contains("flags");
            let cache = line.contains("cache_alignment");
            if flags || cache {
                imported.push_str(line);
                matches += usize::from(flags) + usize::from(cache);
                if matches > 1 {
                    break;
                }
            }
        }
        #[cfg(target_arch = "aarch64")]
        {
            if line.contains("cache_alignment") {
                imported.push_str(line);
            } else {
                break;
            }
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            let _ = line;
        }
    }
    imported
}

#[cfg(target_os = "linux")]
pub(crate) fn selected_processor() -> io::Result<usize> {
    let mask = allowed_processors()?;
    (0..libc::CPU_SETSIZE as usize)
        .find(|cpu| {
            // SAFETY: cpu is below CPU_SETSIZE and mask is initialized.
            unsafe { libc::CPU_ISSET(*cpu, &mask) }
        })
        .ok_or_else(|| io::Error::other("no CPU is allowed by this thread's affinity mask"))
}

#[cfg(target_os = "linux")]
pub(crate) fn processor_allowed(processor: usize) -> io::Result<bool> {
    if processor >= libc::CPU_SETSIZE as usize {
        return Ok(false);
    }
    let mask = allowed_processors()?;
    // SAFETY: processor is below CPU_SETSIZE and mask is initialized.
    Ok(unsafe { libc::CPU_ISSET(processor, &mask) })
}

#[cfg(target_os = "linux")]
fn allowed_processors() -> io::Result<libc::cpu_set_t> {
    // SAFETY: cpu_set_t is plain data and zero is its empty set. The kernel
    // writes no more than the provided size into this live object.
    let mut mask: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    // SAFETY: mask is writable for exactly the supplied cpu_set_t size.
    if unsafe { libc::sched_getaffinity(0, size_of::<libc::cpu_set_t>(), &mut mask) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(mask)
}

fn artifact_path(directory: &Path, artifact: &Artifact) -> Result<PathBuf, ProfileError> {
    let name = Path::new(&artifact.name);
    if artifact.name.is_empty()
        || name.components().count() != 1
        || name.file_name().is_none_or(|file| file != name.as_os_str())
    {
        return Err(ProfileError::ArtifactName(artifact.name.clone()));
    }
    Ok(directory.join(name))
}

fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut block = vec![0_u8; 1 << 20];
    loop {
        let read = file.read(&mut block)?;
        if read == 0 {
            return Ok(hex(&digest.finalize()));
        }
        digest.update(&block[..read]);
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn field<const N: usize>(bytes: &[u8], offset: usize) -> Option<[u8; N]> {
    bytes.get(offset..offset.checked_add(N)?)?.try_into().ok()
}

fn is_static_elf64(bytes: &[u8]) -> bool {
    if bytes.get(..4) != Some(b"\x7fELF".as_slice())
        || bytes.get(4) != Some(&2)
        || bytes.get(5) != Some(&1)
    {
        return false;
    }
    let (Some(phoff), Some(phentsize), Some(phnum)) = (
        field::<8>(bytes, 0x20).map(u64::from_le_bytes),
        field::<2>(bytes, 0x36).map(u16::from_le_bytes),
        field::<2>(bytes, 0x38).map(u16::from_le_bytes),
    ) else {
        return false;
    };
    let Ok(phoff) = usize::try_from(phoff) else {
        return false;
    };
    if phentsize < 4 {
        return false;
    }
    (0..usize::from(phnum)).all(|index| {
        index
            .checked_mul(usize::from(phentsize))
            .and_then(|offset| offset.checked_add(phoff))
            .and_then(|offset| field::<4>(bytes, offset))
            .is_some_and(|kind| u32::from_le_bytes(kind) != PT_INTERP)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn elf(program_types: &[u32]) -> Vec<u8> {
        let mut bytes = vec![0_u8; 64 + 56 * program_types.len()];
        bytes[..4].copy_from_slice(b"\x7fELF");
        bytes[4] = 2;
        bytes[5] = 1;
        bytes[0x20..0x28].copy_from_slice(&64_u64.to_le_bytes());
        bytes[0x36..0x38].copy_from_slice(&56_u16.to_le_bytes());
        bytes[0x38..0x3a].copy_from_slice(&(program_types.len() as u16).to_le_bytes());
        for (index, kind) in program_types.iter().enumerate() {
            let offset = 64 + 56 * index;
            bytes[offset..offset + 4].copy_from_slice(&kind.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn static_elf_has_no_interpreter() {
        assert!(is_static_elf64(&elf(&[1, 1, 4])));
        assert!(!is_static_elf64(&elf(&[6, PT_INTERP, 1])));
        assert!(!is_static_elf64(b"\x7fELF"));
        let mut truncated = elf(&[1, 1]);
        truncated.truncate(100);
        assert!(!is_static_elf64(&truncated));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn child_flags_only_clear_close_on_exec() {
        assert_eq!(inherited_flags(libc::FD_CLOEXEC), 0);
        assert_eq!(inherited_flags(libc::FD_CLOEXEC | 0x40), 0x40);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn pinned_child_paths_use_separate_reserved_descriptors() {
        assert_eq!(child_path(KERNEL_FD), PathBuf::from("/proc/self/fd/240"));
        assert_eq!(child_path(ROOTFS_FD), PathBuf::from("/proc/self/fd/241"));
        assert_eq!(child_path(INITRAMFS_FD), PathBuf::from("/proc/self/fd/242"));
        const { assert!(KERNEL_FD > 4 && KERNEL_FD < ROOTFS_FD && ROOTFS_FD < INITRAMFS_FD) };
    }

    fn write_profile(directory: &Path, edit: impl FnOnce(&mut Profile)) -> Profile {
        std::fs::write(directory.join("linux"), elf(&[1])).unwrap();
        std::fs::write(directory.join("config"), b"CONFIG_UML=y\n").unwrap();
        std::fs::write(directory.join("initramfs.cpio.gz"), b"rootfs").unwrap();
        let artifact = |name: &str| Artifact {
            name: name.to_owned(),
            sha256: sha256_file(&directory.join(name)).unwrap(),
        };
        let mut profile = Profile {
            schema: SCHEMA,
            architecture: std::env::consts::ARCH.to_owned(),
            kernel_version: "6.18.35".to_owned(),
            userspace: "seccomp".to_owned(),
            patch_series_sha256: "0".repeat(64),
            host_libraries: Vec::new(),
            executable: artifact("linux"),
            config: artifact("config"),
            rootfs: artifact("initramfs.cpio.gz"),
            virtual_time: VirtualTimeCosts {
                syscall_vns: 100_000,
                clock_read_vns: 1,
            },
        };
        edit(&mut profile);
        std::fs::write(
            directory.join("profile.json"),
            serde_json::to_vec_pretty(&profile).unwrap(),
        )
        .unwrap();
        profile
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn production_loader_refuses_self_consistent_unreviewed_bundle() {
        let directory = tempfile::tempdir().unwrap();
        write_profile(directory.path(), |_| {});
        assert!(matches!(
            Profile::load(directory.path()),
            Err(ProfileError::UnreviewedProfile(_))
        ));
    }

    #[test]
    #[ignore = "requires a CI-built UML profile"]
    fn reviewed_ci_bundle_admits_only_its_approved_bytes() {
        let source = PathBuf::from(std::env::var_os("HARMONY_UML_REVIEWED_PROFILE").unwrap());
        let approved = reviewed_profile().unwrap();
        let verified = Profile::load(&source).unwrap();
        assert_eq!(verified.profile(), &approved);

        for name in ["linux", "config", "initramfs.cpio.gz"] {
            let directory = tempfile::tempdir().unwrap();
            for artifact in [&approved.executable, &approved.config, &approved.rootfs] {
                std::fs::copy(
                    source.join(&artifact.name),
                    directory.path().join(&artifact.name),
                )
                .unwrap();
            }
            let mut changed = approved.clone();
            let path = directory.path().join(name);
            let mut bytes = std::fs::read(&path).unwrap();
            bytes.push(0);
            std::fs::write(&path, bytes).unwrap();
            let artifact = match name {
                "linux" => &mut changed.executable,
                "config" => &mut changed.config,
                _ => &mut changed.rootfs,
            };
            artifact.sha256 = sha256_file(&path).unwrap();
            std::fs::write(
                directory.path().join("profile.json"),
                serde_json::to_vec_pretty(&changed).unwrap(),
            )
            .unwrap();
            assert!(matches!(
                Profile::load(directory.path()),
                Err(ProfileError::UnreviewedProfile(_))
            ));
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn reviewed_profile_rejects_self_consistent_artifact_and_metadata_edits() {
        let directory = tempfile::tempdir().unwrap();
        let reviewed = write_profile(directory.path(), |_| {});
        for name in ["linux", "config", "initramfs.cpio.gz"] {
            write_profile(directory.path(), |_| {});
            let path = directory.path().join(name);
            let mut bytes = std::fs::read(&path).unwrap();
            bytes.push(0);
            std::fs::write(&path, bytes).unwrap();
            let mut changed = reviewed.clone();
            let artifact = match name {
                "linux" => &mut changed.executable,
                "config" => &mut changed.config,
                _ => &mut changed.rootfs,
            };
            artifact.sha256 = sha256_file(&path).unwrap();
            std::fs::write(
                directory.path().join("profile.json"),
                serde_json::to_vec_pretty(&changed).unwrap(),
            )
            .unwrap();
            assert!(matches!(
                Profile::load_with_reviewed(directory.path(), &reviewed),
                Err(ProfileError::UnreviewedProfile(_))
            ));
        }

        for changed in [
            Profile {
                kernel_version: "7.2-rc4".to_owned(),
                ..reviewed.clone()
            },
            Profile {
                patch_series_sha256: "1".repeat(64),
                ..reviewed.clone()
            },
            Profile {
                virtual_time: VirtualTimeCosts {
                    syscall_vns: 100_001,
                    ..reviewed.virtual_time
                },
                ..reviewed.clone()
            },
        ] {
            write_profile(directory.path(), |_| {});
            std::fs::write(
                directory.path().join("profile.json"),
                serde_json::to_vec_pretty(&changed).unwrap(),
            )
            .unwrap();
            assert!(matches!(
                Profile::load_with_reviewed(directory.path(), &reviewed),
                Err(ProfileError::UnreviewedProfile(_))
            ));
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn verified_profile_rejects_changed_bytes_and_modes() {
        let directory = tempfile::tempdir().unwrap();
        let reviewed = write_profile(directory.path(), |_| {});
        let verified = Profile::load_with_reviewed(directory.path(), &reviewed).unwrap();
        assert_eq!(verified.identity_sha256.len(), 64);

        write_profile(directory.path(), |profile| profile.schema = 2);
        assert!(matches!(
            Profile::load_with_reviewed(directory.path(), &reviewed),
            Err(ProfileError::Schema(2))
        ));
        write_profile(directory.path(), |_| {});

        std::fs::write(directory.path().join("config"), b"CONFIG_UML=n\n").unwrap();
        assert!(matches!(
            Profile::load_with_reviewed(directory.path(), &reviewed),
            Err(ProfileError::Digest { name, .. }) if name == "config"
        ));

        write_profile(directory.path(), |profile| {
            profile.userspace = "ptrace".to_owned()
        });
        assert!(matches!(
            Profile::load_with_reviewed(directory.path(), &reviewed),
            Err(ProfileError::Userspace(_))
        ));

        write_profile(directory.path(), |profile| {
            profile.rootfs.name = "../initramfs.cpio.gz".to_owned()
        });
        assert!(matches!(
            Profile::load_with_reviewed(directory.path(), &reviewed),
            Err(ProfileError::ArtifactName(_))
        ));

        write_profile(directory.path(), |profile| {
            profile.host_libraries.push(Artifact {
                name: "libc.so.6".to_owned(),
                sha256: "0".repeat(64),
            })
        });
        assert!(matches!(
            Profile::load_with_reviewed(directory.path(), &reviewed),
            Err(ProfileError::HostLibraries(1))
        ));
    }

    #[test]
    #[cfg(all(target_os = "linux", not(miri)))]
    fn verified_profile_keeps_the_bytes_it_verified() {
        let directory = tempfile::tempdir().unwrap();
        let reviewed = write_profile(directory.path(), |_| {});
        let verified = Profile::load_with_reviewed(directory.path(), &reviewed).unwrap();
        let original = verified.pinned.as_ref().unwrap().rootfs.bytes().unwrap();

        std::fs::write(directory.path().join("initramfs.cpio.gz"), b"changed").unwrap();
        assert!(matches!(
            Profile::load_with_reviewed(directory.path(), &reviewed),
            Err(ProfileError::Digest { name, .. }) if name == "initramfs.cpio.gz"
        ));
        assert_eq!(
            verified.pinned.as_ref().unwrap().rootfs.bytes().unwrap(),
            original
        );
    }

    #[test]
    #[cfg(all(target_os = "linux", not(miri)))]
    fn command_keeps_pinned_profile_alive_until_spawn() {
        let directory = tempfile::tempdir().unwrap();
        let reviewed = write_profile(directory.path(), |_| {});
        let verified = Profile::load_with_reviewed(directory.path(), &reviewed).unwrap();
        let rootfs = verified.rootfs();
        let parent_path = verified.pinned.as_ref().unwrap().rootfs.parent_path();
        let mut command = Command::new("cat");
        command.arg(&rootfs);
        verified.pass_artifacts(&mut command);
        drop(verified);
        assert_eq!(std::fs::read(&parent_path).unwrap(), b"rootfs");
        assert_eq!(command.output().unwrap().stdout, b"rootfs");
        drop(command);
        assert!(std::fs::read(&parent_path).is_err());
    }

    #[test]
    #[cfg(all(target_os = "linux", not(miri)))]
    fn pinned_override_keeps_the_bytes_it_copied() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("initramfs");
        std::fs::write(&path, b"original").unwrap();
        let image = Arc::new(PinnedImage::from_path(&path, c"override-test", false).unwrap());
        let pinned_path = image.path();
        let parent_path = image.parent_path();
        std::fs::write(&path, b"changed").unwrap();
        assert_eq!(image.bytes().unwrap(), b"original");
        let mut command = Command::new("cat");
        command.arg(&pinned_path);
        image.pass(&mut command);
        drop(image);
        assert_eq!(command.output().unwrap().stdout, b"original");
        drop(command);
        assert!(std::fs::read(&parent_path).is_err());
    }

    #[test]
    fn host_identity_reads_the_first_processor() {
        let text = "processor\t: 0\nvendor_id\t: GenuineIntel\ncpu family\t: 6\nmodel\t\t: 183\nmodel name\t: Example CPU\nstepping\t: 1\nmicrocode\t: 0x12b\nflags\t\t: fpu tsc avx2\nCPU implementer\t: 0x61\nCPU part\t: 0x022\nFeatures\t: fp asimd\n\nprocessor\t: 1\nmodel\t\t: 1\nflags\t\t: fpu\n";
        let host = HostIdentity::from_cpuinfo(text, None).unwrap();
        assert_eq!(host.processor, 0);
        if std::env::consts::ARCH == "aarch64" {
            assert_eq!(host.cpu_model, "CPU implementer=0x61;CPU part=0x022");
            assert_eq!(host.cpu_features, "fp asimd");
        } else {
            assert_eq!(
                host.cpu_model,
                "vendor_id=GenuineIntel;cpu family=6;model=183;stepping=1;model name=Example CPU"
            );
            assert_eq!(host.cpu_features, "fpu tsc avx2");
        }
    }

    #[test]
    fn host_identity_refuses_a_missing_processor_index() {
        assert!(HostIdentity::from_cpuinfo("Features: fp asimd\n", None).is_err());
    }

    #[test]
    fn host_identity_selects_the_requested_processor() {
        let text = "processor: 0\nCPU part: 0xd81\nFeatures: fp\n\nprocessor: 2\nCPU part: 0xd80\nFeatures: fp\n";
        let first = HostIdentity::from_cpuinfo(text, Some(0)).unwrap();
        let second = HostIdentity::from_cpuinfo(text, Some(2)).unwrap();
        assert_eq!(first.processor, 0);
        assert_eq!(second.processor, 2);
        if std::env::consts::ARCH == "aarch64" {
            assert_ne!(first.cpu_model, second.cpu_model);
        }
        assert!(HostIdentity::from_cpuinfo(text, Some(1)).is_err());
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn host_identity_tracks_uml_boot_cpuinfo_ingress() {
        let base =
            "processor: 0\nflags: fpu tsc\n\nprocessor: 1\nflags: fpu tsc\nmodel name: CPU\n";
        let other_flags = "processor: 0\nflags: fpu tsc avx512f\n\nprocessor: 1\nflags: fpu tsc\nmodel name: CPU\n";
        let other_cache = "processor: 0\nflags: fpu tsc\ncache_alignment: 128\n\nprocessor: 1\nflags: fpu tsc\nmodel name: CPU\n";
        let admitted = HostIdentity::from_cpuinfo(base, Some(1)).unwrap();
        assert_ne!(
            admitted,
            HostIdentity::from_cpuinfo(other_flags, Some(1)).unwrap()
        );
        assert_ne!(
            admitted,
            HostIdentity::from_cpuinfo(other_cache, Some(1)).unwrap()
        );
        let irrelevant = base.replace("model name: CPU", "model name: CPU\ncpu MHz: 1000");
        assert_eq!(
            admitted,
            HostIdentity::from_cpuinfo(&irrelevant, Some(1)).unwrap()
        );
        let late_cache = base.replace("model name: CPU", "model name: CPU\ncache_alignment: 128");
        assert_eq!(
            admitted,
            HostIdentity::from_cpuinfo(&late_cache, Some(1)).unwrap()
        );
    }

    #[test]
    #[cfg(target_arch = "aarch64")]
    fn host_identity_tracks_arm_uml_boot_cache_alignment() {
        let base = "cache_alignment: 64\nprocessor: 0\nFeatures: fp asimd\n";
        let changed = "cache_alignment: 128\nprocessor: 0\nFeatures: fp asimd\n";
        assert_ne!(
            HostIdentity::from_cpuinfo(base, Some(0)).unwrap(),
            HostIdentity::from_cpuinfo(changed, Some(0)).unwrap()
        );
    }

    #[test]
    fn host_identity_binds_imported_auxiliary_vector() {
        let mut base = HostIdentity::from_cpuinfo("processor: 0\nflags: fpu\n", Some(0)).unwrap();
        let admitted = base.identity_sha256();
        base.auxv.hwcap ^= 8;
        assert_ne!(base.identity_sha256(), admitted);
        base.auxv.hwcap ^= 8;
        base.auxv.hwcap2 ^= 8;
        assert_ne!(base.identity_sha256(), admitted);
        base.auxv.hwcap2 ^= 8;
        base.auxv.minsigstksz += 1;
        assert_ne!(base.identity_sha256(), admitted);
        base.auxv.minsigstksz -= 1;
        base.auxv.platform.push('x');
        assert_ne!(base.identity_sha256(), admitted);
    }
}
