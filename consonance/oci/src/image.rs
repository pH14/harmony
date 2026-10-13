// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::credentials;
pub use crate::credentials::{CredentialsError, ProcessCredentials};
use crate::exact;
use guest_image::Owner;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, thiserror::Error)]
pub enum ImageError {
    #[error("no container tool found: install docker or podman, or pass a `docker save` tarball")]
    NoTool,
    #[error("`{tool} image save {image}` failed: {detail}")]
    ExportFailed {
        tool: &'static str,
        image: String,
        detail: String,
    },
    #[error("unrecognized image input {0}: not a docker-save tarball or OCI layout")]
    Unrecognized(PathBuf),
    #[error("image has no config/rootfs for this architecture")]
    NoConfig,
    #[error("image environment entry {index} is not VARNAME=VARVALUE")]
    InvalidEnvironment { index: usize },
    #[error("image environment key {key:?} occurs more than once")]
    DuplicateEnvironmentKey { key: String },
    #[error("image working directory {0:?} is not an absolute normalized path")]
    InvalidWorkingDirectory(String),
    #[error("image runtime field {field} contains a NUL byte")]
    InvalidRuntimeField { field: &'static str },
    #[error("image names a path outside its layout or rootfs: {0}")]
    UnsafePath(String),
    #[error("image path is non-ASCII and not portable across host filesystems: {0}")]
    NonPortableName(String),
    #[error("image path {path:?} aliases {other:?} on case-insensitive hosts")]
    CaseCollision { path: String, other: String },
    #[error("tar: {0}")]
    Tar(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Credentials(#[from] CredentialsError),
}

#[derive(Debug, Clone, Default, serde::Serialize, Deserialize)]
pub struct RuntimeConfig {
    pub entrypoint: Vec<String>,
    pub cmd: Vec<String>,
    pub env: Vec<String>,
    pub working_dir: Option<String>,
    pub user: Option<String>,
}

impl RuntimeConfig {
    pub fn validate(&self) -> Result<(), ImageError> {
        credentials::validate_user_spec(self.user.as_deref())?;
        let mut environment_keys = BTreeSet::new();
        for (index, variable) in self.env.iter().enumerate() {
            let Some((name, _)) = variable.split_once('=') else {
                return Err(ImageError::InvalidEnvironment { index });
            };
            if name.is_empty()
                || !name.bytes().enumerate().all(|(at, byte)| {
                    byte == b'_'
                        || byte.is_ascii_alphabetic() && at == 0
                        || byte.is_ascii_alphanumeric() && at != 0
                })
            {
                return Err(ImageError::InvalidEnvironment { index });
            }
            if !environment_keys.insert(name) {
                return Err(ImageError::DuplicateEnvironmentKey {
                    key: name.to_owned(),
                });
            }
        }
        for argument in self
            .entrypoint
            .iter()
            .chain(self.cmd.iter())
            .chain(self.working_dir.iter())
            .chain(self.user.iter())
            .chain(self.env.iter())
        {
            if argument.contains('\0') {
                return Err(ImageError::InvalidRuntimeField {
                    field: "runtime metadata",
                });
            }
        }
        if let Some(directory) = self.working_dir.as_deref()
            && !directory.is_empty()
            && !absolute_normalized(directory)
        {
            return Err(ImageError::InvalidWorkingDirectory(directory.to_owned()));
        }
        Ok(())
    }

    pub fn resolve_process_credentials(
        &self,
        rootfs: &Path,
    ) -> Result<ProcessCredentials, ImageError> {
        self.validate()?;
        Ok(credentials::resolve(self.user.as_deref(), rootfs)?)
    }
}

fn absolute_normalized(path: &str) -> bool {
    let candidate = Path::new(path);
    if !candidate.is_absolute() {
        return false;
    }
    let mut normalized = PathBuf::from("/");
    for component in candidate.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::CurDir | Component::ParentDir | Component::Prefix(_) => return false,
        }
    }
    normalized.to_str() == Some(path)
}

pub struct StagedImage {
    pub rootfs: PathBuf,
    pub owners: Ownership,
    pub config: RuntimeConfig,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Ownership {
    owners: BTreeMap<PathBuf, Owner>,
    modes: BTreeMap<PathBuf, u32>,
}

impl Ownership {
    #[must_use]
    pub fn owner_of(&self, path: &Path) -> Owner {
        self.owners
            .get(&normalized(path))
            .copied()
            .unwrap_or(Owner::ROOT)
    }

    pub fn record(&mut self, path: &Path, owner: Owner) {
        let path = normalized(path);
        if path.as_os_str().is_empty() {
            return;
        }
        self.owners.insert(path, owner);
    }

    pub fn mode_of(&self, path: &Path, staged_mode: u32) -> u32 {
        self.modes
            .get(&normalized(path))
            .copied()
            .unwrap_or(staged_mode)
    }

    fn record_mode(&mut self, path: &Path, mode: u32) {
        let path = normalized(path);
        if !path.as_os_str().is_empty() {
            self.modes.insert(path, mode);
        }
    }

    fn remove_tree(&mut self, path: &Path) {
        let path = normalized(path);
        self.owners
            .retain(|recorded, _| !recorded.starts_with(&path));
        self.modes
            .retain(|recorded, _| !recorded.starts_with(&path));
    }

    fn remove_below(&mut self, path: &Path) {
        let path = normalized(path);
        self.owners
            .retain(|recorded, _| recorded == &path || !recorded.starts_with(&path));
        self.modes
            .retain(|recorded, _| recorded == &path || !recorded.starts_with(&path));
    }
}

fn normalized(path: &Path) -> PathBuf {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part),
            _ => None,
        })
        .collect()
}

pub fn tool() -> Option<&'static str> {
    ["docker", "podman"]
        .into_iter()
        .find(|tool| Command::new(tool).arg("--version").output().is_ok())
}

pub fn local_image_id(tool: &str, image: &str) -> Option<String> {
    let out = Command::new(tool)
        .args(["image", "inspect", "-f", "{{.Id}}", image])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!id.is_empty()).then_some(id)
}

pub fn stage(image: &str, stage_dir: &Path) -> Result<StagedImage, ImageError> {
    let tarball_path = Path::new(image);
    let explicit_path =
        tarball_path.is_absolute() || image.starts_with("./") || image.starts_with("../");
    let local_candidate = explicit_path || exact::local_candidate(tarball_path)?;
    if local_candidate && !exact::path(tarball_path)? {
        return Err(ImageError::UnsafePath(image.to_string()));
    }
    std::fs::create_dir_all(stage_dir)?;
    let isolated = tempfile::Builder::new()
        .prefix("image-stage-")
        .tempdir_in(stage_dir)?;
    let staged = if local_candidate {
        stage_from_path(tarball_path, isolated.path())
    } else {
        let tarball = export_from_tool(image, isolated.path())?;
        stage_from_path(&tarball, isolated.path())
    }?;
    let _ = isolated.keep();
    Ok(staged)
}

fn export_from_tool(image: &str, stage_dir: &Path) -> Result<PathBuf, ImageError> {
    let tarball = stage_dir.join("image.tar");
    for tool in ["docker", "podman"] {
        if Command::new(tool).arg("--version").output().is_err() {
            continue;
        }
        if local_image_id(tool, image).is_none() {
            let _ = Command::new(tool).args(["pull", image]).status();
        }
        let out = Command::new(tool)
            .args(["image", "save", "-o"])
            .arg(&tarball)
            .arg(image)
            .output()?;
        if out.status.success() {
            return Ok(tarball);
        }
        return Err(ImageError::ExportFailed {
            tool: if tool == "docker" { "docker" } else { "podman" },
            image: image.to_string(),
            detail: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Err(ImageError::NoTool)
}

fn stage_from_path(input: &Path, stage_dir: &Path) -> Result<StagedImage, ImageError> {
    if !exact::path(input)? {
        return Err(ImageError::UnsafePath(input.display().to_string()));
    }
    let layout = if input.is_dir() {
        input.to_path_buf()
    } else {
        let dir = stage_dir.join("layout");
        std::fs::create_dir_all(&dir)?;
        extract_checked(input, &dir)?;
        dir
    };

    let manifest = contained_join(&layout, "manifest.json")
        .ok()
        .is_some_and(|path| path.is_file());
    let index = contained_join(&layout, "index.json")
        .ok()
        .is_some_and(|path| path.is_file());
    let (layers, config_blob) = if manifest {
        docker_save_layers(&layout)?
    } else if index {
        oci_layout_layers(&layout)?
    } else {
        return Err(ImageError::Unrecognized(input.to_path_buf()));
    };

    let config = parse_runtime_config(&config_blob)?;
    let rootfs = stage_dir.join("rootfs");
    std::fs::create_dir_all(&rootfs)?;
    let mut owners = Ownership::default();
    let mut portable_names = BTreeMap::new();
    for layer in &layers {
        apply_layer_tracked(layer, &rootfs, &mut owners, &mut portable_names)?;
    }
    Ok(StagedImage {
        rootfs,
        owners,
        config,
    })
}

#[derive(Deserialize)]
struct DockerSaveManifestEntry {
    #[serde(rename = "Config")]
    config: String,
    #[serde(rename = "Layers")]
    layers: Vec<String>,
}

fn docker_save_layers(layout: &Path) -> Result<(Vec<PathBuf>, Vec<u8>), ImageError> {
    let manifest: Vec<DockerSaveManifestEntry> =
        serde_json::from_slice(&std::fs::read(contained_join(layout, "manifest.json")?)?)?;
    let entry = manifest.into_iter().next().ok_or(ImageError::NoConfig)?;
    let layers = entry
        .layers
        .iter()
        .map(|l| contained_join(layout, l))
        .collect::<Result<_, _>>()?;
    let config = std::fs::read(contained_join(layout, &entry.config)?)?;
    Ok((layers, config))
}

#[derive(Deserialize)]
struct OciIndex {
    manifests: Vec<OciDescriptor>,
}
#[derive(Deserialize)]
struct OciDescriptor {
    digest: String,
}
#[derive(Deserialize)]
struct OciManifest {
    config: OciDescriptor,
    layers: Vec<OciDescriptor>,
}

fn blob_path(layout: &Path, digest: &str) -> Result<PathBuf, ImageError> {
    let relative = digest.replace(':', "/");
    check_relative(Path::new(&relative))?;
    if Path::new(&relative).components().next().is_none() {
        return Err(ImageError::UnsafePath(digest.to_string()));
    }
    contained_join(layout, &format!("blobs/{relative}"))
}

fn oci_layout_layers(layout: &Path) -> Result<(Vec<PathBuf>, Vec<u8>), ImageError> {
    let index: OciIndex =
        serde_json::from_slice(&std::fs::read(contained_join(layout, "index.json")?)?)?;
    let first = index.manifests.first().ok_or(ImageError::NoConfig)?;
    let manifest: OciManifest =
        serde_json::from_slice(&std::fs::read(blob_path(layout, &first.digest)?)?)?;
    let layers = manifest
        .layers
        .iter()
        .map(|l| blob_path(layout, &l.digest))
        .collect::<Result<_, _>>()?;
    let config = std::fs::read(blob_path(layout, &manifest.config.digest)?)?;
    Ok((layers, config))
}

#[derive(Deserialize)]
struct ImageConfigFile {
    config: Option<ImageConfigSection>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ImageConfigSection {
    entrypoint: Option<Vec<String>>,
    cmd: Option<Vec<String>>,
    env: Option<Vec<String>>,
    working_dir: Option<String>,
    user: Option<String>,
}

fn parse_runtime_config(blob: &[u8]) -> Result<RuntimeConfig, ImageError> {
    let file: ImageConfigFile = serde_json::from_slice(blob)?;
    let section = file.config;
    let config = section
        .map(|c| RuntimeConfig {
            entrypoint: c.entrypoint.unwrap_or_default(),
            cmd: c.cmd.unwrap_or_default(),
            env: c.env.unwrap_or_default(),
            working_dir: c.working_dir,
            user: c.user,
        })
        .unwrap_or_default();
    config.validate()?;
    Ok(config)
}

pub fn resolve_process_credentials(
    config: &RuntimeConfig,
    rootfs: &Path,
) -> Result<ProcessCredentials, ImageError> {
    config.resolve_process_credentials(rootfs)
}

fn check_relative(path: &Path) -> Result<(), ImageError> {
    if path.as_os_str().as_bytes().contains(&0) {
        return Err(ImageError::UnsafePath(path.to_string_lossy().into_owned()));
    }
    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(ImageError::UnsafePath(path.to_string_lossy().into_owned()));
            }
        }
    }
    Ok(())
}

fn contained_join(root: &Path, relative: &str) -> Result<PathBuf, ImageError> {
    check_relative(Path::new(relative))?;
    if Path::new(relative).components().next().is_none() {
        return Err(ImageError::UnsafePath(relative.to_string()));
    }
    let real_root = root.canonicalize()?;
    let mut exact_path = real_root.clone();
    let mut pending: VecDeque<OsString> = Path::new(relative)
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_os_string()),
            _ => None,
        })
        .collect();
    let mut symlink_hops = 0;
    while let Some(part) = pending.pop_front() {
        if part == OsStr::new("..") {
            if exact_path == real_root {
                return Err(ImageError::UnsafePath(relative.to_string()));
            }
            exact_path.pop();
            continue;
        }
        let Some(next) = exact::entry(&exact_path, &part)? else {
            return Err(ImageError::Io(std::io::ErrorKind::NotFound.into()));
        };
        if std::fs::symlink_metadata(&next)?.file_type().is_symlink() {
            symlink_hops += 1;
            if symlink_hops > 40 {
                return Err(ImageError::UnsafePath(relative.to_string()));
            }
            let target = std::fs::read_link(&next)?;
            if target.is_absolute() {
                return Err(ImageError::UnsafePath(relative.to_string()));
            }
            let mut target_parts = Vec::new();
            for component in target.components() {
                match component {
                    Component::CurDir => {}
                    Component::ParentDir => target_parts.push(OsString::from("..")),
                    Component::Normal(part) => target_parts.push(part.to_os_string()),
                    Component::RootDir | Component::Prefix(_) => {
                        return Err(ImageError::UnsafePath(relative.to_string()));
                    }
                }
            }
            for part in target_parts.into_iter().rev() {
                pending.push_front(part);
            }
            continue;
        }
        exact_path = next;
    }
    let resolved = resolve_deepest(&exact_path)?;
    if !resolved.starts_with(&real_root) {
        return Err(ImageError::UnsafePath(relative.to_string()));
    }
    Ok(resolved)
}

fn resolve_deepest(path: &Path) -> Result<PathBuf, ImageError> {
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    let mut cursor = path.to_path_buf();
    loop {
        match cursor.canonicalize() {
            Ok(mut resolved) => {
                for part in missing.iter().rev() {
                    resolved.push(part);
                }
                return Ok(resolved);
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = cursor.file_name().map(std::ffi::OsStr::to_os_string) else {
                    return Err(ImageError::Io(err));
                };
                missing.push(name);
                if !cursor.pop() {
                    return Err(ImageError::Io(err));
                }
            }
            Err(err) => return Err(ImageError::Io(err)),
        }
    }
}

fn member_key(name: &Path) -> Vec<u8> {
    let parts: Vec<&[u8]> = name
        .components()
        .filter_map(|c| match c {
            Component::Normal(part) => Some(part.as_bytes()),
            _ => None,
        })
        .collect();
    parts.join(&b'/').to_ascii_lowercase()
}

fn check_members(members: &[Member]) -> Result<(), ImageError> {
    let mut non_directories: BTreeSet<Vec<u8>> = BTreeSet::new();
    let mut keys: Vec<(Vec<u8>, &Member)> = Vec::new();
    for member in members {
        check_relative(member.path())?;
        if member.path().components().any(|component| {
            matches!(component, Component::Normal(part) if part.as_bytes().starts_with(b"._"))
        }) {
            return Err(ImageError::Tar(
                "archive carries an AppleDouble metadata name".into(),
            ));
        }
        let key = member_key(member.path());
        if key.is_empty() {
            continue;
        }
        if !member.is_dir {
            non_directories.insert(key.clone());
        }
        keys.push((key, member));
    }
    for (key, member) in &keys {
        for (at, byte) in key.iter().enumerate() {
            if *byte == b'/' && non_directories.contains(&key[..at]) {
                return Err(ImageError::UnsafePath(member.shown()));
            }
        }
    }
    let mut kinds: BTreeMap<Vec<u8>, u8> = BTreeMap::new();
    for (_, member) in keys {
        let exact = normalized(member.path()).as_os_str().as_bytes().to_vec();
        if let Some(kind) = kinds.insert(exact, member.kind) {
            if kind != member.kind {
                return Err(ImageError::Tar(
                    "archive changes a member's type at the same path".into(),
                ));
            }
            if kind == b'2' {
                return Err(ImageError::Tar(
                    "archive repeats a symlink at the same path".into(),
                ));
            }
        }
    }
    Ok(())
}

