// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::credentials;
pub use crate::credentials::{CredentialsError, ProcessCredentials};
use guest_image::Owner;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
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

    fn remove_tree(&mut self, path: &Path) {
        let path = normalized(path);
        self.owners
            .retain(|recorded, _| !recorded.starts_with(&path));
    }

    fn remove_below(&mut self, path: &Path) {
        let path = normalized(path);
        self.owners
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

pub fn ensure_local(image: &str) {
    let Some(tool) = tool() else { return };
    if local_image_id(tool, image).is_none() {
        let _ = Command::new(tool).args(["pull", image]).status();
    }
}

pub fn stage(image: &str, stage_dir: &Path) -> Result<StagedImage, ImageError> {
    let tarball_path = Path::new(image);
    if tarball_path.exists() {
        stage_from_path(tarball_path, stage_dir)
    } else {
        let tarball = export_from_tool(image, stage_dir)?;
        stage_from_path(&tarball, stage_dir)
    }
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
    let layout = if input.is_dir() {
        input.to_path_buf()
    } else {
        let dir = stage_dir.join("layout");
        std::fs::create_dir_all(&dir)?;
        extract_checked(input, &dir)?;
        dir
    };

    let (layers, config_blob) = if layout.join("manifest.json").is_file() {
        docker_save_layers(&layout)?
    } else if layout.join("index.json").is_file() {
        oci_layout_layers(&layout)?
    } else {
        return Err(ImageError::Unrecognized(input.to_path_buf()));
    };

    let config = parse_runtime_config(&config_blob)?;
    let rootfs = stage_dir.join("rootfs");
    std::fs::create_dir_all(&rootfs)?;
    let mut owners = Ownership::default();
    for layer in &layers {
        apply_layer(layer, &rootfs, &mut owners)?;
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
    let resolved = resolve_deepest(&real_root.join(relative))?;
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
    Ok(())
}

const TAR_BLOCK: usize = 512;
const MAX_MEMBERS: usize = 1 << 20;
const MAX_HEADER_DATA: u64 = 1 << 16;

struct Member {
    name: Vec<u8>,
    is_dir: bool,
    owner: Owner,
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
    let mut pax = Pax::default();
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
                header_data(&mut stream, size)?;
            }
            b'x' => {
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
                } = std::mem::take(&mut pax);
                let gnu_name = long_name.take();
                let owner = Owner {
                    uid: owner_id(uid, &block[108..116])?,
                    gid: owner_id(gid, &block[116..124])?,
                };
                members.push(Member {
                    name: path.or(gnu_name).unwrap_or_else(|| ustar_name(&block)),
                    is_dir: typeflag == b'5',
                    owner,
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
    let field = |from: usize, to: usize| {
        block[from..to]
            .split(|byte| *byte == 0)
            .next()
            .unwrap_or_default()
            .to_vec()
    };
    let name = field(0, 100);
    if &block[257..263] != b"ustar\0" {
        return name;
    }
    let prefix = field(345, 500);
    if prefix.is_empty() {
        return name;
    }
    [prefix, vec![b'/'], name].concat()
}

#[derive(Default)]
struct Pax {
    path: Option<Vec<u8>>,
    size: Option<u64>,
    uid: Option<u64>,
    gid: Option<u64>,
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
    check_members(&read_members(archive)?)?;
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
    let Ok(real_root) = rootfs.canonicalize() else {
        return Ok(None);
    };
    let Some(name) = relative.file_name() else {
        return Ok(Some(real_root));
    };
    let parent = rootfs.join(relative.parent().unwrap_or(Path::new("")));
    let Ok(real_parent) = parent.canonicalize() else {
        return Ok(None);
    };
    if !real_parent.starts_with(&real_root) {
        return Err(ImageError::UnsafePath(relative.display().to_string()));
    }
    Ok(Some(real_parent.join(name)))
}

fn untar(tarball: &Path, dest: &Path) -> Result<(), ImageError> {
    let out = Command::new("tar")
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

fn apply_layer(layer: &Path, rootfs: &Path, owners: &mut Ownership) -> Result<(), ImageError> {
    let members = read_members(layer)?;
    check_members(&members)?;
    apply_whiteouts(&members, rootfs)?;
    let staging = rootfs.parent().unwrap_or(rootfs);
    let staged = tempfile::tempdir_in(staging)?;
    untar(layer, staged.path())?;
    merge_layer(staged.path(), rootfs)?;
    record_owners(&members, owners);
    Ok(())
}

fn record_owners(members: &[Member], owners: &mut Ownership) {
    for member in members {
        let path = member.path();
        match whiteout(path) {
            Some(Whiteout::Opaque(dir)) => owners.remove_below(dir),
            Some(Whiteout::Entry(target)) => owners.remove_tree(&target),
            None => {}
        }
    }
    for member in members {
        let path = member.path();
        if whiteout(path).is_some() {
            continue;
        }
        if !member.is_dir {
            owners.remove_tree(path);
        }
        owners.record(path, member.owner);
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
            Err(ImageError::UnsafePath(_))
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
                ("pad\nlink/", b'0', ""),
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
