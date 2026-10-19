// SPDX-License-Identifier: AGPL-3.0-or-later

use super::cpio::{CpioError, Writer};
use super::exact;
use super::image::{ImageError, Ownership, StagedImage, with_host_read_access};
use execution_proto::{ExecutionSpec, SpecError, VERSION};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::path::{Component, Path, PathBuf};

pub const CONTROL_ROOT: &str = "/harmony-oci";
pub const ROOTFS_ROOT: &str = "/harmony-oci/rootfs";
pub const EXECUTION_SOURCE: &str = "/harmony-oci/execution.json";
pub const EXECUTION_DESTINATION: &str = execution_proto::EXECUTION_PATH;
pub const SUPERVISOR_PATH: &str = "/usr/lib/harmony/supervisor";
pub const HARMONY_DEVICE: &str = "/dev/harmony";
const KERNEL_LOG_DEVICE: &str = "/dev/kmsg";
const KVM_DEVICE: &str = "/dev/kvm";
const GUEST_PATH_MAX: usize = 4096;
const GUEST_NAME_MAX: usize = 255;
pub const MAX_EXTERNAL_INPUTS: usize = 256;
pub const MAX_EXTERNAL_INPUT_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_EXTERNAL_INPUT_PATH_BYTES: usize = 4096;