fn check_portable_names(
    members: &[Member],
    names: &mut BTreeMap<Vec<u8>, Vec<u8>>,
) -> Result<(), ImageError> {
    for member in members {
        check_portable_path(member.path(), names, true)?;
    }
    Ok(())
}

fn check_portable_path(
    path: &Path,
    names: &mut BTreeMap<Vec<u8>, Vec<u8>>,
    insert: bool,
) -> Result<Vec<u8>, ImageError> {
    check_relative(path)?;
    let mut folded = Vec::new();
    let mut exact = Vec::new();
    for component in path.components() {
        let Component::Normal(part) = component else {
            continue;
        };
        let bytes = part.as_bytes();
        if !bytes.is_ascii() {
            return Err(ImageError::NonPortableName(
                path.to_string_lossy().into_owned(),
            ));
        }
        if !folded.is_empty() {
            folded.push(b'/');
            exact.push(b'/');
        }
        folded.extend(bytes.to_ascii_lowercase());
        exact.extend_from_slice(bytes);
        if let Some(other) = names.get(&folded) {
            if other != &exact {
                return Err(ImageError::CaseCollision {
                    path: path.to_string_lossy().into_owned(),
                    other: String::from_utf8_lossy(other).into_owned(),
                });
            }
        } else if insert {
            names.insert(folded.clone(), exact.clone());
        }
    }
    Ok(folded)
}

fn remove_portable_tree(names: &mut BTreeMap<Vec<u8>, Vec<u8>>, key: &[u8], include_root: bool) {
    let mut prefix = key.to_vec();
    prefix.push(b'/');
    names.retain(|candidate, _| {
        !((include_root && candidate == key) || key.is_empty() || candidate.starts_with(&prefix))
    });
}

fn check_layer_portable_names(
    members: &[Member],
    names: &mut BTreeMap<Vec<u8>, Vec<u8>>,
) -> Result<(), ImageError> {
    for member in members {
        match whiteout(member.path()) {
            Some(Whiteout::Opaque(directory)) => {
                check_portable_path(member.path(), names, false)?;
                check_portable_path(directory, names, false)?;
            }
            Some(Whiteout::Entry(target)) => {
                check_portable_path(member.path(), names, false)?;
                check_portable_path(&target, names, false)?;
            }
            None => {}
        }
    }
    for member in members {
        match whiteout(member.path()) {
            Some(Whiteout::Opaque(directory)) => {
                let key = member_key(directory);
                remove_portable_tree(names, &key, false);
            }
            Some(Whiteout::Entry(target)) => {
                let key = member_key(&target);
                remove_portable_tree(names, &key, true);
            }
            None => {}
        }
    }
    for member in members {
        if whiteout(member.path()).is_some() {
            continue;
        }
        let key = check_portable_path(member.path(), names, true)?;
        if !member.is_dir {
            remove_portable_tree(names, &key, false);
        }
    }
    for member in members {
        if whiteout(member.path()).is_some() {
            check_portable_path(member.path(), names, false)?;
        }
    }
    Ok(())
}

fn check_hardlinks(members: &[Member]) -> Result<(), ImageError> {
    let mut seen = BTreeSet::new();
    for member in members {
        if let Some(target) = &member.hardlink_target {
            let path = Path::new(std::ffi::OsStr::from_bytes(target));
            check_relative(path)?;
            if !target.is_ascii() {
                return Err(ImageError::NonPortableName(
                    path.to_string_lossy().into_owned(),
                ));
            }
            let exact = normalized(path);
            if exact.as_os_str().is_empty() || !seen.contains(&exact) {
                return Err(ImageError::UnsafePath(format!(
                    "hardlink {} has no earlier exact target {}",
                    member.shown(),
                    path.display()
                )));
            }
        }
        seen.insert(normalized(member.path()));
    }
    Ok(())
}

const TAR_BLOCK: usize = 512;
const MAX_MEMBERS: usize = 1 << 20;
const MAX_HEADER_DATA: u64 = 1 << 16;

struct Member {
    name: Vec<u8>,
    kind: u8,
    is_dir: bool,
    mode: u32,
    owner: Owner,
    hardlink_target: Option<Vec<u8>>,
}

impl Member {
    fn path(&self) -> &Path {
        Path::new(std::ffi::OsStr::from_bytes(&self.name))
    }

    fn shown(&self) -> String {
        self.path().to_string_lossy().into_owned()
    }
}

struct ArchiveReader {
    reader: Box<dyn Read>,
    child: Option<std::process::Child>,
}

impl Read for ArchiveReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.reader.read(buf)
    }
}

impl Drop for ArchiveReader {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        self.reader = Box::new(std::io::empty());
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn open_archive(archive: &Path) -> Result<ArchiveReader, ImageError> {
    let file = std::fs::File::open(archive)?;
    let mut magic = Vec::with_capacity(6);
    file.take(6).read_to_end(&mut magic)?;
    let tool = if magic.starts_with(&[0x1f, 0x8b]) {
        "gzip"
    } else if magic.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        "zstd"
    } else if magic.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0x00]) {
        "xz"
    } else if magic.starts_with(b"BZh") {
        "bzip2"
    } else {
        return Ok(ArchiveReader {
            reader: Box::new(std::io::BufReader::with_capacity(
                64 * 1024,
                std::fs::File::open(archive)?,
            )),
            child: None,
        });
    };
    let mut child = Command::new(tool)
        .arg("-dc")
        .arg(archive)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| ImageError::Tar(format!("{tool} is needed to read this layer: {err}")))?;
    let stdout = child.stdout.take().ok_or_else(|| {
        ImageError::Tar(format!(
            "{tool} produced no output for {}",
            archive.display()
        ))
    })?;
    Ok(ArchiveReader {
        reader: Box::new(std::io::BufReader::with_capacity(64 * 1024, stdout)),
        child: Some(child),
    })
}

fn read_members(archive: &Path) -> Result<Vec<Member>, ImageError> {
    let mut stream = open_archive(archive)?;
    let mut members: Vec<Member> = Vec::new();
    let mut long_name: Option<Vec<u8>> = None;
    let mut long_link: Option<Vec<u8>> = None;
    let mut pax = Pax::default();
    let mut pax_header_seen = false;
    let mut block = [0u8; TAR_BLOCK];
    while read_block(&mut stream, &mut block)? {
        if block.iter().all(|byte| *byte == 0) {
            require_trailing_zeros(&mut stream)?;
            return Ok(members);
        }
        verify_checksum(&block)?;
        let size = parse_octal(&block[124..136])?;
        match block[156] {
            b'L' => {
                let mut data = header_data(&mut stream, size)?;
                data.truncate(
                    data.iter()
                        .position(|byte| *byte == 0)
                        .unwrap_or(data.len()),
                );
                long_name = Some(data);
            }
            b'K' => {
                let mut data = header_data(&mut stream, size)?;
                data.truncate(
                    data.iter()
                        .position(|byte| *byte == 0)
                        .unwrap_or(data.len()),
                );
                long_link = Some(data);
            }
            b'x' => {
                if pax_header_seen {
                    return Err(ImageError::Tar("member has multiple pax headers".into()));
                }
                pax_header_seen = true;
                let data = header_data(&mut stream, size)?;
                parse_pax(&data, &mut pax)?;
            }
            b'g' => {
                return Err(ImageError::Tar(
                    "archive carries a global pax header".into(),
                ));
            }
            b'S' | b'M' => {
                return Err(ImageError::Tar(
                    "archive carries an unsupported member type".into(),
                ));
            }
            typeflag => {
                pax_header_seen = false;
                if !matches!(typeflag, 0 | b'0' | b'1' | b'2' | b'5') {
                    return Err(ImageError::Tar(
                        "archive carries an unsupported member type".into(),
                    ));
                }
                if members.len() == MAX_MEMBERS {
                    return Err(ImageError::Tar(format!(
                        "archive has more than {MAX_MEMBERS} members"
                    )));
                }
                let Pax {
                    path,
                    size: pax_size,
                    uid,
                    gid,
                    link_path,
                } = std::mem::take(&mut pax);
                let no_data_kind = match typeflag {
                    b'1' => Some("hardlink"),
                    b'2' => Some("symlink"),
                    b'5' => Some("directory"),
                    _ => None,
                };
                if let Some(kind) = no_data_kind
                    && (size != 0 || pax_size.is_some_and(|size| size != 0))
                {
                    return Err(ImageError::Tar(format!(
                        "{kind} member has nonzero data size"
                    )));
                }
                let gnu_name = long_name.take();
                let gnu_link = long_link.take();
                if path.is_some() && gnu_name.is_some() {
                    return Err(ImageError::Tar(
                        "member has conflicting GNU and pax name overrides".into(),
                    ));
                }
                if link_path.is_some() && gnu_link.is_some() {
                    return Err(ImageError::Tar(
                        "member has conflicting GNU and pax link overrides".into(),
                    ));
                }
                let owner = Owner {
                    uid: owner_id(uid, &block[108..116])?,
                    gid: owner_id(gid, &block[116..124])?,
                };
                let name = path.or(gnu_name).unwrap_or_else(|| ustar_name(&block));
                if name.is_empty() {
                    return Err(ImageError::Tar("archive member has an empty name".into()));
                }
                if typeflag != b'5' && name.ends_with(b"/") {
                    return Err(ImageError::Tar(
                        "non-directory member has a trailing slash".into(),
                    ));
                }
                members.push(Member {
                    name,
                    kind: if typeflag == 0 { b'0' } else { typeflag },
                    is_dir: typeflag == b'5',
                    mode: (parse_octal(&block[100..108])? & 0o7777) as u32,
                    owner,
                    hardlink_target: (typeflag == b'1').then(|| {
                        link_path
                            .or(gnu_link)
                            .unwrap_or_else(|| tar_field(&block[157..257]))
                    }),
                });
                let size = pax_size.unwrap_or(size);
                let data = size
                    .checked_add(padding(size))
                    .ok_or_else(|| ImageError::Tar("tar member size is too large".into()))?;
                skip(&mut stream, data)?;
            }
        }
    }
    Err(ImageError::Tar("archive ends without a terminator".into()))
}

fn read_block(stream: &mut impl Read, block: &mut [u8; TAR_BLOCK]) -> Result<bool, ImageError> {
    if stream.read(&mut block[..1])? == 0 {
        return Ok(false);
    }
    stream
        .read_exact(&mut block[1..])
        .map_err(|e| ImageError::Tar(format!("archive ends inside a header: {e}")))?;
    Ok(true)
}

fn padding(size: u64) -> u64 {
    let block = TAR_BLOCK as u64;
    (block - size % block) % block
}

fn skip(stream: &mut impl Read, bytes: u64) -> Result<(), ImageError> {
    let skipped = std::io::copy(&mut stream.take(bytes), &mut std::io::sink())?;
    if skipped == bytes {
        Ok(())
    } else {
        Err(ImageError::Tar("archive ends inside a member".into()))
    }
}

fn header_data(stream: &mut impl Read, size: u64) -> Result<Vec<u8>, ImageError> {
    if size > MAX_HEADER_DATA {
        return Err(ImageError::Tar(format!(
            "archive has a {size}-byte name header"
        )));
    }
    let mut data = vec![0u8; size as usize];
    stream.read_exact(&mut data)?;
    skip(stream, padding(size))?;
    Ok(data)
}

fn require_trailing_zeros(stream: &mut impl Read) -> Result<(), ImageError> {
    let mut buffer = [0u8; 8192];
    loop {
        match stream.read(&mut buffer)? {
            0 => return Ok(()),
            read if buffer[..read].iter().all(|byte| *byte == 0) => {}
            _ => {
                return Err(ImageError::Tar(
                    "archive carries data after its terminator".into(),
                ));
            }
        }
    }
}

fn verify_checksum(block: &[u8; TAR_BLOCK]) -> Result<(), ImageError> {
    let stored = parse_octal(&block[148..156])?;
    let sum: u32 = block
        .iter()
        .enumerate()
        .map(|(at, byte)| {
            u32::from(if (148..156).contains(&at) {
                b' '
            } else {
                *byte
            })
        })
        .sum();
    if u64::from(sum) == stored {
        Ok(())
    } else {
        Err(ImageError::Tar("tar header checksum mismatch".into()))
    }
}

fn parse_octal(field: &[u8]) -> Result<u64, ImageError> {
    if let Some(first) = field.first()
        && first & 0x80 != 0
    {
        if *first != 0x80 {
            return Err(ImageError::Tar("negative tar header field".into()));
        }
        return field[1..].iter().try_fold(0u64, |value, byte| {
            value
                .checked_mul(256)
                .and_then(|value| value.checked_add(u64::from(*byte)))
                .ok_or_else(|| ImageError::Tar("tar header field is too large".into()))
        });
    }
    let digits = field.split(|byte| *byte == 0).next().unwrap_or_default();
    let text = std::str::from_utf8(digits)
        .map_err(|_| ImageError::Tar("tar header field is not octal".into()))?
        .trim();
    if text.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(text, 8)
        .map_err(|_| ImageError::Tar("tar header field is not octal".into()))
}

fn owner_id(pax: Option<u64>, field: &[u8]) -> Result<u32, ImageError> {
    let id = match pax {
        Some(id) => id,
        None => parse_octal(field)?,
    };
    u32::try_from(id).map_err(|_| ImageError::Tar("tar owner id is too large".into()))
}

fn ustar_name(block: &[u8; TAR_BLOCK]) -> Vec<u8> {
    let name = tar_field(&block[0..100]);
    if &block[257..263] != b"ustar\0" {
        return name;
    }
    let prefix = tar_field(&block[345..500]);
    if prefix.is_empty() {
        return name;
    }
    [prefix, vec![b'/'], name].concat()
}

fn tar_field(bytes: &[u8]) -> Vec<u8> {
    bytes
        .split(|byte| *byte == 0)
        .next()
        .unwrap_or_default()
        .to_vec()
}

#[derive(Default)]
struct Pax {
    path: Option<Vec<u8>>,
    size: Option<u64>,
    uid: Option<u64>,
    gid: Option<u64>,
    link_path: Option<Vec<u8>>,
}

fn parse_pax(records: &[u8], pax: &mut Pax) -> Result<(), ImageError> {
    let malformed = || ImageError::Tar("malformed pax header".into());
    let mut rest = records;
    while !rest.is_empty() {
        let space = rest
            .iter()
            .position(|byte| *byte == b' ')
            .ok_or_else(malformed)?;
        let length: usize = std::str::from_utf8(&rest[..space])
            .map_err(|_| malformed())?
            .parse()
            .map_err(|_| malformed())?;
        if length <= space || length > rest.len() {
            return Err(malformed());
        }
        let record = &rest[space + 1..length];
        let equals = record
            .iter()
            .position(|byte| *byte == b'=')
            .ok_or_else(malformed)?;
        let (key, value) = record.split_at(equals);
        let value = &value[1..];
        let value = value.strip_suffix(b"\n").unwrap_or(value);
        match key {
            b"path" => pax.path = Some(value.to_vec()),
            b"linkpath" => pax.link_path = Some(value.to_vec()),
            b"size" => {
                let size = std::str::from_utf8(value)
                    .ok()
                    .and_then(|text| text.parse().ok())
                    .ok_or_else(|| ImageError::Tar("unreadable pax size record".into()))?;
                pax.size = Some(size);
            }
            b"uid" | b"gid" => {
                let id = std::str::from_utf8(value)
                    .ok()
                    .and_then(|text| text.parse().ok())
                    .ok_or_else(|| ImageError::Tar("unreadable pax owner record".into()))?;
                if key == b"uid" {
                    pax.uid = Some(id);
                } else {
                    pax.gid = Some(id);
                }
            }
            b"SCHILY.fflags" => {
                return Err(ImageError::Tar(
                    "archive carries platform-specific file flags".into(),
                ));
            }
            key if key.starts_with(b"SCHILY.acl.") => {
                return Err(ImageError::Tar(
                    "archive carries platform-specific ACL metadata".into(),
                ));
            }
            key if key.starts_with(b"GNU.sparse.") => {
                return Err(ImageError::Tar(
                    "archive uses an unsupported sparse pax record".into(),
                ));
            }
            _ => {}
        }
        rest = &rest[length..];
    }
    Ok(())
}

fn extract_checked(archive: &Path, dest: &Path) -> Result<(), ImageError> {
    let members = read_members(archive)?;
    check_members(&members)?;
    check_portable_names(&members, &mut BTreeMap::new())?;
    check_hardlinks(&members)?;
    untar(archive, dest)
}

