// SPDX-License-Identifier: AGPL-3.0-or-later

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const SCHEMA: u32 = 1;
const PT_INTERP: u32 = 3;

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
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostIdentity {
    pub architecture: String,
    pub cpu_model: String,
}

#[derive(Clone, Debug)]
pub struct VerifiedProfile {
    pub directory: PathBuf,
    pub profile: Profile,
    pub identity_sha256: String,
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
}

impl Profile {
    pub fn load(directory: &Path) -> Result<VerifiedProfile, ProfileError> {
        let path = directory.join("profile.json");
        let text = std::fs::read(&path).map_err(|source| ProfileError::Read {
            path: path.clone(),
            source,
        })?;
        let profile: Profile = serde_json::from_slice(&text)?;
        profile.verify(directory)?;
        let identity_sha256 = hex(&Sha256::digest(serde_json::to_vec(&profile)?));
        Ok(VerifiedProfile {
            directory: directory.to_path_buf(),
            profile,
            identity_sha256,
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

impl VerifiedProfile {
    pub fn executable(&self) -> PathBuf {
        self.directory.join(&self.profile.executable.name)
    }

    pub fn rootfs(&self) -> PathBuf {
        self.directory.join(&self.profile.rootfs.name)
    }
}

impl HostIdentity {
    pub fn current() -> io::Result<Self> {
        let cpuinfo = std::fs::read_to_string("/proc/cpuinfo")?;
        Ok(Self {
            architecture: std::env::consts::ARCH.to_owned(),
            cpu_model: cpu_model(&cpuinfo),
        })
    }
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

fn cpu_model(cpuinfo: &str) -> String {
    let first = cpuinfo.split("\n\n").next().unwrap_or_default();
    let keys: &[&str] = match std::env::consts::ARCH {
        "aarch64" => &[
            "CPU implementer",
            "CPU architecture",
            "CPU variant",
            "CPU part",
            "CPU revision",
        ],
        _ => &["vendor_id", "cpu family", "model", "stepping", "model name"],
    };
    keys.iter()
        .filter_map(|key| {
            first.lines().find_map(|line| {
                let (name, value) = line.split_once(':')?;
                (name.trim() == *key).then(|| format!("{key}={}", value.trim()))
            })
        })
        .collect::<Vec<_>>()
        .join(";")
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

    fn write_profile(directory: &Path, edit: impl FnOnce(&mut Profile)) {
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
        };
        edit(&mut profile);
        std::fs::write(
            directory.join("profile.json"),
            serde_json::to_vec_pretty(&profile).unwrap(),
        )
        .unwrap();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn verified_profile_rejects_changed_bytes_and_modes() {
        let directory = tempfile::tempdir().unwrap();
        write_profile(directory.path(), |_| {});
        let verified = Profile::load(directory.path()).unwrap();
        assert_eq!(verified.identity_sha256.len(), 64);

        std::fs::write(directory.path().join("config"), b"CONFIG_UML=n\n").unwrap();
        assert!(matches!(
            Profile::load(directory.path()),
            Err(ProfileError::Digest { name, .. }) if name == "config"
        ));

        write_profile(directory.path(), |profile| {
            profile.userspace = "ptrace".to_owned()
        });
        assert!(matches!(
            Profile::load(directory.path()),
            Err(ProfileError::Userspace(_))
        ));

        write_profile(directory.path(), |profile| {
            profile.rootfs.name = "../initramfs.cpio.gz".to_owned()
        });
        assert!(matches!(
            Profile::load(directory.path()),
            Err(ProfileError::ArtifactName(_))
        ));

        write_profile(directory.path(), |profile| {
            profile.host_libraries.push(Artifact {
                name: "libc.so.6".to_owned(),
                sha256: "0".repeat(64),
            })
        });
        assert!(matches!(
            Profile::load(directory.path()),
            Err(ProfileError::HostLibraries(1))
        ));
    }

    #[test]
    fn cpu_model_reads_the_first_processor() {
        let text = "processor\t: 0\nvendor_id\t: GenuineIntel\ncpu family\t: 6\nmodel\t\t: 183\nmodel name\t: Example CPU\nstepping\t: 1\nmicrocode\t: 0x12b\nCPU implementer\t: 0x61\nCPU part\t: 0x022\n\nprocessor\t: 1\nmodel\t\t: 1\n";
        let model = cpu_model(text);
        if std::env::consts::ARCH == "aarch64" {
            assert_eq!(model, "CPU implementer=0x61;CPU part=0x022");
        } else {
            assert_eq!(
                model,
                "vendor_id=GenuineIntel;cpu family=6;model=183;stepping=1;model name=Example CPU"
            );
        }
    }
}