#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    #[error(transparent)]
    Cpio(#[from] CpioError),
    #[error(transparent)]
    Image(#[from] ImageError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Spec(#[from] SpecError),
    #[error("external input count {count} exceeds {limit}")]
    ExternalInputCount { count: usize, limit: usize },
    #[error("external input bytes {bytes} exceeds {limit}")]
    ExternalInputBytes { bytes: usize, limit: usize },
    #[error("external input destination {path:?} is not valid: {reason}")]
    ExternalInputPath { path: String, reason: &'static str },
    #[error("external input destination {path:?} conflicts with {other:?}")]
    ExternalInputConflict { path: String, other: String },
    #[error("external input destination is not UTF-8")]
    ExternalInputUtf8,
    #[error("mount destination {path:?} crosses an image symlink")]
    MountSymlink { path: String },
    #[error("prepared rootfs archive is malformed: {0}")]
    RootfsArchive(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalInput {
    pub destination: PathBuf,
    pub data: Vec<u8>,
}

impl ExternalInput {
    pub fn new(destination: impl Into<PathBuf>, data: impl Into<Vec<u8>>) -> Self {
        Self {
            destination: destination.into(),
            data: data.into(),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LaunchRequest {
    pub command: Vec<String>,
    pub bundle: Option<String>,
    pub external_inputs: Vec<ExternalInput>,
    pub kvm: bool,
}

impl LaunchRequest {
    pub fn new(command: Vec<String>) -> Self {
        Self {
            command,
            ..Self::default()
        }
    }

    pub fn with_bundle(mut self, bundle: impl Into<String>) -> Self {
        self.bundle = Some(bundle.into());
        self
    }

    pub fn with_external_inputs(mut self, external_inputs: Vec<ExternalInput>) -> Self {
        self.external_inputs = external_inputs;
        self
    }

    pub fn with_kvm(mut self) -> Self {
        self.kvm = true;
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedExecution {
    rootfs_segment: Vec<u8>,
    control_segment: Vec<u8>,
    execution: ExecutionSpec,
    identity: [u8; 32],
    external_inputs: Vec<ExternalInput>,
    kvm: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedExecutionRecord {
    pub version: u32,
    pub rootfs_segment: Vec<u8>,
    pub execution: ExecutionSpec,
    pub external_inputs: Vec<ExternalInput>,
    pub kvm: bool,
    pub identity: [u8; 32],
}

pub struct ArchivedRootfs {
    bytes: Vec<u8>,
    entries: Vec<ArchivedRootfsIndex>,
}

struct ArchivedRootfsIndex {
    path: String,
    mode: u32,
    data: Range<usize>,
}

pub struct ArchivedRootfsEntry<'a> {
    pub path: &'a str,
    pub mode: u32,
    pub data: &'a [u8],
}

impl ArchivedRootfs {
    pub fn entries(&self) -> impl Iterator<Item = ArchivedRootfsEntry<'_>> {
        self.entries.iter().map(|entry| ArchivedRootfsEntry {
            path: &entry.path,
            mode: entry.mode,
            data: &self.bytes[entry.data.clone()],
        })
    }
}

impl PreparedExecution {
    pub fn external_inputs(&self) -> &[ExternalInput] {
        &self.external_inputs
    }

    pub fn exposes_kvm(&self) -> bool {
        self.kvm
    }

    pub fn record(&self) -> PreparedExecutionRecord {
        PreparedExecutionRecord {
            version: 1,
            rootfs_segment: self.rootfs_segment.clone(),
            execution: self.execution.clone(),
            external_inputs: self.external_inputs.clone(),
            kvm: self.kvm,
            identity: self.identity,
        }
    }

    pub fn from_record(record: PreparedExecutionRecord) -> Result<Self, BundleError> {
        if record.version != 1 {
            return Err(BundleError::RootfsArchive(
                "unsupported prepared execution version".into(),
            ));
        }
        record.execution.validate()?;
        if !record
            .execution
            .env
            .iter()
            .any(|entry| entry == "LD_BIND_NOW=1")
            || !record
                .execution
                .env
                .iter()
                .any(|entry| entry.starts_with("PATH="))
        {
            return Err(BundleError::RootfsArchive(
                "prepared environment lacks canonical binding policy".into(),
            ));
        }
        let inputs = validate_external_inputs(&record.external_inputs)?;
        let control_segment =
            build_control_segment_validated(&record.execution, &inputs, record.kvm)?;
        let identity =
            execution_identity(&record.rootfs_segment, &control_segment, &record.execution)?;
        if identity != record.identity {
            return Err(BundleError::RootfsArchive(
                "prepared execution identity differs".into(),
            ));
        }
        let prepared = Self {
            rootfs_segment: record.rootfs_segment,
            control_segment,
            execution: record.execution,
            identity,
            external_inputs: record.external_inputs,
            kvm: record.kvm,
        };
        let archive = prepared.archived_rootfs()?;
        for path in [
            "/proc",
            "/dev",
            "/dev/pts",
            "/dev/shm",
            "/sys",
            "/sys/fs/cgroup",
            "/run",
            "/tmp",
            SUPERVISOR_PATH,
            EXECUTION_DESTINATION,
            HARMONY_DEVICE,
            KERNEL_LOG_DEVICE,
        ]
        .into_iter()
        .chain(inputs.iter().map(|input| input.destination.as_str()))
        .chain(record.kvm.then_some(KVM_DEVICE))
        {
            for ancestor in Path::new(path).ancestors().skip(1) {
                let relative = ancestor.strip_prefix("/").unwrap().to_string_lossy();
                if archive
                    .entries()
                    .any(|entry| entry.path == relative && entry.mode & 0o170000 == 0o120000)
                {
                    return Err(BundleError::MountSymlink {
                        path: path.to_owned(),
                    });
                }
            }
            let relative = path.trim_start_matches('/');
            if archive
                .entries()
                .any(|entry| entry.path == relative && entry.mode & 0o170000 == 0o120000)
            {
                return Err(BundleError::MountSymlink {
                    path: path.to_owned(),
                });
            }
        }
        Ok(prepared)
    }

    pub fn rootfs_segment(&self) -> &[u8] {
        &self.rootfs_segment
    }

    pub fn control_segment(&self) -> &[u8] {
        &self.control_segment
    }

    pub fn execution(&self) -> &ExecutionSpec {
        &self.execution
    }

    pub fn identity(&self) -> &[u8; 32] {
        &self.identity
    }

    pub fn archived_rootfs(&self) -> Result<ArchivedRootfs, BundleError> {
        let segment = &self.rootfs_segment;
        if segment.len() < 18 || segment[..10] != [0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 4, 255] {
            return Err(BundleError::RootfsArchive("gzip header".into()));
        }
        let bytes = inflate_rootfs(&segment[10..segment.len() - 8], 1 << 30)?;
        let trailer = &segment[segment.len() - 8..];
        if trailer[..4] != crc32fast::hash(&bytes).to_le_bytes()
            || trailer[4..] != (bytes.len() as u32).to_le_bytes()
        {
            return Err(BundleError::RootfsArchive("gzip checksum".into()));
        }
        parse_rootfs_archive(bytes)
    }

    #[must_use]
    pub fn initramfs(&self, base: &[u8]) -> Vec<u8> {
        let mut image = Vec::with_capacity(
            base.len()
                .saturating_add(self.rootfs_segment.len())
                .saturating_add(self.control_segment.len()),
        );
        image.extend_from_slice(base);
        image.extend_from_slice(&self.rootfs_segment);
        image.extend_from_slice(&self.control_segment);
        image
    }

    #[must_use]
    pub fn identity_hex(&self) -> String {
        self.identity
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

fn inflate_rootfs(mut input: &[u8], limit: usize) -> Result<Vec<u8>, BundleError> {
    use flate2::{Decompress, FlushDecompress, Status};
    let malformed = |reason: &str| BundleError::RootfsArchive(reason.into());
    let mut decoder = Decompress::new(false);
    let mut bytes = Vec::new();
    let mut chunk = [0; 64 << 10];
    loop {
        let before_in = decoder.total_in();
        let before_out = decoder.total_out();
        let status = decoder
            .decompress(input, &mut chunk, FlushDecompress::None)
            .map_err(|_| malformed("deflate stream"))?;
        let consumed = usize::try_from(decoder.total_in() - before_in)
            .map_err(|_| malformed("deflate input length"))?;
        let produced = usize::try_from(decoder.total_out() - before_out)
            .map_err(|_| malformed("deflate output length"))?;
        if produced > limit.saturating_sub(bytes.len()) {
            return Err(malformed("deflate output limit"));
        }
        bytes.extend_from_slice(&chunk[..produced]);
        input = &input[consumed..];
        if status == Status::StreamEnd {
            return if input.is_empty() {
                Ok(bytes)
            } else {
                Err(malformed("deflate trailing input"))
            };
        }
        if consumed == 0 && produced == 0 {
            return Err(malformed("incomplete deflate stream"));
        }
    }
}

fn parse_rootfs_archive(bytes: Vec<u8>) -> Result<ArchivedRootfs, BundleError> {
    let malformed = |reason: &str| BundleError::RootfsArchive(reason.into());
    let mut cursor = 0usize;
    let mut entries = Vec::new();
    let mut names = BTreeMap::new();
    let mut root_entries = 0;
    let mut next_inode = 1_u32;
    loop {
        let header = bytes
            .get(cursor..cursor.saturating_add(110))
            .ok_or_else(|| malformed("truncated cpio header"))?;
        if &header[..6] != b"070701" {
            return Err(malformed("cpio header magic"));
        }
        let field = |index: usize| -> Result<u32, BundleError> {
            let start = 6 + index * 8;
            let encoded = &header[start..start + 8];
            if encoded
                .iter()
                .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(byte))
            {
                return Err(malformed("noncanonical cpio header field"));
            }
            let text = std::str::from_utf8(encoded).map_err(|_| malformed("cpio header field"))?;
            u32::from_str_radix(text, 16).map_err(|_| malformed("cpio header field"))
        };
        if field(0)? != next_inode
            || field(4)? != 1
            || [5, 7, 8, 9, 10, 12]
                .into_iter()
                .any(|index| !matches!(field(index), Ok(0)))
        {
            return Err(malformed("noncanonical cpio inode or link header"));
        }
        next_inode = next_inode
            .checked_add(1)
            .ok_or_else(|| malformed("cpio inode overflow"))?;
        let mode = field(1)?;
        let uid = field(2)?;
        let gid = field(3)?;
        let filesize = usize::try_from(field(6)?).map_err(|_| malformed("cpio file size"))?;
        let namesize = usize::try_from(field(11)?).map_err(|_| malformed("cpio name size"))?;
        if namesize == 0 || namesize > GUEST_PATH_MAX {
            return Err(malformed("cpio name exceeds guest path limits"));
        }
        let name_start = cursor
            .checked_add(110)
            .ok_or_else(|| malformed("cpio offset overflow"))?;
        let name_end = name_start
            .checked_add(namesize)
            .ok_or_else(|| malformed("cpio offset overflow"))?;
        let name = bytes
            .get(name_start..name_end)
            .ok_or_else(|| malformed("truncated cpio name"))?;
        if name.last() != Some(&0) {
            return Err(malformed("unterminated cpio name"));
        }
        let name = std::str::from_utf8(&name[..name.len() - 1])
            .map_err(|_| malformed("non-UTF-8 cpio name"))?;
        let data_start = name_end
            .checked_add(3)
            .ok_or_else(|| malformed("cpio offset overflow"))?
            & !3;
        let data_end = data_start
            .checked_add(filesize)
            .ok_or_else(|| malformed("cpio offset overflow"))?;
        let data = bytes
            .get(data_start..data_end)
            .ok_or_else(|| malformed("truncated cpio data"))?;
        if bytes[name_end..data_start].iter().any(|byte| *byte != 0) {
            return Err(malformed("noncanonical cpio name padding"));
        }
        cursor = data_end
            .checked_add(3)
            .ok_or_else(|| malformed("cpio offset overflow"))?
            & !3;
        if cursor > bytes.len() {
            return Err(malformed("truncated cpio padding"));
        }
        if bytes[data_end..cursor].iter().any(|byte| *byte != 0) {
            return Err(malformed("noncanonical cpio data padding"));
        }
        if name == "TRAILER!!!" {
            if mode != 0
                || uid != 0
                || gid != 0
                || filesize != 0
                || cursor != bytes.len()
                || root_entries != 2
            {
                return Err(malformed("cpio trailer"));
            }
            break;
        }
        if name == "harmony-oci" || name == "harmony-oci/rootfs" {
            if mode != 0o040755
                || uid != 0
                || gid != 0
                || filesize != 0
                || (root_entries == 0 && name != "harmony-oci")
                || (root_entries == 1 && name != "harmony-oci/rootfs")
                || root_entries >= 2
            {
                return Err(malformed("rootfs directory headers"));
            }
            root_entries += 1;
            continue;
        }
        if root_entries != 2 {
            return Err(malformed("missing rootfs directory headers"));
        }
        let path = name
            .strip_prefix("harmony-oci/rootfs/")
            .ok_or_else(|| malformed("entry outside rootfs"))?;
        if path.is_empty()
            || path.split('/').any(|component| {
                component.is_empty()
                    || component.len() > GUEST_NAME_MAX
                    || component == "."
                    || component == ".."
            })
            || path.contains('\0')
            || Path::new(path)
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
            || mode & !0o177777 != 0
            || !matches!(mode & 0o170000, 0o040000 | 0o100000 | 0o120000)
            || (mode & 0o170000 == 0o040000 && filesize != 0)
            || (mode & 0o170000 == 0o120000
                && (mode != 0o120777
                    || data.is_empty()
                    || data.len() >= GUEST_PATH_MAX
                    || data.contains(&0)))
            || names.contains_key(path)
        {
            return Err(malformed("invalid or duplicate rootfs path"));
        }
        if let Some((parent, _)) = path.rsplit_once('/')
            && names.get(parent) != Some(&0o040000)
        {
            return Err(malformed("rootfs parent is absent or not a directory"));
        }
        names.insert(path.to_owned(), mode & 0o170000);
        entries.push(ArchivedRootfsIndex {
            path: path.to_owned(),
            mode,
            data: data_start..data_end,
        });
    }
    Ok(ArchivedRootfs { bytes, entries })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ValidatedExternalInput {
    destination: String,
    relative: String,
    data: Vec<u8>,
}

pub fn prepare(
    image: &StagedImage,
    request: &LaunchRequest,
) -> Result<PreparedExecution, BundleError> {
    image.config.validate()?;
    let external_inputs = validate_external_inputs(&request.external_inputs)?;
    with_host_read_access(&image.rootfs, |original_modes| {
        prepare_readable(image, request, &external_inputs, original_modes)
    })
}

fn prepare_readable(
    image: &StagedImage,
    request: &LaunchRequest,
    external_inputs: &[ValidatedExternalInput],
    original_modes: &BTreeMap<PathBuf, u32>,
) -> Result<PreparedExecution, BundleError> {
    for destination in [
        "/proc",
        "/dev",
        "/dev/pts",
        "/dev/shm",
        "/sys",
        "/sys/fs/cgroup",
        "/run",
        "/tmp",
        SUPERVISOR_PATH,
        EXECUTION_DESTINATION,
        HARMONY_DEVICE,
        KERNEL_LOG_DEVICE,
    ] {
        validate_mount_path(&image.rootfs, destination)?;
    }
    for input in external_inputs {
        validate_mount_path(&image.rootfs, &input.destination)?;
    }
    if request.kvm {
        validate_mount_path(&image.rootfs, KVM_DEVICE)?;
    }
    let credentials = super::credentials::resolve(image.config.user.as_deref(), &image.rootfs)
        .map_err(ImageError::from)?;
    let execution = ExecutionSpec {
        version: VERSION,
        argv: argv(&image.config, &request.command),
        env: env(&image.config),
        cwd: cwd(&image.config),
        uid: credentials.uid,
        gid: credentials.gid,
        additional_gids: credentials.additional_gids,
        bundle: request.bundle.clone(),
    };
    execution.validate()?;
    let rootfs_segment =
        build_rootfs_segment_readable(&image.rootfs, &image.owners, original_modes)?;
    let control_segment =
        build_control_segment_validated(&execution, external_inputs, request.kvm)?;
    let identity = execution_identity(&rootfs_segment, &control_segment, &execution)?;
    Ok(PreparedExecution {
        rootfs_segment,
        control_segment,
        execution,
        identity,
        external_inputs: request.external_inputs.clone(),
        kvm: request.kvm,
    })
}

fn validate_mount_path(rootfs: &Path, destination: &str) -> Result<(), BundleError> {
    let mut path = rootfs.to_path_buf();
    for component in Path::new(destination).components() {
        let Component::Normal(component) = component else {
            continue;
        };
        path = match exact::entry(&path, component) {
            Ok(Some(found)) => found,
            Ok(None) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        };
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(BundleError::MountSymlink {
                    path: destination.to_owned(),
                });
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn build_rootfs_segment(
    rootfs: &Path,
    owners: &Ownership,
) -> Result<Vec<u8>, BundleError> {
    with_host_read_access(rootfs, |original_modes| {
        build_rootfs_segment_readable(rootfs, owners, original_modes)
    })
}

fn build_rootfs_segment_readable(
    rootfs: &Path,
    owners: &Ownership,
    original_modes: &BTreeMap<PathBuf, u32>,
) -> Result<Vec<u8>, BundleError> {
    let mut writer = Writer::new();
    writer.dir("harmony-oci", 0o755);
    writer.dir("harmony-oci/rootfs", 0o755);
    writer.tree_owned_with_modes(
        rootfs,
        "harmony-oci/rootfs",
        &|path| owners.owner_of(path),
        &|path, staged_mode| {
            let original = original_modes
                .get(&rootfs.join(path))
                .copied()
                .unwrap_or(staged_mode);
            owners.mode_of(path, original)
        },
    )?;
    Ok(gzip(&writer.finish()))
}

#[cfg(test)]
fn build_control_segment(
    execution: &ExecutionSpec,
    external_inputs: &[ExternalInput],
) -> Result<Vec<u8>, BundleError> {
    let external_inputs = validate_external_inputs(external_inputs)?;
    build_control_segment_validated(execution, &external_inputs, false)
}

fn build_control_segment_validated(
    execution: &ExecutionSpec,
    external_inputs: &[ValidatedExternalInput],
    kvm: bool,
) -> Result<Vec<u8>, BundleError> {
    execution.validate()?;
    let external_inputs = validate_external_inputs_owned(external_inputs)?;
    let config = runc_spec(&external_inputs, kvm);
    let mut writer = Writer::new();
    writer.dir("harmony-oci", 0o755);
    writer.file(
        "harmony-oci/config.json",
        0o644,
        &serde_json::to_vec(&config)?,
    );
    writer.file("harmony-oci/execution.json", 0o444, &execution.to_vec()?);
    writer.dir("harmony-oci/external", 0o755);
    let mut directories = BTreeSet::new();
    for input in external_inputs {
        let relative = Path::new(&input.relative);
        let mut prefix = PathBuf::new();
        if let Some(parent) = relative.parent() {
            for component in parent.components() {
                let Component::Normal(part) = component else {
                    continue;
                };
                prefix.push(part);
                if directories.insert(prefix.clone()) {
                    writer.dir(&format!("harmony-oci/external/{}", prefix.display()), 0o755);
                }
            }
        }
        writer.file(
            &format!("harmony-oci/external/{}", input.relative),
            0o444,
            &input.data,
        );
    }
    Ok(gzip(&writer.finish()))
}

fn argv(config: &super::image::RuntimeConfig, command: &[String]) -> Vec<String> {
    if !command.is_empty() {
        return command.to_vec();
    }
    let mut argv = config.entrypoint.clone();
    argv.extend(config.cmd.iter().cloned());
    if argv.is_empty() {
        argv.push("/bin/sh".to_owned());
    }
    argv
}

fn env(config: &super::image::RuntimeConfig) -> Vec<String> {
    let mut env = config.env.clone();
    env.retain(|entry| !entry.starts_with("LD_BIND_NOW="));
    env.push("LD_BIND_NOW=1".into());
    if !env.iter().any(|entry| entry.starts_with("PATH=")) {
        env.push("PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into());
    }
    env
}

fn cwd(config: &super::image::RuntimeConfig) -> String {
    match config.working_dir.as_deref() {
        Some("") | None => "/".to_owned(),
        Some(directory) => directory.to_owned(),
    }
}

fn runc_spec(external_inputs: &[ValidatedExternalInput], kvm: bool) -> serde_json::Value {
    let mut mounts = vec![
        json!({
            "destination": "/proc",
            "type": "proc",
            "source": "proc",
            "options": ["nosuid", "noexec", "nodev"]
        }),
        json!({
            "destination": "/dev",
            "type": "tmpfs",
            "source": "tmpfs",
            "options": ["nosuid", "strictatime", "mode=755", "size=65536k"]
        }),
        json!({
            "destination": "/dev/pts",
            "type": "devpts",
            "source": "devpts",
            "options": ["nosuid", "noexec", "newinstance", "ptmxmode=0666", "mode=0620"]
        }),
        json!({
            "destination": "/dev/shm",
            "type": "tmpfs",
            "source": "shm",
            "options": ["nosuid", "noexec", "nodev", "mode=1777", "size=65536k"]
        }),
        json!({
            "destination": "/sys",
            "type": "sysfs",
            "source": "sysfs",
            "options": ["nosuid", "noexec", "nodev", "ro"]
        }),
        json!({
            "destination": "/sys/fs/cgroup",
            "type": "cgroup",
            "source": "cgroup",
            "options": ["nosuid", "noexec", "nodev", "rw"]
        }),
        json!({
            "destination": "/run",
            "type": "tmpfs",
            "source": "tmpfs",
            "options": ["nosuid", "nodev", "mode=755"]
        }),
        json!({
            "destination": "/tmp",
            "type": "tmpfs",
            "source": "tmpfs",
            "options": ["nosuid", "nodev", "mode=1777"]
        }),
        json!({
            "destination": SUPERVISOR_PATH,
            "type": "bind",
            "source": SUPERVISOR_PATH,
            "options": ["bind", "ro"]
        }),
        json!({
            "destination": EXECUTION_DESTINATION,
            "type": "bind",
            "source": EXECUTION_SOURCE,
            "options": ["bind", "ro"]
        }),
        json!({
            "destination": HARMONY_DEVICE,
            "type": "bind",
            "source": HARMONY_DEVICE,
            "options": ["bind"]
        }),
        json!({
            "destination": KERNEL_LOG_DEVICE,
            "type": "bind",
            "source": KERNEL_LOG_DEVICE,
            "options": ["bind", "ro"]
        }),
    ];
    let mut devices = vec![
        json!({ "allow": true, "type": "c", "major": "HARMONY_SDK_MAJOR", "minor": "HARMONY_SDK_MINOR", "access": "rw" }),
        json!({ "allow": true, "type": "c", "major": 1, "minor": 11, "access": "r" }),
    ];
    if kvm {
        mounts.push(json!({
            "destination": KVM_DEVICE,
            "type": "bind",
            "source": KVM_DEVICE,
            "options": ["bind"]
        }));
        devices
            .push(json!({ "allow": true, "type": "c", "major": 10, "minor": 232, "access": "rw" }));
    }
    for input in external_inputs {
        mounts.push(json!({
            "destination": input.destination,
            "type": "bind",
            "source": format!("/harmony-oci/external/{}", input.relative),
            "options": ["bind", "ro"]
        }));
    }
    json!({
        "ociVersion": "1.0.2",
        "process": {
            "terminal": false,
            "user": { "uid": 0, "gid": 0, "additionalGids": [] },
            "args": [SUPERVISOR_PATH],
            "env": [
                "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                "HARMONY_EXECUTION_SPEC=/run/harmony/execution.json",
                "LD_BIND_NOW=1"
            ],
            "cwd": "/"
        },
        "root": { "path": "rootfs", "readonly": false },
        "hostname": "harmony",
        "mounts": mounts,
        "linux": {
            "rootfsPropagation": "rslave",
            "cgroupsPath": "harmony",
            "resources": {
                "devices": devices
            },
            "namespaces": [
                { "type": "pid" },
                { "type": "ipc" },
                { "type": "uts" },
                { "type": "mount" },
                { "type": "cgroup" },
                { "type": "network" }
            ]
        }
    })
}

fn validate_external_inputs(
    inputs: &[ExternalInput],
) -> Result<Vec<ValidatedExternalInput>, BundleError> {
    let mut validated = Vec::with_capacity(inputs.len());
    for input in inputs {
        let destination = input
            .destination
            .to_str()
            .ok_or(BundleError::ExternalInputUtf8)?
            .to_owned();
        let relative = validate_destination(&destination)?;
        validated.push(ValidatedExternalInput {
            destination,
            relative,
            data: input.data.clone(),
        });
    }
    validate_external_inputs_owned(&validated)
}

fn validate_external_inputs_owned(
    inputs: &[ValidatedExternalInput],
) -> Result<Vec<ValidatedExternalInput>, BundleError> {
    if inputs.len() > MAX_EXTERNAL_INPUTS {
        return Err(BundleError::ExternalInputCount {
            count: inputs.len(),
            limit: MAX_EXTERNAL_INPUTS,
        });
    }
    let mut sorted = inputs.to_vec();
    sorted.sort_by(|left, right| left.destination.cmp(&right.destination));
    let mut total = 0usize;
    for (index, input) in sorted.iter().enumerate() {
        total = total
            .checked_add(input.data.len())
            .ok_or(BundleError::ExternalInputBytes {
                bytes: usize::MAX,
                limit: MAX_EXTERNAL_INPUT_BYTES,
            })?;
        if total > MAX_EXTERNAL_INPUT_BYTES {
            return Err(BundleError::ExternalInputBytes {
                bytes: total,
                limit: MAX_EXTERNAL_INPUT_BYTES,
            });
        }
        validate_destination(&input.destination)?;
        for previous in &sorted[..index] {
            if input.destination == previous.destination
                || input
                    .destination
                    .starts_with(&format!("{}/", previous.destination))
            {
                return Err(BundleError::ExternalInputConflict {
                    path: input.destination.clone(),
                    other: previous.destination.clone(),
                });
            }
        }
    }
    Ok(sorted)
}

fn validate_destination(destination: &str) -> Result<String, BundleError> {
    if destination.is_empty() {
        return Err(BundleError::ExternalInputPath {
            path: destination.to_owned(),
            reason: "empty",
        });
    }
    if destination.len() > MAX_EXTERNAL_INPUT_PATH_BYTES {
        return Err(BundleError::ExternalInputPath {
            path: destination.to_owned(),
            reason: "too long",
        });
    }
    if destination.contains('\0') {
        return Err(BundleError::ExternalInputPath {
            path: destination.to_owned(),
            reason: "contains NUL",
        });
    }
    let path = Path::new(destination);
    if !path.is_absolute() {
        return Err(BundleError::ExternalInputPath {
            path: destination.to_owned(),
            reason: "not absolute",
        });
    }
    let mut normalized = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::CurDir | Component::ParentDir | Component::Prefix(_) => {
                return Err(BundleError::ExternalInputPath {
                    path: destination.to_owned(),
                    reason: "not normalized",
                });
            }
        }
    }
    if normalized.to_str() != Some(destination) {
        return Err(BundleError::ExternalInputPath {
            path: destination.to_owned(),
            reason: "not normalized",
        });
    }
    if normalized == Path::new("/") {
        return Err(BundleError::ExternalInputPath {
            path: destination.to_owned(),
            reason: "root is not a file",
        });
    }
    for reserved in [
        "/dev",
        "/proc",
        "/sys",
        "/run",
        "/harmony-oci",
        "/usr/lib/harmony",
    ] {
        let reserved = Path::new(reserved);
        if normalized == reserved || normalized.starts_with(reserved) {
            return Err(BundleError::ExternalInputPath {
                path: destination.to_owned(),
                reason: "platform-owned path",
            });
        }
    }
    Ok(destination[1..].to_owned())
}

fn execution_identity(
    rootfs_segment: &[u8],
    control_segment: &[u8],
    execution: &ExecutionSpec,
) -> Result<[u8; 32], BundleError> {
    let execution_json = execution.to_vec()?;
    let mut digest = Sha256::new();
    digest.update(b"harmony-oci-prepared-execution-v1\0");
    digest_field(&mut digest, rootfs_segment);
    digest_field(&mut digest, control_segment);
    digest_field(&mut digest, &execution_json);
    Ok(digest.finalize().into())
}

fn digest_field(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let compressed = miniz_oxide::deflate::compress_to_vec(data, 1);
    let mut output = Vec::with_capacity(compressed.len() + 18);
    output.extend_from_slice(&[0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 4, 255]);
    output.extend_from_slice(&compressed);
    output.extend_from_slice(&crc32fast::hash(data).to_le_bytes());
    output.extend_from_slice(&(data.len() as u32).to_le_bytes());
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::RuntimeConfig;
    use guest_image::Owner;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    fn image() -> StagedImage {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("etc")).unwrap();
        std::fs::write(
            dir.path().join("etc/passwd"),
            b"root:x:0:0:root:/root:/bin/sh\nworker:x:42:84:worker:/work:/bin/sh\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("etc/group"),
            b"root:x:0:\nworker:x:84:\nshared:x:99:worker\n",
        )
        .unwrap();
        let rootfs = dir.path().to_path_buf();
        let result = StagedImage {
            rootfs,
            owners: Ownership::default(),
            config: RuntimeConfig {
                entrypoint: vec!["/bin/app".into()],
                cmd: vec!["run".into()],
                env: vec!["APP=one".into()],
                working_dir: Some("/work".into()),
                user: Some("worker".into()),
            },
        };
        std::mem::forget(dir);
        result
    }

    fn execution() -> ExecutionSpec {
        ExecutionSpec {
            version: VERSION,
            argv: vec!["/bin/app".into()],
            env: vec!["PATH=/bin".into()],
            cwd: "/".into(),
            uid: 42,
            gid: 84,
            additional_gids: vec![99],
            bundle: None,
        }
    }

    fn unpack(segment: &[u8]) -> Vec<u8> {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut file, segment).unwrap();
        let output = Command::new("gzip")
            .args(["-dc"])
            .arg(file.path())
            .output()
            .unwrap();
        assert!(output.status.success());
        output.stdout
    }

    #[test]
    fn archived_rootfs_visits_the_packed_bytes_and_rejects_malformed_segments() {
        let staged = image();
        std::fs::write(staged.rootfs.join("proof"), b"packed only").unwrap();
        let prepared = prepare(&staged, &LaunchRequest::default()).unwrap();
        let archive = prepared.archived_rootfs().unwrap();
        assert!(archive.entries().any(|entry| {
            entry.path == "proof"
                && entry.mode & 0o170000 == 0o100000
                && entry.data == b"packed only"
        }));

        let mut changed = prepared.clone();
        changed.rootfs_segment.pop();
        assert!(changed.archived_rootfs().is_err());

        let mut bytes = archive.bytes;
        bytes[0] = b'x';
        changed.rootfs_segment = gzip(&bytes);
        assert!(changed.archived_rootfs().is_err());
    }

    fn cpio_entries(segment: &[u8]) -> Vec<(String, u32)> {
        let bytes = unpack(segment);
        let mut entries = Vec::new();
        let mut at = 0;
        loop {
            let field = |i: usize| {
                let text =
                    std::str::from_utf8(&bytes[at + 6 + 8 * i..at + 6 + 8 * (i + 1)]).unwrap();
                u32::from_str_radix(text, 16).unwrap()
            };
            assert_eq!(&bytes[at..at + 6], b"070701");
            let (mode, filesize, namesize) = (field(1), field(6), field(11));
            let name_at = at + 110;
            let name = std::str::from_utf8(&bytes[name_at..name_at + namesize as usize - 1])
                .unwrap()
                .to_string();
            entries.push((name.clone(), mode));
            if name == "TRAILER!!!" {
                break;
            }
            let data_at = (name_at + namesize as usize).next_multiple_of(4);
            at = (data_at + filesize as usize).next_multiple_of(4);
        }
        entries
    }

    #[test]
    fn launch_resolves_image_argv_environment_cwd_and_credentials() {
        let prepared = prepare(&image(), &LaunchRequest::new(Vec::new())).unwrap();
        assert_eq!(prepared.execution.argv, ["/bin/app", "run"]);
        assert_eq!(prepared.execution.env[0], "APP=one");
        assert!(
            prepared
                .execution
                .env
                .iter()
                .any(|entry| entry.starts_with("PATH="))
        );
        assert_eq!(prepared.execution.cwd, "/work");
        assert_eq!(prepared.execution.uid, 42);
        assert_eq!(prepared.execution.gid, 84);
        assert_eq!(prepared.execution.additional_gids, [99]);
    }

    #[test]
    fn eager_binding_is_carried_through_supervisor_and_workload_launch() {
        for configured in [None, Some("LD_BIND_NOW="), Some("LD_BIND_NOW=0")] {
            let mut image = image();
            if let Some(value) = configured {
                image.config.env.push(value.into());
            }
            let prepared = prepare(&image, &LaunchRequest::new(Vec::new())).unwrap();
            let binding = prepared
                .execution
                .env
                .iter()
                .filter(|entry| entry.starts_with("LD_BIND_NOW="))
                .collect::<Vec<_>>();
            assert_eq!(binding, ["LD_BIND_NOW=1"]);
            let spec = runc_spec(&[], false);
            assert!(
                spec["process"]["env"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|entry| entry == "LD_BIND_NOW=1")
            );
        }
    }

    #[test]
    fn command_override_and_bundle_are_carried_into_execution_spec() {
        let request = LaunchRequest::new(vec!["/bin/custom".into(), "arg".into()])
            .with_bundle("/etc/harmony/bundle");
        let prepared = prepare(&image(), &request).unwrap();
        assert_eq!(prepared.execution.argv, ["/bin/custom", "arg"]);
        assert_eq!(
            prepared.execution.bundle.as_deref(),
            Some("/etc/harmony/bundle")
        );
    }

    #[test]
    fn control_segment_has_one_supervisor_launch_and_required_mounts() {
        let spec = execution();
        let segment = build_control_segment(&spec, &[]).unwrap();
        let text = String::from_utf8(unpack(&segment)).unwrap();
        assert!(text.contains("harmony-oci/config.json"));
        assert!(text.contains("harmony-oci/execution.json"));
        assert_eq!(text.matches("/usr/lib/harmony/supervisor").count(), 3);
        assert!(text.contains("/dev/harmony"));
        assert!(!text.contains("/dev/mem"));

        let config = runc_spec(&[], false);
        let mounts = config["mounts"].as_array().unwrap();
        let kmsg = mounts
            .iter()
            .find(|mount| mount["destination"] == KERNEL_LOG_DEVICE)
            .unwrap();
        assert_eq!(kmsg["type"], "bind");
        assert_eq!(kmsg["source"], KERNEL_LOG_DEVICE);
        assert_eq!(kmsg["options"], json!(["bind", "ro"]));

        let devices = config["linux"]["resources"]["devices"].as_array().unwrap();
        assert!(devices.iter().any(|device| {
            device["type"] == "c"
                && device["major"] == 1
                && device["minor"] == 11
                && device["access"] == "r"
                && device["allow"] == true
        }));
    }

    #[test]
    fn kvm_access_is_explicit_bound_to_identity_and_rejects_aliases() {
        let image = image();
        let ordinary = prepare(&image, &LaunchRequest::default()).unwrap();
        let nested = prepare(&image, &LaunchRequest::default().with_kvm()).unwrap();
        assert_ne!(ordinary.identity, nested.identity);
        let ordinary_config = runc_spec(&[], false);
        assert!(
            ordinary_config["mounts"]
                .as_array()
                .unwrap()
                .iter()
                .all(|mount| mount["destination"] != KVM_DEVICE)
        );
        let nested_config = runc_spec(&[], true);
        assert!(
            nested_config["mounts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|mount| mount["destination"] == KVM_DEVICE && mount["source"] == KVM_DEVICE)
        );
        assert!(
            nested_config["linux"]["resources"]["devices"]
                .as_array()
                .unwrap()
                .iter()
                .any(|device| device["major"] == 10
                    && device["minor"] == 232
                    && device["access"] == "rw")
        );
        std::fs::create_dir_all(image.rootfs.join("dev")).unwrap();
        std::os::unix::fs::symlink("/dev/null", image.rootfs.join("dev/kvm")).unwrap();
        assert!(matches!(
            prepare(&image, &LaunchRequest::default().with_kvm()),
            Err(BundleError::MountSymlink { .. })
        ));
    }

    #[test]
    fn external_inputs_are_sorted_and_readonly() {
        let spec = execution();
        let inputs = vec![
            ExternalInput::new("/z/file", b"z"),
            ExternalInput::new("/a/file", b"a"),
        ];
        let segment = build_control_segment(&spec, &inputs).unwrap();
        let text = String::from_utf8(unpack(&segment)).unwrap();
        assert!(text.find("external/a/file").unwrap() < text.find("external/z/file").unwrap());
        assert!(text.contains("\"ro\""));
    }

    #[test]
    fn root_level_external_input_is_emitted_only_as_a_file() {
        let spec = execution();
        let segment =
            build_control_segment(&spec, &[ExternalInput::new("/program.bin", b"program")])
                .unwrap();
        let entries = cpio_entries(&segment);
        let external_entry = entries
            .iter()
            .find(|(name, _)| name == "harmony-oci/external/program.bin")
            .unwrap();
        assert_eq!(external_entry.1 & 0o170000, 0o100000);
        assert_eq!(
            entries
                .iter()
                .filter(|(name, _)| name == "harmony-oci/external/program.bin")
                .count(),
            1
        );
    }

    #[test]
    fn external_input_paths_reject_escaping_reserved_and_unnormalized_paths() {
        for path in [
            "relative",
            "/a/../b",
            "/a//b",
            "/run/harmony/x",
            "/dev/mem",
            "/",
        ] {
            let error = validate_external_inputs(&[ExternalInput::new(path, b"x")]).unwrap_err();
            assert!(
                matches!(error, BundleError::ExternalInputPath { .. }),
                "{path}: {error:?}"
            );
        }
    }

    #[test]
    fn external_input_paths_reject_duplicates_and_ancestor_conflicts() {
        let duplicate = [
            ExternalInput::new("/a", b"one"),
            ExternalInput::new("/a", b"two"),
        ];
        assert!(matches!(
            validate_external_inputs(&duplicate),
            Err(BundleError::ExternalInputConflict { .. })
        ));
        let ancestor = [
            ExternalInput::new("/a", b"one"),
            ExternalInput::new("/a/b", b"two"),
        ];
        assert!(matches!(
            validate_external_inputs(&ancestor),
            Err(BundleError::ExternalInputConflict { .. })
        ));
        let non_adjacent = [
            ExternalInput::new("/a/b", b"child"),
            ExternalInput::new("/a-foo", b"sibling"),
            ExternalInput::new("/a", b"parent"),
        ];
        assert!(matches!(
            validate_external_inputs(&non_adjacent),
            Err(BundleError::ExternalInputConflict { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn mount_destinations_require_symlink_free_image_paths() {
        let staged = image();
        std::os::unix::fs::symlink("/data", staged.rootfs.join("input")).unwrap();
        let request = LaunchRequest::default()
            .with_external_inputs(vec![ExternalInput::new("/input/file", b"data")]);
        assert!(matches!(
            prepare(&staged, &request),
            Err(BundleError::MountSymlink { .. })
        ));

        let staged = image();
        std::os::unix::fs::symlink("/opt", staged.rootfs.join("usr")).unwrap();
        assert!(matches!(
            prepare(&staged, &LaunchRequest::default()),
            Err(BundleError::MountSymlink { .. })
        ));

        let staged = image();
        std::fs::create_dir(staged.rootfs.join("dev")).unwrap();
        std::os::unix::fs::symlink("/dev/null", staged.rootfs.join("dev/kmsg")).unwrap();
        assert!(matches!(
            prepare(&staged, &LaunchRequest::default()),
            Err(BundleError::MountSymlink { path }) if path == KERNEL_LOG_DEVICE
        ));
    }

    #[test]
    fn mount_destination_checks_require_exact_component_spelling() {
        let rootfs = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("/outside", rootfs.path().join("TMP")).unwrap();
        validate_mount_path(rootfs.path(), "/tmp").unwrap();
    }

    #[test]
    fn external_input_bounds_are_enforced() {
        assert_eq!(MAX_EXTERNAL_INPUT_BYTES, 67_108_864);
        let maximum_count: Vec<_> = (0..MAX_EXTERNAL_INPUTS)
            .map(|index| ExternalInput::new(format!("/input-{index}"), Vec::new()))
            .collect();
        assert_eq!(
            validate_external_inputs(&maximum_count).unwrap().len(),
            MAX_EXTERNAL_INPUTS
        );
        let maximum_path = format!("/{}", "x".repeat(MAX_EXTERNAL_INPUT_PATH_BYTES - 1));
        assert_eq!(
            validate_destination(&maximum_path).unwrap(),
            &maximum_path[1..]
        );
        assert!(validate_destination(&format!("{maximum_path}x")).is_err());
        let maximum_bytes = [ExternalInput::new(
            "/input",
            vec![0; MAX_EXTERNAL_INPUT_BYTES],
        )];
        assert_eq!(
            validate_external_inputs(&maximum_bytes).unwrap()[0]
                .data
                .len(),
            MAX_EXTERNAL_INPUT_BYTES
        );
        let too_many: Vec<_> = (0..=MAX_EXTERNAL_INPUTS)
            .map(|index| ExternalInput::new(format!("/input-{index}"), Vec::new()))
            .collect();
        assert!(matches!(
            validate_external_inputs(&too_many),
            Err(BundleError::ExternalInputCount { .. })
        ));
        let too_large = [ExternalInput::new(
            "/input",
            vec![0; MAX_EXTERNAL_INPUT_BYTES + 1],
        )];
        assert!(matches!(
            validate_external_inputs(&too_large),
            Err(BundleError::ExternalInputBytes { .. })
        ));
    }

    #[test]
    fn prepared_record_round_trip_preserves_the_actual_execution() {
        let launch = LaunchRequest::default()
            .with_external_inputs(vec![ExternalInput::new("/input/value", b"captured")])
            .with_kvm();
        let prepared = prepare(&image(), &launch).unwrap();
        let record: PreparedExecutionRecord =
            serde_json::from_slice(&serde_json::to_vec(&prepared.record()).unwrap()).unwrap();
        let imported = PreparedExecution::from_record(record).unwrap();
        assert_eq!(prepared, imported);
        assert_eq!(prepared.initramfs(b"base"), imported.initramfs(b"base"));
        for change in 0..4 {
            let mut record = prepared.record();
            match change {
                0 => record.version += 1,
                1 => record.execution.argv.push("changed".into()),
                2 => record.external_inputs[0].data.push(1),
                _ => record.rootfs_segment[12] ^= 1,
            }
            assert!(PreparedExecution::from_record(record).is_err());
        }
    }

    #[test]
    fn prepared_record_rejects_a_recommitted_mount_alias() {
        let staged = image();
        let prepared = prepare(&staged, &LaunchRequest::default()).unwrap();
        std::os::unix::fs::symlink("/elsewhere", staged.rootfs.join("usr")).unwrap();
        let mut record = prepared.record();
        record.rootfs_segment = build_rootfs_segment(&staged.rootfs, &staged.owners).unwrap();
        record.identity = execution_identity(
            &record.rootfs_segment,
            prepared.control_segment(),
            &record.execution,
        )
        .unwrap();
        assert!(matches!(
            PreparedExecution::from_record(record),
            Err(BundleError::MountSymlink { .. })
        ));
    }

    #[test]
    fn archived_rootfs_inflation_stops_at_the_explicit_limit() {
        let bytes = miniz_oxide::deflate::compress_to_vec(&[7; 4096], 1);
        assert_eq!(inflate_rootfs(&bytes, 4096).unwrap(), [7; 4096]);
        let error = inflate_rootfs(&bytes, 1024).unwrap_err().to_string();
        assert!(error.contains("output limit"));
        assert!(error.len() < 128);
    }

    fn import_rootfs_segment(segment: Vec<u8>) -> Result<PreparedExecution, BundleError> {
        let prepared = prepare(&image(), &LaunchRequest::default()).unwrap();
        let mut record = prepared.record();
        record.rootfs_segment = segment;
        record.identity = execution_identity(
            &record.rootfs_segment,
            prepared.control_segment(),
            &record.execution,
        )
        .unwrap();
        PreparedExecution::from_record(record)
    }

    fn rootfs_writer() -> Writer {
        let mut writer = Writer::new();
        writer.dir("harmony-oci", 0o755);
        writer.dir("harmony-oci/rootfs", 0o755);
        writer
    }

    #[test]
    fn prepared_record_rejects_additional_compressed_streams_and_truncated_deflate() {
        let prepared = prepare(&image(), &LaunchRequest::default()).unwrap();
        let mut multiple = prepared.rootfs_segment.clone();
        multiple.extend_from_slice(&prepared.rootfs_segment);
        let error = import_rootfs_segment(multiple).unwrap_err().to_string();
        assert!(error.contains("trailing input"), "{error}");
        let compressed = miniz_oxide::deflate::compress_to_vec(&[9; 4096], 1);
        for length in [0, 1, compressed.len() - 1] {
            assert!(inflate_rootfs(&compressed[..length], 4096).is_err());
        }
        let mut trailing = compressed.clone();
        trailing.push(0);
        assert!(
            inflate_rootfs(&trailing, 4096)
                .unwrap_err()
                .to_string()
                .contains("trailing input")
        );
    }

    #[test]
    fn prepared_record_requires_complete_directory_parents_for_every_entry() {
        for kind in ["symlink", "file", "absent"] {
            let mut writer = rootfs_writer();
            match kind {
                "symlink" => writer.symlink("harmony-oci/rootfs/escape", b"/usr/lib/harmony"),
                "file" => writer.file("harmony-oci/rootfs/escape", 0o644, b"regular"),
                _ => {}
            }
            writer.file("harmony-oci/rootfs/escape/init", 0o755, b"replacement");
            let error = import_rootfs_segment(gzip(&writer.finish()))
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("parent is absent or not a directory"),
                "{kind}: {error}"
            );
        }
        let mut valid = rootfs_writer();
        valid.dir_owned("harmony-oci/rootfs/data", 0o750, Owner { uid: 42, gid: 84 });
        valid.file_owned(
            "harmony-oci/rootfs/data/value",
            0o640,
            Owner { uid: 42, gid: 84 },
            b"value",
        );
        valid.symlink("harmony-oci/rootfs/alias", b"data/value");
        assert!(import_rootfs_segment(gzip(&valid.finish())).is_ok());
    }

    #[test]
    fn prepared_record_rejects_noncanonical_newc_headers_and_path_aliases() {
        let canonical = rootfs_writer().finish();
        for (index, value) in [
            (0, 2),
            (4, 0),
            (4, 2),
            (5, 1),
            (7, 1),
            (8, 1),
            (9, 1),
            (10, 1),
            (12, 1),
        ] {
            let mut bytes = canonical.clone();
            let start = 6 + index * 8;
            bytes[start..start + 8].copy_from_slice(format!("{value:08x}").as_bytes());
            assert!(
                import_rootfs_segment(gzip(&bytes)).is_err(),
                "field{index}={value}"
            );
        }
        let mut uppercase = canonical.clone();
        uppercase[14..22].copy_from_slice(b"000041ED");
        assert!(import_rootfs_segment(gzip(&uppercase)).is_err());
        let mut padding = canonical;
        padding[110 + "harmony-oci".len() + 1] = 1;
        assert!(import_rootfs_segment(gzip(&padding)).is_err());
        for path in ["a/../b", "a//b", "a/./b", "a/", "./a", "/a"] {
            let mut writer = rootfs_writer();
            writer.file(&format!("harmony-oci/rootfs/{path}"), 0o644, b"value");
            assert!(
                import_rootfs_segment(gzip(&writer.finish())).is_err(),
                "{path}"
            );
        }
        let mut duplicate = rootfs_writer();
        duplicate.file("harmony-oci/rootfs/value", 0o644, b"one");
        duplicate.file("harmony-oci/rootfs/value", 0o644, b"two");
        assert!(import_rootfs_segment(gzip(&duplicate.finish())).is_err());
    }

    fn rootfs_with_name_length(length: usize) -> Vec<u8> {
        let mut writer = rootfs_writer();
        let mut path = "harmony-oci/rootfs".to_owned();
        while length - path.len() > GUEST_NAME_MAX + 1 {
            path.push('/');
            path.push_str(&"d".repeat(GUEST_NAME_MAX));
            writer.dir(&path, 0o755);
        }
        path.push('/');
        path.push_str(&"f".repeat(length - path.len()));
        writer.file(&path, 0o644, b"value");
        writer.finish()
    }

    #[test]
    fn prepared_record_enforces_guest_name_and_symlink_length_boundaries() {
        assert!(import_rootfs_segment(gzip(&rootfs_with_name_length(GUEST_PATH_MAX - 1))).is_ok());
        assert!(import_rootfs_segment(gzip(&rootfs_with_name_length(GUEST_PATH_MAX))).is_err());
        for (length, accepted) in [(GUEST_NAME_MAX, true), (GUEST_NAME_MAX + 1, false)] {
            let mut writer = rootfs_writer();
            writer.file(
                &format!("harmony-oci/rootfs/{}", "f".repeat(length)),
                0o644,
                b"value",
            );
            assert_eq!(
                import_rootfs_segment(gzip(&writer.finish())).is_ok(),
                accepted
            );
        }
        for (length, accepted) in [
            (GUEST_PATH_MAX - 1, true),
            (GUEST_PATH_MAX, false),
            (GUEST_PATH_MAX + 1, false),
        ] {
            let mut writer = rootfs_writer();
            writer.symlink("harmony-oci/rootfs/leaf", &vec![b'x'; length]);
            assert_eq!(
                import_rootfs_segment(gzip(&writer.finish())).is_ok(),
                accepted
            );
        }
    }

    #[test]
    fn prepared_bytes_and_identity_are_reproducible() {
        let first = prepare(&image(), &LaunchRequest::default()).unwrap();
        let second = prepare(&image(), &LaunchRequest::default()).unwrap();
        assert_eq!(first.rootfs_segment, second.rootfs_segment);
        assert_eq!(first.control_segment, second.control_segment);
        assert_eq!(first.identity, second.identity);
        assert_eq!(first.initramfs(b"base"), second.initramfs(b"base"));
        let combined = first.initramfs(b"base");
        assert_eq!(&combined[..4], b"base");
        assert_eq!(
            &combined[4..4 + first.rootfs_segment.len()],
            first.rootfs_segment
        );
        assert_eq!(
            &combined[4 + first.rootfs_segment.len()..],
            first.control_segment
        );
        assert_eq!(first.identity_hex().len(), 64);
        for (pair, byte) in first
            .identity_hex()
            .as_bytes()
            .chunks_exact(2)
            .zip(first.identity)
        {
            assert_eq!(
                u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap(),
                byte
            );
        }
    }

    #[test]
    fn prepared_identity_is_portable_for_a_large_rootfs() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let payload: Vec<u8> = (0..1 << 20)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 56) as u8
            })
            .collect();
        let path = dir.path().join("payload");
        std::fs::write(&path, payload).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let image = StagedImage {
            rootfs: dir.path().to_path_buf(),
            owners: Ownership::default(),
            config: RuntimeConfig::default(),
        };
        let prepared = prepare(&image, &LaunchRequest::new(vec!["/payload".into()])).unwrap();
        assert_eq!(
            format!("{:x}", Sha256::digest(&prepared.rootfs_segment)),
            "07b518ca1704197efe27b6e33670a5665366854a59943501273d10a8ae273b3f"
        );
        assert_eq!(
            prepared.identity_hex(),
            "cc22e0ae8c440f7edd93b48485fa1a3504bd95072e91a5fcbea6935b589b2f05"
        );
    }

    #[test]
    fn mount_validation_accepts_existing_regular_paths_and_reports_non_directory_parents() {
        let staged = image();
        std::fs::create_dir(staged.rootfs.join("input")).unwrap();
        std::fs::write(staged.rootfs.join("input/file"), b"image file").unwrap();
        let request = LaunchRequest::default()
            .with_external_inputs(vec![ExternalInput::new("/input/file", b"external file")]);
        assert!(prepare(&staged, &request).is_ok());
        assert!(matches!(
            validate_mount_path(&staged.rootfs, "/input/file/child"),
            Err(BundleError::Io(error)) if error.kind() == std::io::ErrorKind::NotADirectory
        ));
    }

    #[test]
    fn changing_external_bytes_changes_identity() {
        let first = prepare(
            &image(),
            &LaunchRequest::default()
                .with_external_inputs(vec![ExternalInput::new("/input", b"one")]),
        )
        .unwrap();
        let second = prepare(
            &image(),
            &LaunchRequest::default()
                .with_external_inputs(vec![ExternalInput::new("/input", b"two")]),
        )
        .unwrap();
        assert_ne!(first.identity, second.identity);
    }

    #[test]
    fn control_segment_rejects_invalid_execution_specs() {
        let mut spec = execution();
        spec.argv = Vec::new();
        assert!(matches!(
            build_control_segment(&spec, &[]),
            Err(BundleError::Spec(SpecError::EmptyArgv))
        ));
    }
}
