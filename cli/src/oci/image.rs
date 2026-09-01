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
//! whiteouts (`.wh.` entries) between layers.

use serde::Deserialize;
use std::path::{Path, PathBuf};
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
        untar(input, &dir, &[])?;
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
        serde_json::from_slice(&std::fs::read(layout.join("manifest.json"))?)?;
    let entry = manifest.into_iter().next().ok_or(ImageError::NoConfig)?;
    let layers = entry.layers.iter().map(|l| layout.join(l)).collect();
    let config = std::fs::read(layout.join(&entry.config))?;
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

fn blob_path(layout: &Path, digest: &str) -> PathBuf {
    layout.join("blobs").join(digest.replace(':', "/"))
}

fn oci_layout_layers(layout: &Path) -> Result<(Vec<PathBuf>, Vec<u8>), ImageError> {
    let index: OciIndex = serde_json::from_slice(&std::fs::read(layout.join("index.json"))?)?;
    let first = index.manifests.first().ok_or(ImageError::NoConfig)?;
    let manifest: OciManifest =
        serde_json::from_slice(&std::fs::read(blob_path(layout, &first.digest))?)?;
    let layers = manifest
        .layers
        .iter()
        .map(|l| blob_path(layout, &l.digest))
        .collect();
    let config = std::fs::read(blob_path(layout, &manifest.config.digest))?;
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

fn untar(tarball: &Path, dest: &Path, extra: &[&str]) -> Result<(), ImageError> {
    let out = Command::new("tar")
        .arg("-xf")
        .arg(tarball)
        .arg("-C")
        .arg(dest)
        .args(extra)
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
fn apply_layer(layer: &Path, rootfs: &Path) -> Result<(), ImageError> {
    let listing = Command::new("tar").arg("-tf").arg(layer).output()?;
    if !listing.status.success() {
        return Err(ImageError::Tar(
            String::from_utf8_lossy(&listing.stderr).into_owned(),
        ));
    }
    for entry in String::from_utf8_lossy(&listing.stdout).lines() {
        let path = Path::new(entry);
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name == ".wh..wh..opq" {
            if let Some(parent) = path.parent() {
                let dir = rootfs.join(parent);
                if dir.is_dir() {
                    std::fs::remove_dir_all(&dir)?;
                    std::fs::create_dir_all(&dir)?;
                }
            }
        } else if let Some(hidden) = name.strip_prefix(".wh.") {
            let target = rootfs.join(path.with_file_name(hidden));
            if target.is_dir() {
                std::fs::remove_dir_all(&target)?;
            } else if target.exists() {
                std::fs::remove_file(&target)?;
            }
        }
    }
    // tar auto-detects gzip/zstd compression on extraction.
    let out = Command::new("tar")
        .arg("-xf")
        .arg(layer)
        .arg("-C")
        .arg(rootfs)
        .args(["--exclude", "*.wh.*", "--exclude", ".wh.*"])
        .output()?;
    if !out.status.success() {
        return Err(ImageError::Tar(
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ));
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
        assert_eq!(
            blob_path(Path::new("/l"), "sha256:abc"),
            Path::new("/l/blobs/sha256/abc")
        );
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
        assert_eq!(
            layers,
            [dir.path().join("l1.tar"), dir.path().join("l2.tar")]
        );
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
        assert_eq!(layers, [blobs.join("l")]);
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
    fn untar_extracts_into_dest() {
        let dir = tempfile::tempdir().unwrap();
        let tarball = tar_layer(dir.path(), "t", &[("hello.txt", b"hi")]);
        let dest = dir.path().join("out");
        std::fs::create_dir_all(&dest).unwrap();
        untar(&tarball, &dest, &[]).unwrap();
        assert_eq!(std::fs::read(dest.join("hello.txt")).unwrap(), b"hi");
        assert!(untar(Path::new("/nonexistent.tar"), &dest, &[]).is_err());
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
