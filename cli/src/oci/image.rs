// SPDX-License-Identifier: AGPL-3.0-or-later
//! OCI image acquisition: resolve an image argument to an unpacked root
//! filesystem plus its runtime config (entrypoint/cmd/env).
//!
//! Two sources, no network code of our own:
//! - a path to a `docker save` tarball or an OCI image-layout directory;
//! - a registry reference, exported via whichever of `docker` or `podman`
//!   is installed. Registry auth, manifest negotiation, and multi-arch
//!   resolution stay in those tools.
//!
//! Layer application shells out to the system `tar` and handles OCI
//! whiteouts (`.wh.` entries) between layers. Every archive is extracted
//! into a fresh empty directory and merged from there, because `tar` follows
//! a symlink that stands in a destination path and an image can carry one
//! that points anywhere.

use serde::Deserialize;
use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

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
    #[error("image names a path outside its layout or rootfs: {0}")]
    UnsafePath(String),
    #[error("tar: {0}")]
    Tar(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// Runtime facts the guest needs to start the container.
#[derive(Debug, Clone, Default, serde::Serialize, Deserialize)]
pub struct RuntimeConfig {
    pub entrypoint: Vec<String>,
    pub cmd: Vec<String>,
    pub env: Vec<String>,
    pub working_dir: Option<String>,
}

/// An image staged on disk: an unpacked rootfs and its runtime config.
pub struct StagedImage {
    pub rootfs: PathBuf,
    pub config: RuntimeConfig,
}

/// The first installed container tool, used for pull/save and for the
/// content-addressed image ID the segment cache is keyed by.
pub fn tool() -> Option<&'static str> {
    ["docker", "podman"]
        .into_iter()
        .find(|tool| Command::new(tool).arg("--version").output().is_ok())
}

/// The local content-addressed image ID (`sha256:...`), if the image is
/// present in the tool's store.
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

/// Pull `image` if no tool has it locally; quiet best-effort (a stage that
/// still cannot find the image reports the real failure).
pub fn ensure_local(image: &str) {
    let Some(tool) = tool() else { return };
    if local_image_id(tool, image).is_none() {
        let _ = Command::new(tool).args(["pull", image]).status();
    }
}

/// Resolve `image` (path or registry reference) into `stage_dir/rootfs`.
pub fn stage(image: &str, stage_dir: &Path) -> Result<StagedImage, ImageError> {
    let tarball_path = Path::new(image);
    if tarball_path.exists() {
        stage_from_path(tarball_path, stage_dir)
    } else {
        let tarball = export_from_tool(image, stage_dir)?;
        stage_from_path(&tarball, stage_dir)
    }
}

/// Export a registry reference through docker or podman into a save-tarball.
fn export_from_tool(image: &str, stage_dir: &Path) -> Result<PathBuf, ImageError> {
    let tarball = stage_dir.join("image.tar");
    for tool in ["docker", "podman"] {
        if Command::new(tool).arg("--version").output().is_err() {
            continue;
        }
        // Pull only when the image is absent so a cached image stages
        // offline and without a registry round trip.
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

/// Unpack a docker-save tarball or OCI layout dir at `input`.
fn stage_from_path(input: &Path, stage_dir: &Path) -> Result<StagedImage, ImageError> {
    let layout = if input.is_dir() {
        input.to_path_buf()
    } else {
        let dir = stage_dir.join("layout");
        std::fs::create_dir_all(&dir)?;
        extract_checked(input, &dir)?;
        dir
    };

    // Both `docker save` (modern) and OCI layout carry index.json +
    // blobs/sha256/...; docker-save additionally has a legacy manifest.json,
    // which we prefer because it lists layers directly in order.
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
    for layer in &layers {
        apply_layer(layer, &rootfs)?;
    }
    Ok(StagedImage { rootfs, config })
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

/// The blob a descriptor digest names. The digest is untrusted input, and so
/// is the layout it indexes: the `algorithm/hex` path is checked on its own,
/// then resolved against the layout root, so neither the digest nor a
/// symlinked `blobs/` can move the read outside the image.
fn blob_path(layout: &Path, digest: &str) -> Result<PathBuf, ImageError> {
    let relative = digest.replace(':', "/");
    // Check the digest's own path before it is prefixed, so an absolute
    // digest cannot hide behind the prefix.
    check_relative(&relative)?;
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
}

fn parse_runtime_config(blob: &[u8]) -> Result<RuntimeConfig, ImageError> {
    let file: ImageConfigFile = serde_json::from_slice(blob)?;
    let section = file.config;
    Ok(section
        .map(|c| RuntimeConfig {
            entrypoint: c.entrypoint.unwrap_or_default(),
            cmd: c.cmd.unwrap_or_default(),
            env: c.env.unwrap_or_default(),
            working_dir: c.working_dir,
        })
        .unwrap_or_default())
}

/// Reject a path taken from an image (a tar member name, a manifest layer
/// reference, a blob digest) that could resolve outside the directory it is
/// joined to. `..`, an absolute root, and a path prefix are refused rather
/// than normalized away, because both `tar -x` and the whiteout deletions
/// act on these names.
fn check_relative(path: &str) -> Result<(), ImageError> {
    for component in Path::new(path).components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(ImageError::UnsafePath(path.to_string()));
            }
        }
    }
    Ok(())
}

