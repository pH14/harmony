// SPDX-License-Identifier: AGPL-3.0-or-later

use super::cpio::{CpioError, Writer};
use super::image::{ImageError, Ownership, StagedImage};
use execution_proto::{ExecutionSpec, SpecError, VERSION};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

pub const CONTROL_ROOT: &str = "/harmony-oci";
pub const ROOTFS_ROOT: &str = "/harmony-oci/rootfs";
pub const EXECUTION_SOURCE: &str = "/harmony-oci/execution.json";
pub const EXECUTION_DESTINATION: &str = execution_proto::EXECUTION_PATH;
pub const SUPERVISOR_PATH: &str = "/usr/lib/harmony/supervisor";
pub const PARK_DEVICE: &str = "/dev/harmony-park";
pub const HARMONY_DEVICE: &str = "/dev/harmony";
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
    #[error("gzip failed: {0}")]
    Gzip(String),
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
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Clone, Debug, Eq, PartialEq)]
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
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedExecution {
    pub rootfs_segment: Vec<u8>,
    pub control_segment: Vec<u8>,
    pub execution: ExecutionSpec,
    pub identity: [u8; 32],
}

impl PreparedExecution {
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
    let credentials = image.config.resolve_process_credentials(&image.rootfs)?;
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
    let rootfs_segment = build_rootfs_segment(&image.rootfs, &image.owners)?;
    let control_segment = build_control_segment_validated(&execution, &external_inputs)?;
    let identity = execution_identity(&rootfs_segment, &control_segment, &execution)?;
    Ok(PreparedExecution {
        rootfs_segment,
        control_segment,
        execution,
        identity,
    })
}

pub fn build_rootfs_segment(rootfs: &Path, owners: &Ownership) -> Result<Vec<u8>, BundleError> {
    let mut writer = Writer::new();
    writer.dir("harmony-oci", 0o755);
    writer.dir("harmony-oci/rootfs", 0o755);
    writer.tree_owned(rootfs, "harmony-oci/rootfs", &|path| owners.owner_of(path))?;
    gzip(&writer.finish())
}

pub fn build_control_segment(
    execution: &ExecutionSpec,
    external_inputs: &[ExternalInput],
) -> Result<Vec<u8>, BundleError> {
    let external_inputs = validate_external_inputs(external_inputs)?;
    build_control_segment_validated(execution, &external_inputs)
}

fn build_control_segment_validated(
    execution: &ExecutionSpec,
    external_inputs: &[ValidatedExternalInput],
) -> Result<Vec<u8>, BundleError> {
    execution.validate()?;
    let external_inputs = validate_external_inputs_owned(external_inputs)?;
    let config = runc_spec(&external_inputs);
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
        for component in relative.components() {
            let Component::Normal(part) = component else {
                continue;
            };
            prefix.push(part);
            if directories.insert(prefix.clone()) {
                writer.dir(&format!("harmony-oci/external/{}", prefix.display()), 0o755);
            }
        }
        writer.file(
            &format!("harmony-oci/external/{}", input.relative),
            0o444,
            &input.data,
        );
    }
    gzip(&writer.finish())
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

fn runc_spec(external_inputs: &[ValidatedExternalInput]) -> serde_json::Value {
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
            "destination": PARK_DEVICE,
            "type": "bind",
            "source": PARK_DEVICE,
            "options": ["bind"]
        }),
    ];
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
                "HARMONY_EXECUTION_SPEC=/run/harmony/execution.json"
            ],
            "cwd": "/"
        },
        "root": { "path": "rootfs", "readonly": false },
        "hostname": "harmony",
        "mounts": mounts,
        "linux": {
            "rootfsPropagation": "private",
            "cgroupsPath": "harmony",
            "namespaces": [
                { "type": "pid" },
                { "type": "ipc" },
                { "type": "uts" },
                { "type": "mount" },
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
        if index != 0 {
            let previous = &sorted[index - 1];
            if input.destination == previous.destination {
                return Err(BundleError::ExternalInputConflict {
                    path: input.destination.clone(),
                    other: previous.destination.clone(),
                });
            }
            if input
                .destination
                .starts_with(&format!("{}/", previous.destination))
            {
                return Err(BundleError::ExternalInputConflict {
                    path: input.destination.clone(),
                    other: previous.destination.clone(),
                });
            }
        }
        for previous in &sorted[..index] {
            if previous
                .destination
                .starts_with(&format!("{}/", input.destination))
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

fn gzip(data: &[u8]) -> Result<Vec<u8>, BundleError> {
    let mut input = tempfile::NamedTempFile::new()?;
    std::io::Write::write_all(&mut input, data)?;
    let output = Command::new("gzip")
        .args(["-n", "-1", "-c"])
        .arg(input.path())
        .output()?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(BundleError::Gzip(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::RuntimeConfig;

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
        assert!(text.contains("/dev/harmony-park"));
        assert!(!text.contains("/dev/mem"));
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
    }

    #[test]
    fn external_input_bounds_are_enforced() {
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
    fn prepared_bytes_and_identity_are_reproducible() {
        let first = prepare(&image(), &LaunchRequest::default()).unwrap();
        let second = prepare(&image(), &LaunchRequest::default()).unwrap();
        assert_eq!(first.rootfs_segment, second.rootfs_segment);
        assert_eq!(first.control_segment, second.control_segment);
        assert_eq!(first.identity, second.identity);
        assert_eq!(first.initramfs(b"base"), second.initramfs(b"base"));
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