fn writable_layer_mode(mode: u32) -> u32 {
    mode | 0o700
}

fn merge_layer(staged: &Path, rootfs: &Path) -> Result<(), ImageError> {
    let mut entries: Vec<_> = std::fs::read_dir(staged)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with(".wh.") {
            continue;
        }
        let source = staged.join(&name);
        let target = rootfs.join(&name);
        let meta = std::fs::symlink_metadata(&source)?;
        let existing = std::fs::symlink_metadata(&target).ok();
        if meta.is_dir() {
            match &existing {
                Some(found) if found.is_symlink() => {
                    return Err(ImageError::UnsafePath(target.display().to_string()));
                }
                Some(found) if found.is_dir() => {}
                Some(_) => {
                    std::fs::remove_file(&target)?;
                    std::fs::create_dir(&target)?;
                }
                None => std::fs::create_dir(&target)?,
            }
            let mode = meta.permissions().mode() & 0o7777;
            let open = std::fs::Permissions::from_mode(writable_layer_mode(mode));
            std::fs::set_permissions(&source, open.clone())?;
            std::fs::set_permissions(&target, open)?;
            merge_layer(&source, &target)?;
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode))?;
        } else {
            match &existing {
                Some(found) if found.is_dir() => std::fs::remove_dir_all(&target)?,
                Some(_) => std::fs::remove_file(&target)?,
                None => {}
            }
            std::fs::rename(&source, &target)?;
        }
    }
    Ok(())
}

fn resolve_under(rootfs: &Path, relative: &Path) -> Result<Option<PathBuf>, ImageError> {
    let Ok(mut cursor) = rootfs.canonicalize() else {
        return Ok(None);
    };
    let parts: Vec<_> = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part),
            _ => None,
        })
        .collect();
    for (at, part) in parts.iter().enumerate() {
        let next = match exact::entry(&cursor, part) {
            Ok(Some(next)) => next,
            Ok(None) => return Ok(None),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        if at + 1 < parts.len() && std::fs::symlink_metadata(&next)?.file_type().is_symlink() {
            return Err(ImageError::UnsafePath(relative.display().to_string()));
        }
        cursor = next;
    }
    Ok(Some(cursor))
}

fn untar(tarball: &Path, dest: &Path) -> Result<(), ImageError> {
    let out = Command::new("sh")
        .arg("-c")
        .arg("umask 022; exec tar \"$@\"")
        .arg("tar")
        .arg("-xpf")
        .arg(tarball)
        .arg("--no-same-owner")
        .arg("-C")
        .arg(dest)
        .output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(ImageError::Tar(
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ))
    }
}

#[cfg(test)]
fn apply_layer(layer: &Path, rootfs: &Path, owners: &mut Ownership) -> Result<(), ImageError> {
    apply_layer_tracked(layer, rootfs, owners, &mut BTreeMap::new())
}

fn apply_layer_tracked(
    layer: &Path,
    rootfs: &Path,
    owners: &mut Ownership,
    portable_names: &mut BTreeMap<Vec<u8>, Vec<u8>>,
) -> Result<(), ImageError> {
    let members = read_members(layer)?;
    check_members(&members)?;
    check_layer_portable_names(&members, portable_names)?;
    check_hardlinks(&members)?;
    apply_whiteouts(&members, rootfs)?;
    let staging = rootfs.parent().unwrap_or(rootfs);
    let staged = tempfile::tempdir_in(staging)?;
    untar(layer, staged.path())?;
    merge_layer(staged.path(), rootfs)?;
    record_owners(&members, owners);
    Ok(())
}

fn record_owners(members: &[Member], owners: &mut Ownership) {
    let explicit_dirs: BTreeSet<PathBuf> = members
        .iter()
        .filter(|member| member.is_dir && whiteout(member.path()).is_none())
        .map(|member| normalized(member.path()))
        .collect();
    for member in members {
        let path = member.path();
        match whiteout(path) {
            Some(Whiteout::Opaque(dir)) => owners.remove_below(dir),
            Some(Whiteout::Entry(target)) => owners.remove_tree(&target),
            None => {}
        }
    }
    for member in members {
        let path = normalized(member.path());
        let mut parent = path.parent();
        while let Some(dir) = parent.filter(|dir| !dir.as_os_str().is_empty()) {
            if !explicit_dirs.contains(dir) {
                owners.record_mode(dir, 0o755);
            }
            parent = dir.parent();
        }
    }
    for member in members {
        let path = member.path();
        if whiteout(path).is_some() {
            continue;
        }
        let (owner, mode) =
            member
                .hardlink_target
                .as_ref()
                .map_or((member.owner, member.mode), |target| {
                    let target = Path::new(OsStr::from_bytes(target));
                    (owners.owner_of(target), owners.mode_of(target, member.mode))
                });
        if !member.is_dir {
            owners.remove_tree(path);
        }
        owners.record(path, owner);
        owners.record_mode(path, mode);
    }
}

enum Whiteout<'a> {
    Opaque(&'a Path),
    Entry(PathBuf),
}

fn whiteout(path: &Path) -> Option<Whiteout<'_>> {
    let name = path.file_name()?.to_str()?;
    if name == ".wh..wh..opq" {
        Some(Whiteout::Opaque(path.parent().unwrap_or(Path::new(""))))
    } else {
        name.strip_prefix(".wh.")
            .map(|hidden| Whiteout::Entry(path.with_file_name(hidden)))
    }
}