/// Resolve an image-supplied relative path under `root` and prove the result
/// stays there. The lexical check refuses `..` and absolute names; resolving
/// what already exists refuses a layout carrying a symlink that points
/// outside, which no lexical check can see. The resolved path is returned so
/// the caller reads the location that was checked.
fn contained_join(root: &Path, relative: &str) -> Result<PathBuf, ImageError> {
    check_relative(relative)?;
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

/// `path` with every component that exists resolved through symlinks and the
/// components that do not exist yet appended, so containment can be checked
/// for a target that has not been created.
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

/// The comparison key for a member name: its `Normal` components joined and
/// ASCII-lowercased, so the member check also holds on a case-insensitive
/// host filesystem, where `Link` and `link` are one directory entry.
fn member_key(name: &str) -> String {
    Path::new(name)
        .components()
        .filter_map(|c| match c {
            Component::Normal(part) => Some(part.to_string_lossy().to_ascii_lowercase()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Refuse a member set that could escape the directory it is extracted into.
/// Names are checked lexically, and any member that another member is stored
/// under must itself be a directory member: `tar` writing to `link/pwn` when
/// the archive also carries `link` as a symlink writes wherever that link
/// points. `tar` lists a directory member with a trailing slash, which is how
/// the archive stores it.
fn check_members(entries: &[String]) -> Result<(), ImageError> {
    let mut directories: BTreeSet<String> = BTreeSet::new();
    let mut names: BTreeSet<String> = BTreeSet::new();
    let mut keys: Vec<(String, &String)> = Vec::new();
    for entry in entries {
        check_relative(entry)?;
        let key = member_key(entry);
        if key.is_empty() {
            continue;
        }
        if entry.ends_with('/') {
            directories.insert(key.clone());
        }
        names.insert(key.clone());
        keys.push((key, entry));
    }
    for (key, entry) in &keys {
        let mut prefix = String::new();
        for part in key.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            if prefix.len() == key.len() {
                break;
            }
            if names.contains(&prefix) && !directories.contains(&prefix) {
                return Err(ImageError::UnsafePath((*entry).clone()));
            }
        }
    }
    Ok(())
}

/// One `tar -tf` listing of an archive's member names.
fn tar_listing(archive: &Path) -> Result<Vec<String>, ImageError> {
    let out = Command::new("tar").arg("-tf").arg(archive).output()?;
    if !out.status.success() {
        return Err(ImageError::Tar(
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect())
}

/// Extract `archive` into `dest`, which must be a fresh empty directory.
/// Checked members landing in an empty tree cannot escape it: no member names
/// a path outside, and no symlink stands in any member's path.
fn extract_checked(archive: &Path, dest: &Path) -> Result<(), ImageError> {
    check_members(&tar_listing(archive)?)?;
    untar(archive, dest)
}

/// Move `staged`'s tree into `rootfs`, entry by entry. Each destination is
/// checked as it is used: a lower layer can leave a symlink where this layer
/// has a directory, and descending through it would write outside the
/// staging tree.
fn merge_layer(staged: &Path, rootfs: &Path) -> Result<(), ImageError> {
    let mut entries: Vec<_> = std::fs::read_dir(staged)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name();
        // Whiteout markers describe deletions; they are not image content.
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
            // An image may ship a directory its owner cannot write. Both
            // sides stay writable while this layer's children move across,
            // because moving an entry out of a directory needs write
            // permission on it. The target takes the layer's mode afterwards.
            let mode = meta.permissions().mode() & 0o7777;
            let open = std::fs::Permissions::from_mode(mode | 0o700);
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

/// The real path a whiteout entry names under `rootfs`, or `None` when
/// nothing is there to delete. The parent is resolved through symlinks and
/// checked against the real staging root: a lower layer can leave a symlink
/// pointing outside the staging tree, and a whiteout naming a path through it
/// would otherwise delete the symlink's target.
fn resolve_under(rootfs: &Path, relative: &Path) -> Result<Option<PathBuf>, ImageError> {
    let Ok(real_root) = rootfs.canonicalize() else {
        return Ok(None);
    };
    let Some(name) = relative.file_name() else {
        // The layer's own root, named by a top-level opaque whiteout.
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
        .arg("-xf")
        .arg(tarball)
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

/// Apply one layer tar to `rootfs`, honoring OCI whiteouts: a `.wh.<name>`
/// entry deletes `<name>` from lower layers; `.wh..wh..opq` clears the
/// directory's prior contents.
///
/// The layer is extracted into a fresh directory and merged in, never
/// extracted over the rootfs directly: `tar` follows a symlink standing in a
/// destination path, and a lower layer can leave one pointing anywhere.
fn apply_layer(layer: &Path, rootfs: &Path) -> Result<(), ImageError> {
    let entries = tar_listing(layer)?;
    check_members(&entries)?;
    apply_whiteouts(&entries, rootfs)?;
    let staging = rootfs.parent().unwrap_or(rootfs);
    let staged = tempfile::tempdir_in(staging)?;
    untar(layer, staged.path())?;
    merge_layer(staged.path(), rootfs)
}

/// Delete what this layer's whiteout entries mark, before its own content
/// lands.
fn apply_whiteouts(entries: &[String], rootfs: &Path) -> Result<(), ImageError> {
    for entry in entries {
        let path = Path::new(entry);
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
            // Clearing a directory that is really a symlink would clear the
            // link's target, which can sit outside the staging tree.
            if meta.is_symlink() {
                return Err(ImageError::UnsafePath(entry.clone()));
            }
            if meta.is_dir() {
                std::fs::remove_dir_all(&dir)?;
                std::fs::create_dir_all(&dir)?;
            }
        } else if let Some(hidden) = name.strip_prefix(".wh.") {
            let Some(target) = resolve_under(rootfs, &path.with_file_name(hidden))? else {
                continue;
            };
            // Read the target's own type, never the type it points at: a
            // whiteout deletes the entry, not what a symlink resolves to.
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
            "Env":["A=1"],"WorkingDir":"/w"},"rootfs":{}}"#;
        let config = parse_runtime_config(blob).unwrap();
        assert_eq!(config.entrypoint, ["/e"]);
        assert_eq!(config.cmd, ["run"]);
        assert_eq!(config.env, ["A=1"]);
        assert_eq!(config.working_dir.as_deref(), Some("/w"));

        let empty = parse_runtime_config(b"{}").unwrap();
        assert!(empty.entrypoint.is_empty() && empty.cmd.is_empty());
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

    /// A layout is untrusted too: a symlinked `blobs/` tree, or a symlinked
    /// blob, reaches outside the layout without ever spelling `..`.
    #[test]
    fn blob_path_refuses_a_symlinked_layout() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside/sha256");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("m"), b"secret").unwrap();

        // The blobs directory itself points out of the layout.
        let via_dir = dir.path().join("via-dir");
        std::fs::create_dir_all(&via_dir).unwrap();
        std::os::unix::fs::symlink(dir.path().join("outside"), via_dir.join("blobs")).unwrap();
        let err = blob_path(&via_dir, "sha256:m").unwrap_err();
        assert!(matches!(err, ImageError::UnsafePath(_)), "{err:?}");

        // One blob points out of the layout.
        let via_blob = dir.path().join("via-blob");
        std::fs::create_dir_all(via_blob.join("blobs/sha256")).unwrap();
        std::os::unix::fs::symlink(outside.join("m"), via_blob.join("blobs/sha256/m")).unwrap();
        let err = blob_path(&via_blob, "sha256:m").unwrap_err();
        assert!(matches!(err, ImageError::UnsafePath(_)), "{err:?}");
    }

    /// The same for the OCI layout entry point and its blob reads.
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

        // A real index whose manifest blob is a symlink out of the layout.
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

    /// A docker-save layout can point its manifest or a layer out of the
    /// layout with a symlink instead of a `..` path.
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

        // A real manifest whose layer reference is a symlink out of the layout.
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

    /// A digest is untrusted: an OCI layout from anywhere can name one that
    /// walks out of `blobs/`.
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

    /// The same for a docker-save manifest, whose layer and config entries
    /// are plain strings read out of the tarball.
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

    /// The outer docker-save tarball is untrusted too: a traversing member in
    /// it must be refused before anything is written.
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

    /// A ustar archive whose member names and types are stored verbatim.
    /// `tar -cf` cannot produce a traversing member name, nor a symlink and a
    /// member stored under it, so the planted-attack tests write the headers
    /// themselves. Members are `(name, typeflag, linkname)`; typeflag `0` is a
    /// regular file, `2` a symlink, `5` a directory.
    fn raw_tar(dir: &Path, name: &str, members: &[(&str, u8, &str)]) -> PathBuf {
        let mut archive: Vec<u8> = Vec::new();
        for (member, typeflag, linkname) in members {
            let mut header = [b'\0'; 512];
            let mut put = |offset: usize, bytes: &[u8]| {
                header[offset..offset + bytes.len()].copy_from_slice(bytes);
            };
            put(0, member.as_bytes());
            put(100, b"0000644\0"); // mode
            put(108, b"0000000\0"); // uid
            put(116, b"0000000\0"); // gid
            put(124, b"00000000000\0"); // size
            put(136, b"00000000000\0"); // mtime
            put(148, b"        "); // checksum field, spaces while summing
            put(156, &[*typeflag]);
            put(157, linkname.as_bytes());
            put(257, b"ustar\0");
            put(263, b"00");
            let sum: u32 = header.iter().map(|b| u32::from(*b)).sum();
            header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
            archive.extend_from_slice(&header);
        }
        // The two zero blocks that end an archive.
        archive.extend_from_slice(&[0u8; 1024]);
        let tarball = dir.join(name);
        std::fs::write(&tarball, archive).unwrap();
        tarball
    }

    fn tar_with_raw_name(dir: &Path, name: &str, member: &str) -> PathBuf {
        raw_tar(dir, name, &[(member, b'0', "")])
    }

    /// A rootfs with an outside directory beside it holding a sentinel file.
    fn planted(dir: &Path) -> (PathBuf, PathBuf) {
        let rootfs = dir.join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();
        let victim = dir.join("victim");
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(victim.join("sentinel"), b"survives").unwrap();
        (rootfs, victim)
    }

    /// A layer member that walks out of the rootfs is refused, and nothing
    /// outside the staging tree is touched. Both whiteout forms are planted:
    /// the opaque marker (`remove_dir_all`) and the sibling delete
    /// (`remove_file`).
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
            let err = apply_layer(&layer, &rootfs).unwrap_err();
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

    /// A whiteout that reaches outside through a symlink a lower layer left
    /// behind is refused: the deletion resolves the parent and checks it
    /// against the real staging root.
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
            let err = apply_layer(&layer, &rootfs).unwrap_err();
            assert!(
                matches!(err, ImageError::UnsafePath(_)),
                "{member} gave {err:?}"
            );
            assert!(
                victim.join("sentinel").is_file(),
                "{member} deleted outside"
            );
        }
        // The symlink itself is still a whiteout-able entry in the rootfs.
        assert!(std::fs::symlink_metadata(rootfs.join("link")).is_ok());
    }

    /// A whiteout naming a symlink deletes the link, never what it points at.
    #[test]
    fn whiteout_of_a_symlink_removes_only_the_link() {
        let dir = tempfile::tempdir().unwrap();
        let (rootfs, victim) = planted(dir.path());
        std::os::unix::fs::symlink(&victim, rootfs.join("link")).unwrap();

        let layer = tar_with_raw_name(dir.path(), "wh-link.tar", ".wh.link");
        apply_layer(&layer, &rootfs).unwrap();
        assert!(std::fs::symlink_metadata(rootfs.join("link")).is_err());
        assert!(victim.join("sentinel").is_file());
        assert!(victim.is_dir());
    }

    /// An ordinary member stored under a symlink a lower layer left behind
    /// must not be written through it. The layer names nothing suspicious;
    /// only the destination the merge resolves is dangerous.
    #[test]
    fn apply_layer_refuses_a_member_under_a_lower_layer_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let (rootfs, victim) = planted(dir.path());

        // Lower layer: a symlink pointing out of the rootfs.
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
        apply_layer(&base, &rootfs).unwrap();
        assert!(
            std::fs::symlink_metadata(rootfs.join("link"))
                .unwrap()
                .is_symlink()
        );

        // Upper layer: an ordinary file stored under that name.
        let upper = tar_layer(dir.path(), "upper", &[("link/pwn", b"owned")]);
        let err = apply_layer(&upper, &rootfs).unwrap_err();
        assert!(matches!(err, ImageError::UnsafePath(_)), "{err:?}");
        assert!(!victim.join("pwn").exists(), "wrote outside the rootfs");
        assert!(victim.join("sentinel").is_file());
    }

    /// The same attack inside one layer: the archive carries the symlink and
    /// the member stored under it, so no lower layer is needed. The layer is
    /// refused before `tar` runs.
    #[test]
    fn apply_layer_refuses_a_symlink_and_a_member_under_it_in_one_layer() {
        let dir = tempfile::tempdir().unwrap();
        let (rootfs, victim) = planted(dir.path());
        for (n, members) in [
            vec![("link", b'2', "../../victim"), ("link/pwn", b'0', "")],
            vec![("link", b'2', "/etc"), ("link/pwn", b'0', "")],
            // Case-insensitive host filesystems make these one entry.
            vec![("Link", b'2', "../../victim"), ("link/pwn", b'0', "")],
            // A regular file used as a directory is refused on the same rule.
            vec![("link", b'0', ""), ("link/pwn", b'0', "")],
        ]
        .iter()
        .enumerate()
        {
            let layer = raw_tar(dir.path(), &format!("same{n}.tar"), members);
            let err = apply_layer(&layer, &rootfs).unwrap_err();
            assert!(
                matches!(err, ImageError::UnsafePath(_)),
                "{members:?}: {err:?}"
            );
            assert!(!victim.join("pwn").exists(), "{members:?} wrote outside");
            assert!(victim.join("sentinel").is_file(), "{members:?}");
        }
    }

    /// A directory member and its children are the ordinary case the member
    /// check must not refuse, and symlinks inside an image survive the merge.
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
        apply_layer(&layer, &rootfs).unwrap();
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

    /// An image may ship a directory its owner cannot write. Its contents
    /// must still land, and its mode must survive the merge.
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
        apply_layer(&layer, &rootfs).unwrap();
        assert_eq!(std::fs::read(rootfs.join("ro/x")).unwrap(), b"content");
        let mode = std::fs::symlink_metadata(rootfs.join("ro"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o555);
        // Leave the trees removable for the temporary directory's cleanup.
        for path in [stage.join("ro"), rootfs.join("ro")] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// A later layer replaces an earlier file, and a directory it ships over
    /// an earlier file replaces that too.
    #[test]
    fn apply_layer_replaces_earlier_entries() {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();
        let base = tar_layer(dir.path(), "one", &[("f", b"old"), ("d/x", b"x")]);
        apply_layer(&base, &rootfs).unwrap();
        let upper = tar_layer(dir.path(), "two", &[("f", b"new"), ("d/y", b"y")]);
        apply_layer(&upper, &rootfs).unwrap();
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
        apply_layer(&base, &rootfs).unwrap();
        assert!(rootfs.join("keep.txt").is_file());
        assert!(rootfs.join("d/old").is_file());

        // Upper layer: delete gone.txt, opaque-clear d/, add d/new.
        let upper = tar_layer(
            dir.path(),
            "upper",
            &[
                (".wh.gone.txt", b""),
                ("d/.wh..wh..opq", b""),
                ("d/new", b"new"),
            ],
        );
        apply_layer(&upper, &rootfs).unwrap();
        assert!(rootfs.join("keep.txt").is_file());
        assert!(!rootfs.join("gone.txt").exists());
        assert!(!rootfs.join("d/old").exists());
        assert_eq!(std::fs::read(rootfs.join("d/new")).unwrap(), b"new");
        assert!(!rootfs.join("d/.wh..wh..opq").exists());
    }
}