fn apply_whiteouts(members: &[Member], rootfs: &Path) -> Result<(), ImageError> {
    for member in members {
        let path = member.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name == ".wh..wh..opq" {
            let parent = path.parent().unwrap_or(Path::new(""));
            let Some(dir) = resolve_under(rootfs, parent)? else {
                continue;
            };
            let Ok(meta) = std::fs::symlink_metadata(&dir) else {
                continue;
            };
            if meta.is_symlink() {
                return Err(ImageError::UnsafePath(member.shown()));
            }
            if meta.is_dir() {
                std::fs::remove_dir_all(&dir)?;
                std::fs::create_dir_all(&dir)?;
            }
        } else if let Some(hidden) = name.strip_prefix(".wh.") {
            let Some(target) = resolve_under(rootfs, &path.with_file_name(hidden))? else {
                continue;
            };
            let Ok(meta) = std::fs::symlink_metadata(&target) else {
                continue;
            };
            if meta.is_dir() {
                std::fs::remove_dir_all(&target)?;
            } else {
                std::fs::remove_file(&target)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_credential_resolver_preserves_explicit_ids() {
        let rootfs = tempfile::tempdir().unwrap();
        let config = RuntimeConfig {
            user: Some("123:456".into()),
            ..RuntimeConfig::default()
        };
        assert_eq!(
            resolve_process_credentials(&config, rootfs.path()).unwrap(),
            ProcessCredentials {
                uid: 123,
                gid: 456,
                additional_gids: Vec::new()
            }
        );
    }

    #[test]
    fn runtime_config_parses_the_config_section() {
        let blob = br#"{"config":{"Entrypoint":["/e"],"Cmd":["run"],
            "Env":["A=1"],"WorkingDir":"/w","User":"worker:shared"},"rootfs":{}}"#;
        let config = parse_runtime_config(blob).unwrap();
        assert_eq!(config.entrypoint, ["/e"]);
        assert_eq!(config.cmd, ["run"]);
        assert_eq!(config.env, ["A=1"]);
        assert_eq!(config.working_dir.as_deref(), Some("/w"));
        assert_eq!(config.user.as_deref(), Some("worker:shared"));

        let empty = parse_runtime_config(b"{}").unwrap();
        assert!(empty.entrypoint.is_empty() && empty.cmd.is_empty());
    }

    #[test]
    fn runtime_config_rejects_environment_entries_the_launcher_would_drop() {
        for env in [
            vec!["MISSING_EQUALS".to_string()],
            vec!["1NAME=value".to_string()],
        ] {
            let blob = serde_json::json!({"config": {"Env": env}});
            assert!(matches!(
                parse_runtime_config(&serde_json::to_vec(&blob).unwrap()),
                Err(ImageError::InvalidEnvironment { .. })
            ));
        }
    }

    #[test]
    fn runtime_config_rejects_duplicate_environment_and_unnormalized_cwd() {
        let duplicate = serde_json::json!({
            "config": {"Env": ["A=one", "A=two"]}
        });
        assert!(matches!(
            parse_runtime_config(&serde_json::to_vec(&duplicate).unwrap()),
            Err(ImageError::DuplicateEnvironmentKey { .. })
        ));
        for directory in ["relative", "/a/../b", "/a//b", "/a/"] {
            let blob = serde_json::json!({"config": {"WorkingDir": directory}});
            assert!(matches!(
                parse_runtime_config(&serde_json::to_vec(&blob).unwrap()),
                Err(ImageError::InvalidWorkingDirectory(_)),
            ));
        }
    }

    #[test]
    fn blob_path_splits_the_digest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("blobs/sha256")).unwrap();
        std::fs::write(dir.path().join("blobs/sha256/abc"), b"blob").unwrap();
        assert_eq!(
            blob_path(dir.path(), "sha256:abc").unwrap(),
            dir.path().canonicalize().unwrap().join("blobs/sha256/abc")
        );
    }

    #[test]
    fn blob_path_refuses_a_symlinked_layout() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside/sha256");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("m"), b"secret").unwrap();

        let via_dir = dir.path().join("via-dir");
        std::fs::create_dir_all(&via_dir).unwrap();
        std::os::unix::fs::symlink(dir.path().join("outside"), via_dir.join("blobs")).unwrap();
        let err = blob_path(&via_dir, "sha256:m").unwrap_err();
        assert!(matches!(err, ImageError::UnsafePath(_)), "{err:?}");

        let via_blob = dir.path().join("via-blob");
        std::fs::create_dir_all(via_blob.join("blobs/sha256")).unwrap();
        std::os::unix::fs::symlink(outside.join("m"), via_blob.join("blobs/sha256/m")).unwrap();
        let err = blob_path(&via_blob, "sha256:m").unwrap_err();
        assert!(matches!(err, ImageError::UnsafePath(_)), "{err:?}");
    }

    #[test]
    fn oci_layout_refuses_a_symlinked_index_or_blob() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret"), br#"{"manifests":[]}"#).unwrap();

        let layout = dir.path().join("layout");
        std::fs::create_dir_all(layout.join("blobs/sha256")).unwrap();
        std::os::unix::fs::symlink(outside.join("secret"), layout.join("index.json")).unwrap();
        let err = oci_layout_layers(&layout).unwrap_err();
        assert!(matches!(err, ImageError::UnsafePath(_)), "{err:?}");

        std::fs::remove_file(layout.join("index.json")).unwrap();
        std::fs::write(
            layout.join("index.json"),
            br#"{"manifests":[{"digest":"sha256:m"}]}"#,
        )
        .unwrap();
        std::os::unix::fs::symlink(outside.join("secret"), layout.join("blobs/sha256/m")).unwrap();
        let err = oci_layout_layers(&layout).unwrap_err();
        assert!(matches!(err, ImageError::UnsafePath(_)), "{err:?}");
    }

    #[test]
    fn docker_save_refuses_symlinked_references() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret"), b"[]").unwrap();

        let layout = dir.path().join("layout");
        std::fs::create_dir_all(&layout).unwrap();
        std::os::unix::fs::symlink(outside.join("secret"), layout.join("manifest.json")).unwrap();
        let err = docker_save_layers(&layout).unwrap_err();
        assert!(matches!(err, ImageError::UnsafePath(_)), "{err:?}");

        std::fs::remove_file(layout.join("manifest.json")).unwrap();
        std::fs::write(
            layout.join("manifest.json"),
            br#"[{"Config":"c.json","Layers":["l1.tar"]}]"#,
        )
        .unwrap();
        std::fs::write(layout.join("c.json"), b"{}").unwrap();
        std::os::unix::fs::symlink(outside.join("secret"), layout.join("l1.tar")).unwrap();
        let err = docker_save_layers(&layout).unwrap_err();
        assert!(matches!(err, ImageError::UnsafePath(_)), "{err:?}");
    }

    #[test]
    fn blob_path_refuses_a_traversing_digest() {
        for digest in ["sha256:../../etc/passwd", "..:..", "/etc:passwd", ""] {
            assert!(
                matches!(
                    blob_path(Path::new("/l"), digest),
                    Err(ImageError::UnsafePath(_))
                ),
                "{digest} was accepted"
            );
        }
    }

    #[test]
    fn docker_save_manifest_refuses_traversing_references() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("manifest.json"),
            br#"[{"Config":"c.json","Layers":["../../outside.tar"]}]"#,
        )
        .unwrap();
        assert!(matches!(
            docker_save_layers(dir.path()),
            Err(ImageError::UnsafePath(_))
        ));

        std::fs::write(
            dir.path().join("manifest.json"),
            br#"[{"Config":"/etc/passwd","Layers":[]}]"#,
        )
        .unwrap();
        assert!(matches!(
            docker_save_layers(dir.path()),
            Err(ImageError::UnsafePath(_))
        ));
    }

    #[test]
    fn docker_save_manifest_lists_layers_in_order() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("manifest.json"),
            br#"[{"Config":"c.json","Layers":["l1.tar","l2.tar"]}]"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("c.json"), b"{\"config\":{}}").unwrap();
        std::fs::write(dir.path().join("l1.tar"), b"").unwrap();
        std::fs::write(dir.path().join("l2.tar"), b"").unwrap();
        let (layers, config) = docker_save_layers(dir.path()).unwrap();
        let real = dir.path().canonicalize().unwrap();
        assert_eq!(layers, [real.join("l1.tar"), real.join("l2.tar")]);
        assert_eq!(config, b"{\"config\":{}}");
    }

    #[test]
    fn oci_layout_resolves_blobs_through_the_index() {
        let dir = tempfile::tempdir().unwrap();
        let blobs = dir.path().join("blobs/sha256");
        std::fs::create_dir_all(&blobs).unwrap();
        std::fs::write(
            dir.path().join("index.json"),
            br#"{"manifests":[{"digest":"sha256:m"}]}"#,
        )
        .unwrap();
        std::fs::write(
            blobs.join("m"),
            br#"{"config":{"digest":"sha256:c"},"layers":[{"digest":"sha256:l"}]}"#,
        )
        .unwrap();
        std::fs::write(blobs.join("c"), b"cfg").unwrap();
        std::fs::write(blobs.join("l"), b"").unwrap();
        let (layers, config) = oci_layout_layers(dir.path()).unwrap();
        assert_eq!(layers, [blobs.canonicalize().unwrap().join("l")]);
        assert_eq!(config, b"cfg");
    }

    fn tar_layer(dir: &Path, name: &str, files: &[(&str, &[u8])]) -> PathBuf {
        let stage = dir.join(format!("{name}-stage"));
        std::fs::create_dir_all(&stage).unwrap();
        let mut args: Vec<String> = Vec::new();
        for (path, data) in files {
            let file = stage.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, data).unwrap();
            args.push(path.to_string());
        }
        let tarball = dir.join(format!("{name}.tar"));
        let status = Command::new("tar")
            .env("COPYFILE_DISABLE", "1")
            .arg("-cf")
            .arg(&tarball)
            .arg("-C")
            .arg(&stage)
            .args(&args)
            .status()
            .unwrap();
        assert!(status.success());
        tarball
    }

    #[test]
    fn staging_a_second_image_in_the_same_directory_has_no_prior_files() {
        let dir = tempfile::tempdir().unwrap();
        let layout = |name: &str, file: &str, data: &[u8]| {
            let layout = dir.path().join(name);
            std::fs::create_dir_all(&layout).unwrap();
            std::fs::write(
                layout.join("manifest.json"),
                br#"[{"Config":"config.json","Layers":["layer.tar"]}]"#,
            )
            .unwrap();
            std::fs::write(layout.join("config.json"), br#"{"config":{}}"#).unwrap();
            let layer = tar_layer(dir.path(), name, &[(file, data)]);
            std::fs::copy(layer, layout.join("layer.tar")).unwrap();
            layout
        };
        let first = layout("first", "old", b"old");
        let second = layout("second", "new", b"new");
        let reused = dir.path().join("reused");
        std::fs::create_dir_all(&reused).unwrap();

        let original = stage(first.to_str().unwrap(), &reused).unwrap();
        assert_eq!(std::fs::read(original.rootfs.join("old")).unwrap(), b"old");
        let after_reuse = stage(second.to_str().unwrap(), &reused).unwrap();
        let fresh_dir = dir.path().join("fresh");
        std::fs::create_dir_all(&fresh_dir).unwrap();
        let fresh = stage(second.to_str().unwrap(), &fresh_dir).unwrap();

        assert_ne!(original.rootfs, after_reuse.rootfs);
        assert_eq!(std::fs::read(original.rootfs.join("old")).unwrap(), b"old");
        assert!(!after_reuse.rootfs.join("old").exists());
        assert_eq!(
            std::fs::read(after_reuse.rootfs.join("new")).unwrap(),
            b"new"
        );
        assert_eq!(
            super::super::bundle::build_rootfs_segment(&after_reuse.rootfs, &after_reuse.owners)
                .unwrap(),
            super::super::bundle::build_rootfs_segment(&fresh.rootfs, &fresh.owners).unwrap()
        );
    }

    #[test]
    fn staging_an_oci_archive_after_a_docker_archive_uses_only_the_oci_layout() {
        let dir = tempfile::tempdir().unwrap();
        let docker = dir.path().join("docker");
        std::fs::create_dir_all(&docker).unwrap();
        std::fs::write(
            docker.join("manifest.json"),
            br#"[{"Config":"config.json","Layers":["layer.tar"]}]"#,
        )
        .unwrap();
        std::fs::write(
            docker.join("config.json"),
            br#"{"config":{"Cmd":["/old"]}}"#,
        )
        .unwrap();
        let old_layer = tar_layer(dir.path(), "old-layer", &[("old", b"old")]);
        std::fs::copy(old_layer, docker.join("layer.tar")).unwrap();

        let oci = dir.path().join("oci");
        let blobs = oci.join("blobs/sha256");
        std::fs::create_dir_all(&blobs).unwrap();
        std::fs::write(
            oci.join("index.json"),
            br#"{"manifests":[{"digest":"sha256:m"}]}"#,
        )
        .unwrap();
        std::fs::write(
            blobs.join("m"),
            br#"{"config":{"digest":"sha256:c"},"layers":[{"digest":"sha256:l"}]}"#,
        )
        .unwrap();
        std::fs::write(blobs.join("c"), br#"{"config":{"Cmd":["/new"]}}"#).unwrap();
        let new_layer = tar_layer(dir.path(), "new-layer", &[("new", b"new")]);
        std::fs::copy(new_layer, blobs.join("l")).unwrap();

        let archive = |layout: &Path, name: &str, entries: &[&str]| {
            let path = dir.path().join(name);
            let status = Command::new("tar")
                .env("COPYFILE_DISABLE", "1")
                .arg("-cf")
                .arg(&path)
                .arg("-C")
                .arg(layout)
                .args(entries)
                .status()
                .unwrap();
            assert!(status.success());
            path
        };
        let docker_archive = archive(
            &docker,
            "docker-archive.tar",
            &["manifest.json", "config.json", "layer.tar"],
        );
        let oci_archive = archive(&oci, "oci-archive.tar", &["index.json", "blobs"]);
        let reused = dir.path().join("reused");
        let original = stage(docker_archive.to_str().unwrap(), &reused).unwrap();
        assert_eq!(original.config.cmd, ["/old"]);

        let after_reuse = stage(oci_archive.to_str().unwrap(), &reused).unwrap();
        assert_eq!(after_reuse.config.cmd, ["/new"]);
        assert_eq!(
            std::fs::read(after_reuse.rootfs.join("new")).unwrap(),
            b"new"
        );
        assert!(!after_reuse.rootfs.join("old").exists());
    }

    #[test]
    fn extract_checked_extracts_into_dest() {
        let dir = tempfile::tempdir().unwrap();
        let tarball = tar_layer(dir.path(), "t", &[("hello.txt", b"hi")]);
        let dest = dir.path().join("out");
        std::fs::create_dir_all(&dest).unwrap();
        extract_checked(&tarball, &dest).unwrap();
        assert_eq!(std::fs::read(dest.join("hello.txt")).unwrap(), b"hi");
        assert!(extract_checked(Path::new("/nonexistent.tar"), &dest).is_err());
    }

    #[test]
    fn staging_refuses_a_traversing_outer_tarball() {
        let dir = tempfile::tempdir().unwrap();
        let (_, victim) = planted(dir.path());
        let tarball = tar_with_raw_name(dir.path(), "image.tar", "../victim/pwn");
        let stage_dir = dir.path().join("stage");
        std::fs::create_dir_all(&stage_dir).unwrap();
        let staged = stage_from_path(&tarball, &stage_dir);
        assert!(matches!(staged, Err(ImageError::UnsafePath(_))));
        assert!(!victim.join("pwn").exists());
    }

    fn raw_member(name: &[u8], typeflag: u8, linkname: &str, data: &[u8]) -> Vec<u8> {
        raw_member_owned(name, typeflag, linkname, data, Owner::ROOT)
    }

    fn raw_member_owned(
        name: &[u8],
        typeflag: u8,
        linkname: &str,
        data: &[u8],
        owner: Owner,
    ) -> Vec<u8> {
        let mode = if typeflag == b'5' { 0o755 } else { 0o644 };
        raw_member_with_mode(name, typeflag, linkname, data, owner, mode)
    }

    fn raw_member_with_mode(
        name: &[u8],
        typeflag: u8,
        linkname: &str,
        data: &[u8],
        owner: Owner,
        mode: u32,
    ) -> Vec<u8> {
        let mut header = [b'\0'; 512];
        let mut put = |offset: usize, bytes: &[u8]| {
            header[offset..offset + bytes.len()].copy_from_slice(bytes);
        };
        put(0, &name[..name.len().min(100)]);
        put(100, format!("{mode:07o}\0").as_bytes());
        put(108, format!("{:07o}\0", owner.uid).as_bytes());
        put(116, format!("{:07o}\0", owner.gid).as_bytes());
        put(124, format!("{:011o}\0", data.len()).as_bytes());
        put(136, b"00000000000\0");
        put(148, b"        ");
        put(156, &[typeflag]);
        put(157, linkname.as_bytes());
        put(257, b"ustar\0");
        put(263, b"00");
        let sum: u32 = header.iter().map(|b| u32::from(*b)).sum();
        header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        let mut block = header.to_vec();
        block.extend_from_slice(data);
        block.resize(block.len().next_multiple_of(TAR_BLOCK), 0);
        block
    }

    fn write_archive(dir: &Path, name: &str, body: &[u8]) -> PathBuf {
        let tarball = dir.join(name);
        std::fs::write(&tarball, [body, &[0u8; 1024]].concat()).unwrap();
        tarball
    }

    fn pax_record(key: &str, value: &str) -> String {
        let mut length = key.len() + value.len() + 3;
        loop {
            let record = format!("{length} {key}={value}\n");
            if record.len() == length {
                return record;
            }
            length = record.len();
        }
    }

    fn reseal(block: &mut [u8]) {
        block[148..156].copy_from_slice(b"        ");
        let sum: u32 = block[..TAR_BLOCK].iter().map(|b| u32::from(*b)).sum();
        block[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    }

    fn raw_tar(dir: &Path, name: &str, members: &[(&str, u8, &str)]) -> PathBuf {
        let body: Vec<u8> = members
            .iter()
            .flat_map(|(member, typeflag, linkname)| {
                raw_member(member.as_bytes(), *typeflag, linkname, b"")
            })
            .collect();
        write_archive(dir, name, &body)
    }

    fn tar_with_raw_name(dir: &Path, name: &str, member: &str) -> PathBuf {
        raw_tar(dir, name, &[(member, b'0', "")])
    }

    fn planted(dir: &Path) -> (PathBuf, PathBuf) {
        let rootfs = dir.join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();
        let victim = dir.join("victim");
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(victim.join("sentinel"), b"survives").unwrap();
        (rootfs, victim)
    }

    #[test]
    fn apply_layer_refuses_traversing_members() {
        let dir = tempfile::tempdir().unwrap();
        let (rootfs, victim) = planted(dir.path());

        for (n, member) in [
            "../victim/.wh..wh..opq",
            "../../victim/.wh..wh..opq",
            "../victim/.wh.sentinel",
            "/victim/.wh.sentinel",
            "./../victim/.wh.sentinel",
            "../victim/payload",
        ]
        .iter()
        .enumerate()
        {
            let layer = tar_with_raw_name(dir.path(), &format!("evil{n}.tar"), member);
            let err = apply_layer(&layer, &rootfs, &mut Ownership::default()).unwrap_err();
            assert!(
                matches!(err, ImageError::UnsafePath(_)),
                "{member} gave {err:?}"
            );
            assert!(
                victim.join("sentinel").is_file(),
                "{member} deleted outside"
            );
            assert!(victim.is_dir(), "{member} removed the directory outside");
        }
    }

    #[test]
    fn apply_layer_refuses_whiteouts_through_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let (rootfs, victim) = planted(dir.path());
        std::os::unix::fs::symlink(&victim, rootfs.join("link")).unwrap();

        for (n, member) in ["link/.wh.sentinel", "link/.wh..wh..opq"]
            .iter()
            .enumerate()
        {
            let layer = tar_with_raw_name(dir.path(), &format!("sym{n}.tar"), member);
            let err = apply_layer(&layer, &rootfs, &mut Ownership::default()).unwrap_err();
            assert!(
                matches!(err, ImageError::UnsafePath(_)),
                "{member} gave {err:?}"
            );
            assert!(
                victim.join("sentinel").is_file(),
                "{member} deleted outside"
            );
        }
        assert!(std::fs::symlink_metadata(rootfs.join("link")).is_ok());
    }

    #[test]
    fn a_whiteout_cannot_follow_an_internal_symlink_parent() {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        std::fs::create_dir_all(rootfs.join("actual")).unwrap();
        std::fs::write(rootfs.join("actual/sentinel"), b"unchanged").unwrap();
        std::os::unix::fs::symlink("actual", rootfs.join("link")).unwrap();
        let upper = write_archive(
            dir.path(),
            "upper.tar",
            &raw_member(b"link/.wh.sentinel", b'0', "", b""),
        );

        assert!(matches!(
            apply_layer(&upper, &rootfs, &mut Ownership::default()),
            Err(ImageError::UnsafePath(_))
        ));
        assert_eq!(
            std::fs::read(rootfs.join("actual/sentinel")).unwrap(),
            b"unchanged"
        );
    }

    #[test]
    fn whiteout_of_a_symlink_removes_only_the_link() {
        let dir = tempfile::tempdir().unwrap();
        let (rootfs, victim) = planted(dir.path());
        std::os::unix::fs::symlink(&victim, rootfs.join("link")).unwrap();

        let layer = tar_with_raw_name(dir.path(), "wh-link.tar", ".wh.link");
        apply_layer(&layer, &rootfs, &mut Ownership::default()).unwrap();
        assert!(std::fs::symlink_metadata(rootfs.join("link")).is_err());
        assert!(victim.join("sentinel").is_file());
        assert!(victim.is_dir());
    }

    #[test]
    fn apply_layer_refuses_a_member_under_a_lower_layer_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let (rootfs, victim) = planted(dir.path());

        let stage = dir.path().join("base-stage");
        std::fs::create_dir_all(&stage).unwrap();
        std::os::unix::fs::symlink("../victim", stage.join("link")).unwrap();
        let base = dir.path().join("base.tar");
        assert!(
            Command::new("tar")
                .env("COPYFILE_DISABLE", "1")
                .arg("-cf")
                .arg(&base)
                .arg("-C")
                .arg(&stage)
                .arg("link")
                .status()
                .unwrap()
                .success()
        );
        apply_layer(&base, &rootfs, &mut Ownership::default()).unwrap();
        assert!(
            std::fs::symlink_metadata(rootfs.join("link"))
                .unwrap()
                .is_symlink()
        );

        let upper = tar_layer(dir.path(), "upper", &[("link/pwn", b"owned")]);
        let err = apply_layer(&upper, &rootfs, &mut Ownership::default()).unwrap_err();
        assert!(matches!(err, ImageError::UnsafePath(_)), "{err:?}");
        assert!(!victim.join("pwn").exists(), "wrote outside the rootfs");
        assert!(victim.join("sentinel").is_file());
    }

    #[test]
    fn apply_layer_refuses_a_name_stored_as_both_a_directory_and_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let (rootfs, victim) = planted(dir.path());
        let layer = raw_tar(
            dir.path(),
            "dup.tar",
            &[
                ("link/", b'5', ""),
                ("link", b'2', "../../victim"),
                ("link/pwn", b'0', ""),
            ],
        );
        assert!(matches!(
            apply_layer(&layer, &rootfs, &mut Ownership::default()),
            Err(ImageError::UnsafePath(_))
        ));
        assert!(victim.join("sentinel").exists());
        assert!(!victim.join("pwn").exists());
    }

    #[test]
    fn apply_layer_refuses_a_case_folded_duplicate_name() {
        let dir = tempfile::tempdir().unwrap();
        let (rootfs, victim) = planted(dir.path());
        let layer = raw_tar(
            dir.path(),
            "case.tar",
            &[
                ("Link/", b'5', ""),
                ("link", b'2', "../../victim"),
                ("LINK/pwn", b'0', ""),
            ],
        );
        assert!(matches!(
            apply_layer(&layer, &rootfs, &mut Ownership::default()),
            Err(ImageError::UnsafePath(_))
        ));
        assert!(victim.join("sentinel").exists());
        assert!(!victim.join("pwn").exists());
    }

    #[test]
    fn case_distinct_layer_members_are_rejected_before_extraction() {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        std::fs::create_dir(&rootfs).unwrap();
        std::fs::write(rootfs.join("sentinel"), b"unchanged").unwrap();
        let body = [
            raw_member(b"A", b'0', "", b"first"),
            raw_member(b"a", b'0', "", b"second"),
        ]
        .concat();
        let layer = write_archive(dir.path(), "case-files.tar", &body);

        assert!(matches!(
            apply_layer(&layer, &rootfs, &mut Ownership::default()),
            Err(ImageError::CaseCollision { .. })
        ));
        let names: Vec<_> = std::fs::read_dir(&rootfs)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["sentinel"]);
        assert_eq!(
            std::fs::read(rootfs.join("sentinel")).unwrap(),
            b"unchanged"
        );
    }

    #[test]
    fn case_distinct_ancestor_components_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        std::fs::create_dir(&rootfs).unwrap();
        let body = [
            raw_member(b"Dir/first", b'0', "", b"first"),
            raw_member(b"dir/second", b'0', "", b"second"),
        ]
        .concat();
        let layer = write_archive(dir.path(), "case-ancestors.tar", &body);

        assert!(matches!(
            apply_layer(&layer, &rootfs, &mut Ownership::default()),
            Err(ImageError::CaseCollision { .. })
        ));
        assert_eq!(std::fs::read_dir(rootfs).unwrap().count(), 0);
    }

    #[test]
    fn case_distinct_names_across_layers_are_rejected_before_the_upper_merge() {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        std::fs::create_dir(&rootfs).unwrap();
        let lower = write_archive(
            dir.path(),
            "lower.tar",
            &raw_member(b"A", b'0', "", b"lower"),
        );
        let upper = write_archive(
            dir.path(),
            "upper.tar",
            &raw_member(b"a", b'0', "", b"upper"),
        );
        let mut owners = Ownership::default();
        let mut names = BTreeMap::new();
        apply_layer_tracked(&lower, &rootfs, &mut owners, &mut names).unwrap();

        assert!(matches!(
            apply_layer_tracked(&upper, &rootfs, &mut owners, &mut names),
            Err(ImageError::CaseCollision { .. })
        ));
        let entries: Vec<_> = std::fs::read_dir(&rootfs)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries, ["A"]);
        assert_eq!(std::fs::read(rootfs.join("A")).unwrap(), b"lower");
    }

    #[test]
    fn staging_rejects_case_distinct_names_across_layers() {
        let dir = tempfile::tempdir().unwrap();
        let layout = dir.path().join("layout");
        std::fs::create_dir(&layout).unwrap();
        std::fs::write(
            layout.join("manifest.json"),
            br#"[{"Config":"config.json","Layers":["lower.tar","upper.tar"]}]"#,
        )
        .unwrap();
        std::fs::write(layout.join("config.json"), br#"{"config":{}}"#).unwrap();
        write_archive(&layout, "lower.tar", &raw_member(b"A", b'0', "", b"lower"));
        write_archive(&layout, "upper.tar", &raw_member(b"a", b'0', "", b"upper"));
        let staging = dir.path().join("staging");

        assert!(matches!(
            stage(layout.to_str().unwrap(), &staging),
            Err(ImageError::CaseCollision { .. })
        ));
        assert_eq!(std::fs::read_dir(staging).unwrap().count(), 0);
    }

    #[test]
    fn a_whiteout_cannot_hide_a_differently_cased_lower_name() {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        std::fs::create_dir(&rootfs).unwrap();
        let lower = write_archive(
            dir.path(),
            "lower.tar",
            &raw_member(b"Foo", b'0', "", b"lower"),
        );
        let upper = write_archive(
            dir.path(),
            "upper.tar",
            &raw_member(b".wh.foo", b'0', "", b""),
        );
        let mut owners = Ownership::default();
        let mut names = BTreeMap::new();
        apply_layer_tracked(&lower, &rootfs, &mut owners, &mut names).unwrap();

        assert!(matches!(
            apply_layer_tracked(&upper, &rootfs, &mut owners, &mut names),
            Err(ImageError::CaseCollision { .. })
        ));
        assert_eq!(std::fs::read(rootfs.join("Foo")).unwrap(), b"lower");
    }

    #[test]
    fn an_exact_whiteout_allows_a_new_case_after_removal() {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        std::fs::create_dir(&rootfs).unwrap();
        let lower = write_archive(
            dir.path(),
            "lower.tar",
            &raw_member(b"Foo", b'0', "", b"lower"),
        );
        let upper = write_archive(
            dir.path(),
            "upper.tar",
            &[
                raw_member(b".wh.Foo", b'0', "", b""),
                raw_member(b"foo", b'0', "", b"upper"),
            ]
            .concat(),
        );
        let mut owners = Ownership::default();
        let mut names = BTreeMap::new();
        apply_layer_tracked(&lower, &rootfs, &mut owners, &mut names).unwrap();
        apply_layer_tracked(&upper, &rootfs, &mut owners, &mut names).unwrap();

        assert_eq!(std::fs::read(rootfs.join("foo")).unwrap(), b"upper");
        assert_eq!(std::fs::read_dir(&rootfs).unwrap().count(), 1);
    }

    #[test]
    fn layout_control_files_require_exact_spelling() {
        let dir = tempfile::tempdir().unwrap();
        let layout = dir.path().join("layout");
        std::fs::create_dir(&layout).unwrap();
        std::fs::write(
            layout.join("MANIFEST.JSON"),
            br#"[{"Config":"config.json","Layers":[]}]"#,
        )
        .unwrap();
        std::fs::write(layout.join("config.json"), br#"{"config":{}}"#).unwrap();

        assert!(matches!(
            stage(layout.to_str().unwrap(), &dir.path().join("staging")),
            Err(ImageError::Unrecognized(_))
        ));
    }

    #[test]
    fn docker_save_layers_can_reuse_a_prior_layer_by_relative_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let layout = dir.path().join("layout");
        std::fs::create_dir_all(layout.join("a")).unwrap();
        std::fs::create_dir_all(layout.join("b")).unwrap();
        std::fs::write(
            layout.join("manifest.json"),
            br#"[{"Config":"config.json","Layers":["a/layer.tar","b/layer.tar"]}]"#,
        )
        .unwrap();
        std::fs::write(layout.join("config.json"), br#"{"config":{}}"#).unwrap();
        write_archive(
            &layout.join("a"),
            "layer.tar",
            &raw_member(b"file", b'0', "", b"data"),
        );
        std::os::unix::fs::symlink("../a/layer.tar", layout.join("b/layer.tar")).unwrap();

        let staged = stage(layout.to_str().unwrap(), &dir.path().join("staging")).unwrap();
        assert_eq!(std::fs::read(staged.rootfs.join("file")).unwrap(), b"data");
    }

    #[test]
    fn layout_type_ignores_a_case_alias_symlinked_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let layout = dir.path().join("layout");
        let blobs = layout.join("blobs/sha256");
        std::fs::create_dir_all(&blobs).unwrap();
        std::fs::write(
            layout.join("index.json"),
            br#"{"manifests":[{"digest":"sha256:m"}]}"#,
        )
        .unwrap();
        std::fs::write(
            blobs.join("m"),
            br#"{"config":{"digest":"sha256:c"},"layers":[]}"#,
        )
        .unwrap();
        std::fs::write(blobs.join("c"), br#"{"config":{}}"#).unwrap();
        std::os::unix::fs::symlink("INDEX.JSON", layout.join("manifest.json")).unwrap();

        let staged = stage(layout.to_str().unwrap(), &dir.path().join("stage")).unwrap();
        assert!(staged.rootfs.is_dir());
    }

    #[test]
    fn staging_refuses_a_case_aliased_layout_path() {
        let dir = tempfile::tempdir().unwrap();
        let layout = dir.path().join("layout");
        let blobs = layout.join("blobs/sha256");
        std::fs::create_dir_all(&blobs).unwrap();
        std::fs::write(
            layout.join("index.json"),
            br#"{"manifests":[{"digest":"sha256:m"}]}"#,
        )
        .unwrap();
        std::fs::write(
            blobs.join("m"),
            br#"{"config":{"digest":"sha256:c"},"layers":[]}"#,
        )
        .unwrap();
        std::fs::write(blobs.join("c"), br#"{"config":{}}"#).unwrap();

        let aliased = dir.path().join("LAYOUT");
        assert!(matches!(
            stage_from_path(&aliased, &dir.path().join("stage")),
            Err(ImageError::UnsafePath(_))
        ));
        let staging = dir.path().join("public-stage");
        assert!(matches!(
            stage(aliased.to_str().unwrap(), &staging),
            Err(ImageError::UnsafePath(_))
        ));
        assert!(!staging.exists());

        let parent = dir.path().join("Parent");
        std::fs::create_dir(&parent).unwrap();
        std::os::unix::fs::symlink("../layout", parent.join("image")).unwrap();
        let aliased_parent = dir.path().join("parent/image");
        assert!(matches!(
            stage(aliased_parent.to_str().unwrap(), &staging),
            Err(ImageError::UnsafePath(_))
        ));
        assert!(!staging.exists());

        let bad_target = parent.join("bad-target");
        std::os::unix::fs::symlink("../LAYOUT", &bad_target).unwrap();
        assert!(matches!(
            stage(bad_target.to_str().unwrap(), &staging),
            Err(ImageError::UnsafePath(_))
        ));
        assert!(!staging.exists());

        let exact_link = parent.join("image");
        let staged = stage(exact_link.to_str().unwrap(), &staging).unwrap();
        assert!(staged.rootfs.is_dir());
    }

    #[test]
    fn case_aliased_local_entry_does_not_shadow_image_reference() {
        if let Ok(mode) = std::env::var("HARMONY_IMAGE_REF_CHILD") {
            match mode.as_str() {
                "alias" => {
                    assert!(matches!(
                        stage("./app", Path::new("explicit-stage")),
                        Err(ImageError::UnsafePath(_))
                    ));
                    assert!(!Path::new("explicit-stage").exists());
                    let staged = stage("app", Path::new("stage")).unwrap();
                    assert!(staged.rootfs.is_dir());
                }
                "bad-link" | "dangling-link" => {
                    assert!(matches!(
                        stage("app", Path::new("rejected-stage")),
                        Err(ImageError::UnsafePath(_))
                    ));
                    assert!(!Path::new("rejected-stage").exists());
                }
                _ => panic!("unknown test mode"),
            }
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir(&work).unwrap();
        std::fs::create_dir(work.join("App")).unwrap();
        let archive = write_archive(
            dir.path(),
            "image.tar",
            &[
                raw_member(
                    b"manifest.json",
                    b'0',
                    "",
                    br#"[{"Config":"config.json","Layers":[]}]"#,
                ),
                raw_member(b"config.json", b'0', "", br#"{"config":{}}"#),
            ]
            .concat(),
        );
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let tool = bin.join("docker");
        std::fs::write(
            &tool,
            b"#!/bin/sh\nif [ \"$1\" = --version ]; then exit 0; fi\nif [ \"$1\" = image ] && [ \"$2\" = inspect ]; then echo sha256:fixture; exit 0; fi\nif [ \"$1\" = image ] && [ \"$2\" = save ]; then cp \"$FAKE_IMAGE_TAR\" \"$4\"; exit $?; fi\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let child = |mode| {
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "image::tests::case_aliased_local_entry_does_not_shadow_image_reference",
                ])
                .current_dir(&work)
                .env("HARMONY_IMAGE_REF_CHILD", mode)
                .env("FAKE_IMAGE_TAR", &archive)
                .env(
                    "PATH",
                    format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
                )
                .status()
                .unwrap()
        };
        assert!(child("alias").success());
        std::fs::remove_dir(work.join("App")).unwrap();
        std::fs::create_dir(work.join("layout")).unwrap();
        std::os::unix::fs::symlink("LAYOUT", work.join("app")).unwrap();
        assert!(child("bad-link").success());
        std::fs::remove_file(work.join("app")).unwrap();
        std::os::unix::fs::symlink("missing", work.join("app")).unwrap();
        assert!(child("dangling-link").success());
    }

    #[test]
    fn layout_symlink_targets_require_exact_names_and_stay_inside_the_layout() {
        let dir = tempfile::tempdir().unwrap();
        let layout = dir.path().join("layout");
        std::fs::create_dir_all(layout.join("a")).unwrap();
        std::fs::create_dir_all(layout.join("b")).unwrap();
        std::fs::write(layout.join("a/layer.tar"), b"layer").unwrap();
        for (name, target) in [("case", "../a/LAYER.TAR"), ("escape", "../../outside")] {
            std::os::unix::fs::symlink(target, layout.join("b/layer.tar")).unwrap();
            assert!(
                contained_join(&layout, "b/layer.tar").is_err(),
                "{name} symlink resolved"
            );
            std::fs::remove_file(layout.join("b/layer.tar")).unwrap();
        }
    }

    #[test]
    fn archive_names_outside_ascii_are_rejected_before_extraction() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("layout");
        std::fs::create_dir(&dest).unwrap();
        let archive = write_archive(
            dir.path(),
            "unicode.tar",
            &raw_member(b"caf\xc3\xa9", b'0', "", b"data"),
        );

        assert!(matches!(
            extract_checked(&archive, &dest),
            Err(ImageError::NonPortableName(_))
        ));
        assert_eq!(std::fs::read_dir(dest).unwrap().count(), 0);
    }

    #[test]
    fn hardlink_target_requires_an_earlier_exact_archive_name() {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        std::fs::create_dir(&rootfs).unwrap();
        std::fs::write(rootfs.join("sentinel"), b"unchanged").unwrap();
        let body = [
            raw_member(b"A", b'0', "", b"data"),
            raw_member(b"link", b'1', "a", b""),
        ]
        .concat();
        let layer = write_archive(dir.path(), "case-hardlink.tar", &body);

        assert!(matches!(
            apply_layer(&layer, &rootfs, &mut Ownership::default()),
            Err(ImageError::UnsafePath(_))
        ));
        assert_eq!(std::fs::read_dir(&rootfs).unwrap().count(), 1);
        assert_eq!(
            std::fs::read(rootfs.join("sentinel")).unwrap(),
            b"unchanged"
        );

        let body = [
            raw_member(b"A", b'0', "", b"data"),
            raw_member(b"link", b'1', "A", b""),
        ]
        .concat();
        let layer = write_archive(dir.path(), "exact-hardlink.tar", &body);
        apply_layer(&layer, &rootfs, &mut Ownership::default()).unwrap();
        assert_eq!(std::fs::read(rootfs.join("link")).unwrap(), b"data");
    }

    #[test]
    fn hardlink_with_data_is_rejected_before_rootfs_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        std::fs::create_dir(&rootfs).unwrap();
        std::fs::write(rootfs.join("sentinel"), b"unchanged").unwrap();
        let raw = [
            raw_member(b"a", b'0', "", b"original"),
            raw_member(b"link", b'1', "a", b"replacement"),
        ]
        .concat();
        let record = pax_record("size", "11");
        let mut pax_link = raw_member(b"link", b'1', "a", b"replacement");
        pax_link[124..136].copy_from_slice(b"00000000000\0");
        reseal(&mut pax_link);
        let pax = [
            raw_member(b"a", b'0', "", b"original"),
            raw_member(b"PaxHeaders/0", b'x', "", record.as_bytes()),
            pax_link,
        ]
        .concat();
        for (name, body) in [("raw-hardlink.tar", raw), ("pax-hardlink.tar", pax)] {
            let layer = write_archive(dir.path(), name, &body);
            assert!(matches!(
                apply_layer(&layer, &rootfs, &mut Ownership::default()),
                Err(ImageError::Tar(message)) if message == "hardlink member has nonzero data size"
            ));
            assert_eq!(std::fs::read_dir(&rootfs).unwrap().count(), 1);
            assert_eq!(
                std::fs::read(rootfs.join("sentinel")).unwrap(),
                b"unchanged"
            );
        }
    }

    #[test]
    fn symlink_and_directory_data_are_rejected_before_rootfs_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        std::fs::create_dir(&rootfs).unwrap();
        std::fs::write(rootfs.join("sentinel"), b"unchanged").unwrap();
        let record = pax_record("size", "11");
        let cases = [
            (
                "symlink",
                b'2',
                b"link".as_slice(),
                "target",
                raw_member(b"target", b'0', "", b"data"),
                Vec::new(),
            ),
            (
                "directory",
                b'5',
                b"dir/".as_slice(),
                "",
                Vec::new(),
                raw_member(b"dir/item", b'0', "", b"data"),
            ),
        ];
        for (kind, flag, name, target, before, after) in cases {
            let raw_entry = raw_member(name, flag, target, b"replacement");
            let mut pax_member = raw_entry.clone();
            pax_member[124..136].copy_from_slice(b"00000000000\0");
            reseal(&mut pax_member);
            let raw = [before.clone(), raw_entry, after.clone()].concat();
            let pax = [
                before,
                raw_member(b"PaxHeaders/0", b'x', "", record.as_bytes()),
                pax_member,
                after,
            ]
            .concat();
            for (variant, body) in [("raw", raw), ("pax", pax)] {
                let layer = write_archive(dir.path(), &format!("{kind}-{variant}.tar"), &body);
                assert!(matches!(
                    apply_layer(&layer, &rootfs, &mut Ownership::default()),
                    Err(ImageError::Tar(message)) if message == format!("{kind} member has nonzero data size")
                ));
                assert_eq!(std::fs::read_dir(&rootfs).unwrap().count(), 1);
                assert_eq!(
                    std::fs::read(rootfs.join("sentinel")).unwrap(),
                    b"unchanged"
                );
            }
        }
    }

    #[test]
    fn regular_member_with_trailing_slash_is_rejected_before_extraction() {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        std::fs::create_dir(&rootfs).unwrap();
        std::fs::write(rootfs.join("sentinel"), b"unchanged").unwrap();
        for (name, payload) in [
            ("payload", b"replacement".to_vec()),
            ("hidden", raw_member(b"hidden", b'3', "", b"")),
        ] {
            let layer = write_archive(
                dir.path(),
                &format!("slash-regular-{name}.tar"),
                &raw_member(b"dir/", b'0', "", &payload),
            );
            assert!(matches!(
                apply_layer(&layer, &rootfs, &mut Ownership::default()),
                Err(ImageError::Tar(message)) if message == "non-directory member has a trailing slash"
            ));
            assert_eq!(std::fs::read_dir(&rootfs).unwrap().count(), 1);
            assert_eq!(
                std::fs::read(rootfs.join("sentinel")).unwrap(),
                b"unchanged"
            );
        }
    }

    #[test]
    fn empty_name_override_is_rejected_before_extraction() {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        std::fs::create_dir(&rootfs).unwrap();
        std::fs::write(rootfs.join("sentinel"), b"unchanged").unwrap();
        let pax = pax_record("path", "");
        for (name, header) in [
            ("pax", raw_member(b"PaxHeaders/0", b'x', "", pax.as_bytes())),
            ("gnu", raw_member(b"././@LongLink", b'L', "", b"\0")),
        ] {
            let body = [header, raw_member(b"dir/", b'0', "", b"replacement")].concat();
            let layer = write_archive(dir.path(), &format!("empty-{name}.tar"), &body);
            assert!(matches!(
                apply_layer(&layer, &rootfs, &mut Ownership::default()),
                Err(ImageError::Tar(message)) if message == "archive member has an empty name"
            ));
            assert_eq!(std::fs::read_dir(&rootfs).unwrap().count(), 1);
            assert_eq!(
                std::fs::read(rootfs.join("sentinel")).unwrap(),
                b"unchanged"
            );
        }
    }

    #[test]
    fn overridden_trailing_slash_is_rejected_before_extraction() {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        std::fs::create_dir(&rootfs).unwrap();
        std::fs::write(rootfs.join("sentinel"), b"unchanged").unwrap();
        let pax = pax_record("path", "dir/");
        for (name, header) in [
            ("pax", raw_member(b"PaxHeaders/0", b'x', "", pax.as_bytes())),
            ("gnu", raw_member(b"././@LongLink", b'L', "", b"dir/\0")),
        ] {
            let body = [header, raw_member(b"safe", b'0', "", b"replacement")].concat();
            let layer = write_archive(dir.path(), &format!("slash-{name}.tar"), &body);
            assert!(matches!(
                apply_layer(&layer, &rootfs, &mut Ownership::default()),
                Err(ImageError::Tar(message)) if message == "non-directory member has a trailing slash"
            ));
            assert_eq!(std::fs::read_dir(&rootfs).unwrap().count(), 1);
            assert_eq!(
                std::fs::read(rootfs.join("sentinel")).unwrap(),
                b"unchanged"
            );
        }
    }

    #[test]
    fn mixed_pax_and_gnu_name_overrides_are_rejected_before_extraction() {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        std::fs::create_dir(&rootfs).unwrap();
        std::fs::write(rootfs.join("sentinel"), b"unchanged").unwrap();
        let gnu = raw_member(b"././@LongLink", b'L', "", b"dir/\0");
        let pax = raw_member(
            b"PaxHeaders/0",
            b'x',
            "",
            pax_record("path", "safe").as_bytes(),
        );
        let hidden = raw_member(b"evil", b'0', "", b"bad");
        for (name, headers) in [
            ("gnu-pax", [gnu.clone(), pax.clone()].concat()),
            ("pax-gnu", [pax.clone(), gnu.clone()].concat()),
        ] {
            let body = [headers, raw_member(b"raw", b'0', "", &hidden)].concat();
            let layer = write_archive(dir.path(), &format!("mixed-name-{name}.tar"), &body);
            assert!(matches!(
                apply_layer(&layer, &rootfs, &mut Ownership::default()),
                Err(ImageError::Tar(message)) if message == "member has conflicting GNU and pax name overrides"
            ));
            assert_eq!(std::fs::read_dir(&rootfs).unwrap().count(), 1);
            assert_eq!(
                std::fs::read(rootfs.join("sentinel")).unwrap(),
                b"unchanged"
            );
        }
    }

    #[test]
    fn mixed_pax_and_gnu_link_overrides_are_rejected_before_extraction() {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        std::fs::create_dir(&rootfs).unwrap();
        std::fs::write(rootfs.join("sentinel"), b"unchanged").unwrap();
        let gnu = raw_member(b"././@LongLink", b'K', "", b"missing\0");
        let pax = raw_member(
            b"PaxHeaders/0",
            b'x',
            "",
            pax_record("linkpath", "source").as_bytes(),
        );
        for (name, headers) in [
            ("gnu-pax", [gnu.clone(), pax.clone()].concat()),
            ("pax-gnu", [pax.clone(), gnu.clone()].concat()),
        ] {
            let body = [
                raw_member(b"source", b'0', "", b"data"),
                headers,
                raw_member(b"link", b'1', "source", b""),
            ]
            .concat();
            let layer = write_archive(dir.path(), &format!("mixed-link-{name}.tar"), &body);
            assert!(matches!(
                apply_layer(&layer, &rootfs, &mut Ownership::default()),
                Err(ImageError::Tar(message)) if message == "member has conflicting GNU and pax link overrides"
            ));
            assert_eq!(std::fs::read_dir(&rootfs).unwrap().count(), 1);
            assert_eq!(
                std::fs::read(rootfs.join("sentinel")).unwrap(),
                b"unchanged"
            );
        }
    }

    #[test]
    fn multiple_pax_headers_for_one_member_are_rejected_before_extraction() {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        std::fs::create_dir(&rootfs).unwrap();
        std::fs::write(rootfs.join("sentinel"), b"unchanged").unwrap();
        let path = raw_member(
            b"PaxHeaders/0",
            b'x',
            "",
            pax_record("path", "safe").as_bytes(),
        );
        let time = raw_member(
            b"PaxHeaders/1",
            b'x',
            "",
            pax_record("mtime", "0").as_bytes(),
        );
        let hidden = raw_member(b"evil", b'0', "", b"bad");
        for (name, headers) in [
            ("path-time", [path.clone(), time.clone()].concat()),
            ("time-path", [time.clone(), path.clone()].concat()),
        ] {
            let body = [headers, raw_member(b"dir/", b'0', "", &hidden)].concat();
            let layer = write_archive(dir.path(), &format!("double-pax-{name}.tar"), &body);
            assert!(matches!(
                apply_layer(&layer, &rootfs, &mut Ownership::default()),
                Err(ImageError::Tar(message)) if message == "member has multiple pax headers"
            ));
            assert_eq!(std::fs::read_dir(&rootfs).unwrap().count(), 1);
            assert_eq!(
                std::fs::read(rootfs.join("sentinel")).unwrap(),
                b"unchanged"
            );
        }
    }

    #[test]
    fn same_path_type_changes_are_rejected_before_extraction() {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        let outside = dir.path().join("outside");
        std::fs::create_dir(&rootfs).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(rootfs.join("sentinel"), b"unchanged").unwrap();
        std::fs::write(outside.join("victim"), b"unchanged").unwrap();
        let symlink = raw_member(b"node", b'2', "../outside/victim", b"");
        let regular = raw_member(b"node", b'0', "", b"new");
        for (name, body) in [
            ("symlink-file", [symlink.clone(), regular.clone()].concat()),
            ("file-symlink", [regular.clone(), symlink.clone()].concat()),
        ] {
            let layer = write_archive(dir.path(), &format!("type-change-{name}.tar"), &body);
            assert!(matches!(
                apply_layer(&layer, &rootfs, &mut Ownership::default()),
                Err(ImageError::Tar(message)) if message == "archive changes a member's type at the same path"
            ));
            assert_eq!(std::fs::read_dir(&rootfs).unwrap().count(), 1);
            assert_eq!(
                std::fs::read(rootfs.join("sentinel")).unwrap(),
                b"unchanged"
            );
            assert_eq!(std::fs::read(outside.join("victim")).unwrap(), b"unchanged");
        }
    }

    #[test]
    fn repeated_symlinks_at_one_path_are_rejected_before_extraction() {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        std::fs::create_dir(&rootfs).unwrap();
        std::fs::write(rootfs.join("sentinel"), b"unchanged").unwrap();
        for (name, first, second) in [
            ("ab", "../outside/a", "../outside/b"),
            ("ba", "../outside/b", "../outside/a"),
        ] {
            let body = [
                raw_member(b"node", b'2', first, b""),
                raw_member(b"node", b'2', second, b""),
            ]
            .concat();
            let layer = write_archive(dir.path(), &format!("symlink-repeat-{name}.tar"), &body);
            assert!(matches!(
                apply_layer(&layer, &rootfs, &mut Ownership::default()),
                Err(ImageError::Tar(message)) if message == "archive repeats a symlink at the same path"
            ));
            assert_eq!(std::fs::read_dir(&rootfs).unwrap().count(), 1);
            assert_eq!(
                std::fs::read(rootfs.join("sentinel")).unwrap(),
                b"unchanged"
            );
        }
    }

    #[test]
    fn repeated_regular_members_at_one_path_keep_the_last_content() {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        std::fs::create_dir(&rootfs).unwrap();
        let body = [
            raw_member(b"node", b'0', "", b"first"),
            raw_member(b"node", b'0', "", b"last"),
        ]
        .concat();
        let layer = write_archive(dir.path(), "regular-repeat.tar", &body);
        apply_layer(&layer, &rootfs, &mut Ownership::default()).unwrap();
        assert_eq!(std::fs::read(rootfs.join("node")).unwrap(), b"last");
    }

    #[test]
    fn nonportable_member_types_are_rejected_before_extraction() {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        std::fs::create_dir(&rootfs).unwrap();
        std::fs::write(rootfs.join("sentinel"), b"unchanged").unwrap();
        for (flag, data) in [(b'A', b"x".as_slice()), (b'3', b"".as_slice())] {
            let layer = write_archive(
                dir.path(),
                &format!("type-{}.tar", flag as char),
                &raw_member(b"item", flag, "", data),
            );
            assert!(matches!(
                apply_layer(&layer, &rootfs, &mut Ownership::default()),
                Err(ImageError::Tar(message)) if message == "archive carries an unsupported member type"
            ));
            assert_eq!(std::fs::read_dir(&rootfs).unwrap().count(), 1);
            assert_eq!(
                std::fs::read(rootfs.join("sentinel")).unwrap(),
                b"unchanged"
            );
        }
    }

    #[test]
    fn hardlink_target_checks_pax_and_gnu_longlink_names() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("layout");
        std::fs::create_dir(&dest).unwrap();
        let pax = pax_record("linkpath", "a");
        for (name, header) in [
            (
                "pax-hardlink.tar",
                raw_member(b"PaxHeaders/0", b'x', "", pax.as_bytes()),
            ),
            (
                "gnu-hardlink.tar",
                raw_member(b"././@LongLink", b'K', "", b"a\0"),
            ),
        ] {
            let body = [
                raw_member(b"A", b'0', "", b"data"),
                header,
                raw_member(b"link", b'1', "A", b""),
            ]
            .concat();
            let archive = write_archive(dir.path(), name, &body);
            assert!(matches!(
                extract_checked(&archive, &dest),
                Err(ImageError::UnsafePath(_))
            ));
            assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 0);
        }
    }

    #[test]
    fn apply_layer_refuses_a_symlink_stored_with_a_trailing_slash() {
        let dir = tempfile::tempdir().unwrap();
        let (rootfs, victim) = planted(dir.path());
        let layer = raw_tar(
            dir.path(),
            "slash.tar",
            &[("link/", b'2', "../../victim"), ("link/pwn", b'0', "")],
        );
        assert!(matches!(
            apply_layer(&layer, &rootfs, &mut Ownership::default()),
            Err(ImageError::Tar(message)) if message == "non-directory member has a trailing slash"
        ));
        assert!(victim.join("sentinel").exists());
        assert!(!victim.join("pwn").exists());
    }

    #[test]
    fn read_members_reads_names_a_listing_cannot_show() {
        let dir = tempfile::tempdir().unwrap();
        let body = [
            raw_member(b"two\nlines", b'0', "", b""),
            raw_member(b"caf\xc3\xa9", b'0', "", b""),
            raw_member(b"raw\xff", b'0', "", b""),
        ]
        .concat();
        let names: Vec<Vec<u8>> = read_members(&write_archive(dir.path(), "odd.tar", &body))
            .unwrap()
            .into_iter()
            .map(|m| m.name)
            .collect();
        assert_eq!(
            names,
            vec![
                b"two\nlines".to_vec(),
                b"caf\xc3\xa9".to_vec(),
                b"raw\xff".to_vec()
            ]
        );
    }

    #[test]
    fn apply_layer_refuses_a_symlink_child_beside_a_newline_name() {
        let dir = tempfile::tempdir().unwrap();
        let (rootfs, victim) = planted(dir.path());
        let layer = raw_tar(
            dir.path(),
            "newline.tar",
            &[
                ("pad\nlink/", b'5', ""),
                ("link", b'2', "../../victim"),
                ("link/pwn", b'0', ""),
            ],
        );
        assert!(matches!(
            apply_layer(&layer, &rootfs, &mut Ownership::default()),
            Err(ImageError::UnsafePath(_))
        ));
        assert!(victim.join("sentinel").exists());
        assert!(!victim.join("pwn").exists());
    }

    #[test]
    fn read_members_reads_long_names() {
        let dir = tempfile::tempdir().unwrap();
        let long = "d/".repeat(60) + "leaf";
        let pax = pax_record("path", &long);
        let gnu = [long.as_bytes(), b"\0"].concat();
        let body = [
            raw_member(b"././@LongLink", b'L', "", &gnu),
            raw_member(b"truncated", b'0', "", b""),
            raw_member(b"PaxHeaders/0", b'x', "", pax.as_bytes()),
            raw_member(b"truncated", b'0', "", b""),
        ]
        .concat();
        let names: Vec<Vec<u8>> = read_members(&write_archive(dir.path(), "long.tar", &body))
            .unwrap()
            .into_iter()
            .map(|m| m.name)
            .collect();
        assert_eq!(names, vec![long.as_bytes().to_vec(); 2]);

        let host = tar_layer(dir.path(), "long-host", &[(long.as_str(), b"x")]);
        let names: Vec<String> = read_members(&host)
            .unwrap()
            .iter()
            .map(Member::shown)
            .collect();
        assert!(names.contains(&long), "{names:?}");
    }

    #[test]
    fn apply_layer_refuses_a_traversing_long_name() {
        let dir = tempfile::tempdir().unwrap();
        let (rootfs, victim) = planted(dir.path());
        let long = format!("{}../../victim/pwn\0", "a/".repeat(50));
        let body = [
            raw_member(b"././@LongLink", b'L', "", long.as_bytes()),
            raw_member(b"truncated", b'0', "", b""),
        ]
        .concat();
        let layer = write_archive(dir.path(), "longpath.tar", &body);
        assert!(matches!(
            apply_layer(&layer, &rootfs, &mut Ownership::default()),
            Err(ImageError::UnsafePath(_))
        ));
        assert!(victim.join("sentinel").exists());
    }

    #[test]
    fn read_members_refuses_archives_it_cannot_follow() {
        let dir = tempfile::tempdir().unwrap();
        let member = raw_member(b"file", b'0', "", b"");

        let mut corrupt = member.clone();
        corrupt[148..156].copy_from_slice(b"000000\0 ");
        let path = write_archive(dir.path(), "corrupt.tar", &corrupt);
        assert!(matches!(read_members(&path), Err(ImageError::Tar(_))));

        let mut trailing = member.clone();
        trailing.extend_from_slice(&[0u8; 1024]);
        trailing.extend_from_slice(&raw_member(b"after", b'0', "", b""));
        let path = write_archive(dir.path(), "trailing.tar", &trailing);
        assert!(matches!(read_members(&path), Err(ImageError::Tar(_))));

        let path = dir.path().join("unterminated.tar");
        std::fs::write(&path, &member).unwrap();
        assert!(matches!(read_members(&path), Err(ImageError::Tar(_))));

        let path = dir.path().join("truncated.tar");
        std::fs::write(&path, &member[..300]).unwrap();
        assert!(matches!(read_members(&path), Err(ImageError::Tar(_))));

        let mut oversized = member.clone();
        oversized[124..136].copy_from_slice(&[0xff; 12]);
        reseal(&mut oversized);
        let path = write_archive(dir.path(), "oversized.tar", &oversized);
        assert!(matches!(read_members(&path), Err(ImageError::Tar(_))));
    }

    #[test]
    fn apply_layer_refuses_a_traversing_path_from_the_last_pax_record() {
        let dir = tempfile::tempdir().unwrap();
        let (rootfs, victim) = planted(dir.path());
        let records = [
            pax_record("path", "safe"),
            pax_record("path", "../../victim/pwn"),
        ]
        .concat();
        let body = [
            raw_member(b"PaxHeaders/0", b'x', "", records.as_bytes()),
            raw_member(b"safe", b'0', "", b""),
        ]
        .concat();
        let layer = write_archive(dir.path(), "pax-path.tar", &body);
        assert_eq!(
            read_members(&layer)
                .unwrap()
                .iter()
                .map(Member::shown)
                .collect::<Vec<_>>(),
            vec!["../../victim/pwn".to_string()]
        );
        assert!(matches!(
            apply_layer(&layer, &rootfs, &mut Ownership::default()),
            Err(ImageError::UnsafePath(_))
        ));
        assert!(victim.join("sentinel").exists());
    }

    #[test]
    fn apply_layer_reads_the_members_a_pax_size_of_zero_exposes() {
        let dir = tempfile::tempdir().unwrap();
        let (rootfs, victim) = planted(dir.path());
        let mut cover = raw_member(b"cover", b'0', "", b"");
        cover[124..136].copy_from_slice(b"00000002000\0");
        reseal(&mut cover);
        let body = [
            raw_member(
                b"PaxHeaders/0",
                b'x',
                "",
                pax_record("size", "0").as_bytes(),
            ),
            cover,
            raw_member(b"link", b'2', "../../victim", b""),
            raw_member(b"link/pwn", b'0', "", b""),
        ]
        .concat();
        let layer = write_archive(dir.path(), "pax-zero-size.tar", &body);
        assert_eq!(
            read_members(&layer)
                .unwrap()
                .iter()
                .map(Member::shown)
                .collect::<Vec<_>>(),
            vec![
                "cover".to_string(),
                "link".to_string(),
                "link/pwn".to_string()
            ]
        );
        assert!(matches!(
            apply_layer(&layer, &rootfs, &mut Ownership::default()),
            Err(ImageError::UnsafePath(_))
        ));
        assert!(victim.join("sentinel").exists());
        assert!(!victim.join("pwn").exists());
    }

    #[test]
    fn read_members_skips_the_data_a_pax_size_declares() {
        let dir = tempfile::tempdir().unwrap();
        let data = [
            raw_member(b"link", b'2', "../../victim", b""),
            raw_member(b"link/pwn", b'0', "", b""),
        ]
        .concat();
        let mut cover = raw_member(b"cover", b'0', "", &data);
        cover[124..136].copy_from_slice(b"00000000000\0");
        reseal(&mut cover);
        let body = [
            raw_member(
                b"PaxHeaders/0",
                b'x',
                "",
                pax_record("size", "1024").as_bytes(),
            ),
            cover,
        ]
        .concat();
        let layer = write_archive(dir.path(), "pax-size.tar", &body);
        assert_eq!(
            read_members(&layer)
                .unwrap()
                .iter()
                .map(Member::shown)
                .collect::<Vec<_>>(),
            vec!["cover".to_string()]
        );
    }

    #[test]
    fn read_members_refuses_unmodeled_header_forms() {
        let dir = tempfile::tempdir().unwrap();
        let member = raw_member(b"file", b'0', "", b"");
        let refused = |name: &str, body: &[u8]| {
            let path = write_archive(dir.path(), name, body);
            assert!(
                matches!(read_members(&path), Err(ImageError::Tar(_))),
                "{name}"
            );
        };

        for (name, record) in [
            ("global-path.tar", pax_record("path", "elsewhere")),
            ("global-comment.tar", pax_record("comment", "anything")),
        ] {
            let mut body = raw_member(b"pax_global_header", b'g', "", record.as_bytes());
            body.extend_from_slice(&member);
            refused(name, &body);
        }

        let mut sparse = raw_member(
            b"PaxHeaders/0",
            b'x',
            "",
            pax_record("GNU.sparse.size", "4096").as_bytes(),
        );
        sparse.extend_from_slice(&member);
        refused("sparse-pax.tar", &sparse);
        refused("sparse-member.tar", &raw_member(b"file", b'S', "", b""));

        refused("multivolume.tar", &raw_member(b"file", b'M', "", b""));

        let mut keyless = raw_member(b"PaxHeaders/0", b'x', "", b"5 bad\n");
        keyless.extend_from_slice(&member);
        refused("keyless-pax.tar", &keyless);

        let mut unreadable = raw_member(
            b"PaxHeaders/0",
            b'x',
            "",
            pax_record("size", "eleven").as_bytes(),
        );
        unreadable.extend_from_slice(&member);
        refused("unreadable-size.tar", &unreadable);
    }

    #[test]
    fn stage_rejects_platform_file_flags_before_extraction() {
        let dir = tempfile::tempdir().unwrap();
        let layout = dir.path().join("layout");
        std::fs::create_dir(&layout).unwrap();
        std::fs::write(
            layout.join("manifest.json"),
            br#"[{"Config":"config.json","Layers":["layer.tar"]}]"#,
        )
        .unwrap();
        std::fs::write(layout.join("config.json"), br#"{"config":{}}"#).unwrap();
        let body = [
            raw_member(
                b"PaxHeaders/0",
                b'x',
                "",
                pax_record("SCHILY.fflags", "uchg").as_bytes(),
            ),
            raw_member(b"foo", b'0', "", b"payload"),
        ]
        .concat();
        write_archive(&layout, "layer.tar", &body);
        let staging = dir.path().join("stage");

        assert!(matches!(
            stage(layout.to_str().unwrap(), &staging),
            Err(ImageError::Tar(message)) if message == "archive carries platform-specific file flags"
        ));
        assert_eq!(std::fs::read_dir(&staging).unwrap().count(), 0);

        let harmless = [
            raw_member(
                b"PaxHeaders/0",
                b'x',
                "",
                pax_record("mtime", "0").as_bytes(),
            ),
            raw_member(b"foo", b'0', "", b"payload"),
        ]
        .concat();
        write_archive(&layout, "layer.tar", &harmless);
        let staged = stage(layout.to_str().unwrap(), &staging).unwrap();
        assert_eq!(
            std::fs::read(staged.rootfs.join("foo")).unwrap(),
            b"payload"
        );
    }

    #[test]
    fn stage_rejects_nested_appledouble_name_before_extraction() {
        let dir = tempfile::tempdir().unwrap();
        let layout = dir.path().join("layout");
        std::fs::create_dir(&layout).unwrap();
        std::fs::write(
            layout.join("manifest.json"),
            br#"[{"Config":"config.json","Layers":["layer.tar"]}]"#,
        )
        .unwrap();
        std::fs::write(layout.join("config.json"), br#"{"config":{}}"#).unwrap();
        let mut apple_double = Vec::new();
        apple_double.extend_from_slice(&0x0005_1607u32.to_be_bytes());
        apple_double.extend_from_slice(&0x0002_0000u32.to_be_bytes());
        apple_double.extend_from_slice(b"Mac OS X        ");
        apple_double.extend_from_slice(&2u16.to_be_bytes());
        apple_double.extend_from_slice(&9u32.to_be_bytes());
        apple_double.extend_from_slice(&50u32.to_be_bytes());
        apple_double.extend_from_slice(&32u32.to_be_bytes());
        apple_double.extend_from_slice(&2u32.to_be_bytes());
        apple_double.extend_from_slice(&82u32.to_be_bytes());
        apple_double.extend_from_slice(&8u32.to_be_bytes());
        apple_double.extend_from_slice(&[0; 32]);
        apple_double.extend_from_slice(b"forkdata");
        let body = [
            raw_member(b"dir/sub/._foo", b'0', "", &apple_double),
            raw_member(b"dir/sub/foo", b'0', "", b"payload"),
        ]
        .concat();
        write_archive(&layout, "layer.tar", &body);
        let staging = dir.path().join("stage");

        assert!(matches!(
            stage(layout.to_str().unwrap(), &staging),
            Err(ImageError::Tar(message)) if message == "archive carries an AppleDouble metadata name"
        ));
        assert_eq!(std::fs::read_dir(staging).unwrap().count(), 0);
    }

    #[test]
    fn stage_rejects_platform_acl_before_extraction() {
        let dir = tempfile::tempdir().unwrap();
        let layout = dir.path().join("layout");
        std::fs::create_dir(&layout).unwrap();
        std::fs::write(
            layout.join("manifest.json"),
            br#"[{"Config":"config.json","Layers":["layer.tar"]}]"#,
        )
        .unwrap();
        std::fs::write(layout.join("config.json"), br#"{"config":{}}"#).unwrap();
        let acl = "group:everyone:d::deny:12,owner@:rwpaARWcCos::allow,group@:raRcs::allow,everyone@:raRcs::allow";
        let body = [
            raw_member(
                b"PaxHeaders/0",
                b'x',
                "",
                pax_record("SCHILY.acl.ace", acl).as_bytes(),
            ),
            raw_member(b"foo", b'0', "", b"payload"),
        ]
        .concat();
        write_archive(&layout, "layer.tar", &body);
        let staging = dir.path().join("stage");

        assert!(matches!(
            stage(layout.to_str().unwrap(), &staging),
            Err(ImageError::Tar(message)) if message == "archive carries platform-specific ACL metadata"
        ));
        assert_eq!(std::fs::read_dir(staging).unwrap().count(), 0);

        for key in ["SCHILY.acl.access", "SCHILY.acl.default"] {
            let body = [
                raw_member(
                    b"PaxHeaders/0",
                    b'x',
                    "",
                    pax_record(key, "u::rwx").as_bytes(),
                ),
                raw_member(b"foo", b'0', "", b"payload"),
            ]
            .concat();
            let layer = write_archive(dir.path(), &format!("{key}.tar"), &body);
            assert!(matches!(
                read_members(&layer),
                Err(ImageError::Tar(message)) if message == "archive carries platform-specific ACL metadata"
            ));
        }
    }

    #[test]
    fn read_members_reads_a_binary_size_field() {
        let dir = tempfile::tempdir().unwrap();
        let mut member = raw_member(b"big", b'0', "", b"data");
        let mut size = [0u8; 12];
        size[0] = 0x80;
        size[11] = 4;
        member[124..136].copy_from_slice(&size);
        reseal(&mut member);
        let path = write_archive(dir.path(), "binary-size.tar", &member);
        let names: Vec<String> = read_members(&path)
            .unwrap()
            .iter()
            .map(Member::shown)
            .collect();
        assert_eq!(names, vec!["big".to_string()]);
    }

    #[test]
    fn read_members_reads_compressed_layers() {
        let dir = tempfile::tempdir().unwrap();
        let plain = tar_layer(dir.path(), "layer", &[("etc/passwd", b"root")]);
        let expected: Vec<String> = read_members(&plain)
            .unwrap()
            .iter()
            .map(Member::shown)
            .collect();
        assert!(expected.contains(&"etc/passwd".to_string()));
        for tool in ["gzip", "zstd", "xz", "bzip2"] {
            let compressed = dir.path().join(format!("layer.{tool}"));
            let out = Command::new(tool)
                .arg("-c")
                .arg(&plain)
                .stdout(std::fs::File::create(&compressed).unwrap())
                .status();
            if !out.is_ok_and(|status| status.success()) {
                continue;
            }
            let names: Vec<String> = read_members(&compressed)
                .unwrap()
                .iter()
                .map(Member::shown)
                .collect();
            assert_eq!(names, expected, "{tool}");
        }
    }

    #[test]
    fn apply_layer_refuses_a_symlink_and_a_member_under_it_in_one_layer() {
        let dir = tempfile::tempdir().unwrap();
        let (rootfs, victim) = planted(dir.path());
        for (n, members) in [
            vec![("link", b'2', "../../victim"), ("link/pwn", b'0', "")],
            vec![("link", b'2', "/etc"), ("link/pwn", b'0', "")],
            vec![("Link", b'2', "../../victim"), ("link/pwn", b'0', "")],
            vec![("link", b'0', ""), ("link/pwn", b'0', "")],
        ]
        .iter()
        .enumerate()
        {
            let layer = raw_tar(dir.path(), &format!("same{n}.tar"), members);
            let err = apply_layer(&layer, &rootfs, &mut Ownership::default()).unwrap_err();
            assert!(
                matches!(err, ImageError::UnsafePath(_)),
                "{members:?}: {err:?}"
            );
            assert!(!victim.join("pwn").exists(), "{members:?} wrote outside");
            assert!(victim.join("sentinel").is_file(), "{members:?}");
        }
    }

    #[test]
    fn apply_layer_keeps_directories_and_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();
        let stage = dir.path().join("ok-stage");
        std::fs::create_dir_all(stage.join("bin")).unwrap();
        std::fs::write(stage.join("bin/busybox"), b"elf").unwrap();
        std::os::unix::fs::symlink("busybox", stage.join("bin/sh")).unwrap();
        std::os::unix::fs::symlink("/absolute/target", stage.join("abs")).unwrap();
        let layer = dir.path().join("ok.tar");
        assert!(
            Command::new("tar")
                .env("COPYFILE_DISABLE", "1")
                .arg("-cf")
                .arg(&layer)
                .arg("-C")
                .arg(&stage)
                .args(["bin", "abs"])
                .status()
                .unwrap()
                .success()
        );
        apply_layer(&layer, &rootfs, &mut Ownership::default()).unwrap();
        assert_eq!(std::fs::read(rootfs.join("bin/busybox")).unwrap(), b"elf");
        assert_eq!(
            std::fs::read_link(rootfs.join("bin/sh")).unwrap(),
            Path::new("busybox")
        );
        assert_eq!(
            std::fs::read_link(rootfs.join("abs")).unwrap(),
            Path::new("/absolute/target")
        );
    }

    #[test]
    fn apply_layer_fills_a_read_only_directory() {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();
        let stage = dir.path().join("ro-stage");
        std::fs::create_dir_all(stage.join("ro")).unwrap();
        std::fs::write(stage.join("ro/x"), b"content").unwrap();
        std::fs::set_permissions(stage.join("ro"), std::fs::Permissions::from_mode(0o555)).unwrap();
        let layer = dir.path().join("ro.tar");
        assert!(
            Command::new("tar")
                .env("COPYFILE_DISABLE", "1")
                .arg("-cf")
                .arg(&layer)
                .arg("-C")
                .arg(&stage)
                .arg("ro")
                .status()
                .unwrap()
                .success()
        );
        apply_layer(&layer, &rootfs, &mut Ownership::default()).unwrap();
        assert_eq!(std::fs::read(rootfs.join("ro/x")).unwrap(), b"content");
        let mode = std::fs::symlink_metadata(rootfs.join("ro"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o555);
        for path in [stage.join("ro"), rootfs.join("ro")] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    fn apply_layer_replaces_earlier_entries() {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();
        let base = tar_layer(dir.path(), "one", &[("f", b"old"), ("d/x", b"x")]);
        apply_layer(&base, &rootfs, &mut Ownership::default()).unwrap();
        let upper = tar_layer(dir.path(), "two", &[("f", b"new"), ("d/y", b"y")]);
        apply_layer(&upper, &rootfs, &mut Ownership::default()).unwrap();
        assert_eq!(std::fs::read(rootfs.join("f")).unwrap(), b"new");
        assert_eq!(std::fs::read(rootfs.join("d/x")).unwrap(), b"x");
        assert_eq!(std::fs::read(rootfs.join("d/y")).unwrap(), b"y");
    }

    #[test]
    fn apply_layer_honors_whiteouts() {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();

        let base = tar_layer(
            dir.path(),
            "base",
            &[
                ("keep.txt", b"keep"),
                ("gone.txt", b"gone"),
                ("d/old", b"old"),
            ],
        );
        apply_layer(&base, &rootfs, &mut Ownership::default()).unwrap();
        assert!(rootfs.join("keep.txt").is_file());
        assert!(rootfs.join("d/old").is_file());

        let upper = tar_layer(
            dir.path(),
            "upper",
            &[
                (".wh.gone.txt", b""),
                ("d/.wh..wh..opq", b""),
                ("d/new", b"new"),
            ],
        );
        apply_layer(&upper, &rootfs, &mut Ownership::default()).unwrap();
        assert!(rootfs.join("keep.txt").is_file());
        assert!(!rootfs.join("gone.txt").exists());
        assert!(!rootfs.join("d/old").exists());
        assert_eq!(std::fs::read(rootfs.join("d/new")).unwrap(), b"new");
        assert!(!rootfs.join("d/.wh..wh..opq").exists());
    }

    const SERVICE: Owner = Owner { uid: 70, gid: 70 };

    #[test]
    fn read_members_reads_the_owner_of_each_member() {
        let dir = tempfile::tempdir().unwrap();
        let body = [
            raw_member_owned(b"data", b'5', "", b"", SERVICE),
            raw_member(b"etc", b'5', "", b""),
            raw_member(
                b"PaxHeaders/0",
                b'x',
                "",
                [pax_record("uid", "70000"), pax_record("gid", "5")]
                    .concat()
                    .as_bytes(),
            ),
            raw_member_owned(b"wide", b'0', "", b"", Owner { uid: 1, gid: 1 }),
        ]
        .concat();
        let members = read_members(&write_archive(dir.path(), "owned.tar", &body)).unwrap();
        let owners: Vec<(String, Owner)> = members
            .iter()
            .map(|member| (member.shown(), member.owner))
            .collect();
        assert_eq!(
            owners,
            [
                ("data".to_string(), SERVICE),
                ("etc".to_string(), Owner::ROOT),
                ("wide".to_string(), Owner { uid: 70000, gid: 5 }),
            ]
        );
    }

    #[test]
    fn read_members_refuses_owners_it_cannot_apply() {
        let dir = tempfile::tempdir().unwrap();
        for (name, record) in [
            ("huge-uid.tar", pax_record("uid", "4294967296")),
            ("text-gid.tar", pax_record("gid", "service")),
        ] {
            let mut body = raw_member(b"PaxHeaders/0", b'x', "", record.as_bytes());
            body.extend_from_slice(&raw_member(b"file", b'0', "", b""));
            let path = write_archive(dir.path(), name, &body);
            assert!(
                matches!(read_members(&path), Err(ImageError::Tar(_))),
                "{name}"
            );
        }
    }

    #[test]
    fn apply_layer_records_what_each_layer_did_to_ownership() {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();
        let mut owners = Ownership::default();

        let base = [
            raw_member_owned(b"data/", b'5', "", b"", SERVICE),
            raw_member_owned(b"data/conf", b'0', "", b"cfg", SERVICE),
            raw_member_owned(b"data/sub/", b'5', "", b"", SERVICE),
            raw_member_owned(b"data/sub/x", b'0', "", b"x", SERVICE),
            raw_member_owned(b"gone", b'0', "", b"gone", SERVICE),
            raw_member_owned(b"swap/", b'5', "", b"", SERVICE),
            raw_member_owned(b"swap/child", b'0', "", b"child", SERVICE),
            raw_member(b"etc", b'0', "", b"root"),
        ]
        .concat();
        apply_layer(
            &write_archive(dir.path(), "base.tar", &base),
            &rootfs,
            &mut owners,
        )
        .unwrap();
        for path in [
            "data",
            "data/conf",
            "data/sub",
            "data/sub/x",
            "gone",
            "swap",
            "swap/child",
        ] {
            assert_eq!(owners.owner_of(Path::new(path)), SERVICE, "{path}");
        }
        assert_eq!(owners.owner_of(Path::new("etc")), Owner::ROOT);
        assert_eq!(
            owners.owner_of(Path::new("./data/")),
            SERVICE,
            "one entry, however spelled"
        );

        let other = Owner { uid: 71, gid: 71 };
        let upper = [
            raw_member(b".wh.gone", b'0', "", b""),
            raw_member(b"data/.wh..wh..opq", b'0', "", b""),
            raw_member_owned(b"data/new", b'0', "", b"new", other),
            raw_member(b"swap", b'0', "", b"file now"),
            raw_member_owned(b"etc", b'0', "", b"taken", other),
        ]
        .concat();
        apply_layer(
            &write_archive(dir.path(), "upper.tar", &upper),
            &rootfs,
            &mut owners,
        )
        .unwrap();
        assert_eq!(
            owners.owner_of(Path::new("gone")),
            Owner::ROOT,
            "whiteout forgets the entry"
        );
        assert_eq!(
            owners.owner_of(Path::new("data")),
            SERVICE,
            "opaque keeps the directory"
        );
        assert_eq!(
            owners.owner_of(Path::new("data/conf")),
            Owner::ROOT,
            "opaque forgets children"
        );
        assert_eq!(owners.owner_of(Path::new("data/sub/x")), Owner::ROOT);
        assert_eq!(owners.owner_of(Path::new("data/new")), other);
        assert_eq!(
            owners.owner_of(Path::new("swap")),
            Owner::ROOT,
            "the file replacing the directory"
        );
        assert_eq!(
            owners.owner_of(Path::new("swap/child")),
            Owner::ROOT,
            "and the children it took"
        );
        assert_eq!(
            owners.owner_of(Path::new("etc")),
            other,
            "a later layer owns what it replaced"
        );
        assert!(!rootfs.join("gone").exists());
        assert!(!rootfs.join("data/conf").exists());
        assert!(rootfs.join("swap").is_file());
        assert_eq!(std::fs::read(rootfs.join("data/new")).unwrap(), b"new");
    }

    #[test]
    fn apply_layer_keeps_the_modes_the_layer_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        std::fs::create_dir(&rootfs).unwrap();
        let expected = [
            ("tmp", b'5', 0o1777),
            ("secret", b'5', 0o700),
            ("secret/key", b'0', 0o600),
            ("shared", b'0', 0o660),
        ];
        let layer: Vec<u8> = expected
            .iter()
            .flat_map(|(name, typeflag, mode)| {
                raw_member_with_mode(name.as_bytes(), *typeflag, "", b"", Owner::ROOT, *mode)
            })
            .collect();
        apply_layer(
            &write_archive(dir.path(), "layer.tar", &layer),
            &rootfs,
            &mut Ownership::default(),
        )
        .unwrap();
        for (name, _, mode) in expected {
            let on_disk = std::fs::metadata(rootfs.join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777;
            assert_eq!(on_disk, mode, "{name}");
        }
    }

    #[test]
    fn implicit_layer_parent_has_a_fixed_mode() {
        let Some(marker) = std::env::var_os("HARMONY_OCI_UMASK_TEST_MARKER") else {
            let marker = tempfile::NamedTempFile::new().unwrap();
            let output = Command::new("sh")
                .arg("-c")
                .arg("umask 077; exec \"$@\"")
                .arg("sh")
                .arg(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "image::tests::implicit_layer_parent_has_a_fixed_mode",
                    "--nocapture",
                ])
                .env("HARMONY_OCI_UMASK_TEST_MARKER", marker.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "restricted-umask child failed: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(std::fs::read(marker.path()).unwrap(), b"ok");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();
        let member = raw_member_with_mode(b"implicit/file", b'0', "", b"data", Owner::ROOT, 0o644);
        let layer = write_archive(dir.path(), "implicit.tar", &member);

        apply_layer(&layer, &rootfs, &mut Ownership::default()).unwrap();
        assert_eq!(
            std::fs::symlink_metadata(rootfs.join("implicit"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o755
        );
        assert_eq!(
            std::fs::read(rootfs.join("implicit/file")).unwrap(),
            b"data"
        );
        std::fs::write(marker, b"ok").unwrap();
    }

    fn cpio_entry(bytes: &[u8], at: usize) -> (String, Owner, usize) {
        let field = |i: usize| {
            let text = std::str::from_utf8(&bytes[at + 6 + 8 * i..at + 6 + 8 * (i + 1)]).unwrap();
            u32::from_str_radix(text, 16).unwrap()
        };
        assert_eq!(&bytes[at..at + 6], b"070701");
        let (uid, gid, filesize, namesize) = (field(2), field(3), field(6), field(11));
        let name_at = at + 110;
        let name = std::str::from_utf8(&bytes[name_at..name_at + namesize as usize - 1])
            .unwrap()
            .to_string();
        let data_at = (name_at + namesize as usize).next_multiple_of(4);
        let next = (data_at + filesize as usize).next_multiple_of(4);
        (name, Owner { uid, gid }, next)
    }

    #[test]
    fn staging_carries_layer_ownership_into_the_rootfs_segment() {
        let dir = tempfile::tempdir().unwrap();
        let layout = dir.path().join("layout");
        let blobs = layout.join("blobs/sha256");
        std::fs::create_dir_all(&blobs).unwrap();
        std::fs::write(
            layout.join("index.json"),
            br#"{"manifests":[{"digest":"sha256:m"}]}"#,
        )
        .unwrap();
        std::fs::write(
            blobs.join("m"),
            br#"{"config":{"digest":"sha256:c"},"layers":[{"digest":"sha256:l"}]}"#,
        )
        .unwrap();
        std::fs::write(blobs.join("c"), br#"{"config":{"Cmd":["/node"]}}"#).unwrap();
        let layer = [
            raw_member_owned(b"var/", b'5', "", b"", Owner::ROOT),
            raw_member_owned(b"var/data/", b'5', "", b"", SERVICE),
            raw_member_owned(b"var/data/conf", b'0', "", b"cfg", SERVICE),
            raw_member(b"node", b'0', "", b"#!/bin/sh"),
        ]
        .concat();
        std::fs::write(blobs.join("l"), [layer, vec![0u8; 1024]].concat()).unwrap();

        let stage_dir = dir.path().join("stage");
        std::fs::create_dir_all(&stage_dir).unwrap();
        let staged = stage(layout.to_str().unwrap(), &stage_dir).unwrap();
        assert_eq!(staged.config.cmd, ["/node"]);
        assert_eq!(staged.owners.owner_of(Path::new("var/data/conf")), SERVICE);
        assert_eq!(staged.owners.owner_of(Path::new("var")), Owner::ROOT);
        use std::os::unix::fs::MetadataExt as _;
        let staging_user = std::fs::metadata(&stage_dir).unwrap().uid();
        for path in ["var", "var/data", "var/data/conf", "node"] {
            let on_disk = std::fs::metadata(staged.rootfs.join(path)).unwrap().uid();
            assert_eq!(on_disk, staging_user, "{path}");
        }

        let segment =
            super::super::bundle::build_rootfs_segment(&staged.rootfs, &staged.owners).unwrap();
        let inflated = Command::new("gzip")
            .arg("-dc")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write as _;
                child.stdin.take().unwrap().write_all(&segment)?;
                child.wait_with_output()
            })
            .unwrap();
        assert!(inflated.status.success());
        let cpio = inflated.stdout;
        let mut at = 0;
        let mut owners = BTreeMap::new();
        loop {
            let (name, owner, next) = cpio_entry(&cpio, at);
            if name == "TRAILER!!!" {
                break;
            }
            owners.insert(name, owner);
            at = next;
        }
        assert_eq!(owners["harmony-oci/rootfs/var/data"], SERVICE);
        assert_eq!(owners["harmony-oci/rootfs/var/data/conf"], SERVICE);
        assert_eq!(owners["harmony-oci/rootfs/var"], Owner::ROOT);
        assert_eq!(owners["harmony-oci/rootfs/node"], Owner::ROOT);
        assert_eq!(owners["harmony-oci/rootfs"], Owner::ROOT);
    }

    #[test]
    fn preparation_preserves_special_modes_from_layer_headers() {
        let dir = tempfile::tempdir().unwrap();
        let layout = dir.path().join("layout");
        std::fs::create_dir_all(&layout).unwrap();
        std::fs::write(
            layout.join("manifest.json"),
            br#"[{"Config":"config.json","Layers":["layer.tar","upper.tar"]}]"#,
        )
        .unwrap();
        std::fs::write(layout.join("config.json"), br#"{"config":{}}"#).unwrap();
        let layer = [
            raw_member_with_mode(b"bin/", b'5', "", b"", Owner::ROOT, 0o755),
            raw_member_with_mode(b"bin/tool", b'0', "", b"exec", Owner::ROOT, 0o4755),
            raw_member_with_mode(b"bin/tool-link", b'1', "bin/tool", b"", SERVICE, 0),
            raw_member_with_mode(b"shared/", b'5', "", b"", Owner::ROOT, 0o2775),
            raw_member_with_mode(b"shared/implicit/data", b'0', "", b"x", Owner::ROOT, 0o644),
            raw_member_with_mode(b"overlaid/", b'5', "", b"", Owner::ROOT, 0o2775),
        ]
        .concat();
        write_archive(&layout, "layer.tar", &layer);
        write_archive(
            &layout,
            "upper.tar",
            &raw_member(b"overlaid/child", b'0', "", b"upper"),
        );

        let staged = stage(layout.to_str().unwrap(), &dir.path().join("staging")).unwrap();
        let prepared = super::super::bundle::prepare(
            &staged,
            &super::super::bundle::LaunchRequest::new(Vec::new()),
        )
        .unwrap();
        let inflated = Command::new("gzip")
            .arg("-dc")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write as _;
                child
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(&prepared.rootfs_segment)?;
                child.wait_with_output()
            })
            .unwrap();
        assert!(inflated.status.success());
        let mut at = 0;
        let mut metadata = BTreeMap::new();
        loop {
            let mode = u32::from_str_radix(
                std::str::from_utf8(&inflated.stdout[at + 14..at + 22]).unwrap(),
                16,
            )
            .unwrap();
            let (name, owner, next) = cpio_entry(&inflated.stdout, at);
            if name == "TRAILER!!!" {
                break;
            }
            metadata.insert(name, (mode & 0o7777, owner));
            at = next;
        }
        assert_eq!(
            metadata["harmony-oci/rootfs/bin/tool"],
            (0o4755, Owner::ROOT)
        );
        assert_eq!(
            metadata["harmony-oci/rootfs/bin/tool-link"],
            (0o4755, Owner::ROOT)
        );
        assert_eq!(metadata["harmony-oci/rootfs/shared"].0, 0o2775);
        assert_eq!(metadata["harmony-oci/rootfs/shared/implicit"].0, 0o755);
        assert_eq!(metadata["harmony-oci/rootfs/overlaid"].0, 0o755);
        assert_eq!(
            prepared.identity_hex(),
            "1825f2d18ba46aefc692b982b0643f12fd7a94256310e60f31f385d5d19f6a27"
        );
    }
}

#[cfg(test)]
mod acceptance_regressions {
    use super::*;

    #[test]
    fn block_reader_distinguishes_empty_complete_and_truncated_headers() {
        let mut block = [0; TAR_BLOCK];
        assert!(!read_block(&mut &b""[..], &mut block).unwrap());
        assert!(read_block(&mut &[3; TAR_BLOCK][..], &mut block).unwrap());
        assert_eq!(block, [3; TAR_BLOCK]);
        assert!(read_block(&mut &[3; TAR_BLOCK - 1][..], &mut block).is_err());
    }

    #[test]
    fn name_header_limit_accepts_exact_bound_and_rejects_one_more() {
        for size in [0, 1, MAX_HEADER_DATA - 1, MAX_HEADER_DATA] {
            let source = vec![7; (size + padding(size)) as usize];
            assert_eq!(
                header_data(&mut source.as_slice(), size).unwrap(),
                vec![7; size as usize]
            );
        }
        assert!(header_data(&mut &b""[..], MAX_HEADER_DATA + 1).is_err());
    }

    #[test]
    fn gnu_header_does_not_treat_timestamp_field_as_ustar_prefix() {
        let mut block = [0; TAR_BLOCK];
        block[..4].copy_from_slice(b"file");
        block[345..348].copy_from_slice(b"123");
        assert_eq!(ustar_name(&block), b"file");
        block[257..263].copy_from_slice(b"ustar\0");
        assert_eq!(ustar_name(&block), b"123/file");
    }

    #[test]
    fn pax_lengths_are_bounded_and_unrelated_metadata_is_allowed() {
        for record in [b"0 a=b\n".as_slice(), b"99 a=b\n"] {
            assert!(parse_pax(record, &mut Pax::default()).is_err());
        }
        parse_pax(b"11 uid=123\n", &mut Pax::default()).unwrap();
        parse_pax(b"10 uid=12\n", &mut Pax::default()).unwrap();
        assert!(parse_pax(b"22 GNU.sparse.size=1\n", &mut Pax::default()).is_err());
    }

    #[test]
    fn layers_replace_files_with_directories_and_directories_with_files() {
        let root = tempfile::tempdir().unwrap();
        let staged = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("entry"), b"old").unwrap();
        std::fs::create_dir(staged.path().join("entry")).unwrap();
        std::fs::write(staged.path().join("entry/child"), b"child").unwrap();
        merge_layer(staged.path(), root.path()).unwrap();
        assert_eq!(
            std::fs::read(root.path().join("entry/child")).unwrap(),
            b"child"
        );
        let staged = tempfile::tempdir().unwrap();
        std::fs::write(staged.path().join("entry"), b"replacement").unwrap();
        merge_layer(staged.path(), root.path()).unwrap();
        assert_eq!(
            std::fs::read(root.path().join("entry")).unwrap(),
            b"replacement"
        );
    }

    #[test]
    fn resolution_propagates_symlink_loop_instead_of_treating_it_as_missing() {
        let root = tempfile::tempdir().unwrap();
        let link = root.path().join("loop");
        std::os::unix::fs::symlink("loop", &link).unwrap();
        assert!(resolve_deepest(&link).is_err());
    }

    #[test]
    fn dropping_archive_reader_reaps_its_decompressor() {
        let mut child = Command::new("sh")
            .args(["-c", "exec cat"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let reader = child.stdout.take().unwrap();
        let pid = child.id();
        drop(ArchiveReader {
            reader: Box::new(reader),
            child: Some(child),
        });
        let exists = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .unwrap();
        assert!(!exists.success(), "decompressor was not reaped");
        drop(input);
    }
}

#[cfg(test)]
mod reader_regressions {
    use super::*;
    #[test]
    fn archive_reader_forwards_bytes_count_and_eof() {
        let mut reader = ArchiveReader {
            reader: Box::new(std::io::Cursor::new(b"hello")),
            child: None,
        };
        let mut bytes = [0; 8];
        assert_eq!(reader.read(&mut bytes).unwrap(), 5);
        assert_eq!(&bytes[..5], b"hello");
        assert_eq!(reader.read(&mut bytes).unwrap(), 0);
    }
}

#[cfg(test)]
mod trailing_eof_regression {
    use super::*;
    #[test]
    fn trailing_padding_stops_at_first_eof() {
        struct Once(bool);
        impl Read for Once {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                assert!(!self.0, "reader was polled again after EOF");
                self.0 = true;
                Ok(0)
            }
        }
        require_trailing_zeros(&mut Once(false)).unwrap();
        require_trailing_zeros(&mut &b"\0\0"[..]).unwrap();
        assert!(require_trailing_zeros(&mut &b"\0x"[..]).is_err());
    }
}

#[cfg(test)]
mod layer_mode_tests {
    #[test]
    fn owner_access_is_added_without_removing_existing_permissions() {
        for (mode, expected) in [(0, 0o700), (0o700, 0o700), (0o2555, 0o2755), (0o077, 0o777)] {
            assert_eq!(super::writable_layer_mode(mode), expected);
        }
    }
}
