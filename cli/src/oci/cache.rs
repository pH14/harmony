// SPDX-License-Identifier: AGPL-3.0-or-later
//! Rootfs-segment cache. The gzip-compressed rootfs segment is a pure
//! function of the unpacked image tree, so it is cached under the container
//! tool's content-addressed image ID; a new pull yields a new ID and thus a
//! new entry. The runtime config staged from the image travels with it.
//! Bumping `FORMAT` orphans old entries whenever the segment layout changes.

use super::image::{self, RuntimeConfig};
use std::path::{Path, PathBuf};

const FORMAT: &str = "v1";

fn cache_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("HARMONY_CACHE_DIR") {
        return Some(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".cache/harmony/segments"))
}

/// The content-addressed cache key for a registry image, if one is
/// resolvable: requires a container tool and the image present locally.
pub fn key(image: &str) -> Option<String> {
    let tool = image::tool()?;
    let id = image::local_image_id(tool, image)?;
    Some(format!("{}-{FORMAT}", id.replace([':', '/'], "_")))
}

pub fn load(key: &str) -> Option<(Vec<u8>, RuntimeConfig)> {
    let dir = cache_dir()?;
    let segment = std::fs::read(dir.join(format!("{key}.cpio.gz"))).ok()?;
    let config = std::fs::read(dir.join(format!("{key}.json"))).ok()?;
    let config: RuntimeConfig = serde_json::from_slice(&config).ok()?;
    Some((segment, config))
}

/// Best-effort atomic store; a failed write only costs the next run a
/// restage.
pub fn store(key: &str, segment: &[u8], config: &RuntimeConfig) {
    let Some(dir) = cache_dir() else { return };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let Ok(json) = serde_json::to_vec_pretty(config) else {
        return;
    };
    let _ = write_atomic(&dir, &format!("{key}.json"), &json);
    let _ = write_atomic(&dir, &format!("{key}.cpio.gz"), segment);
}

fn write_atomic(dir: &Path, name: &str, data: &[u8]) -> std::io::Result<()> {
    let tmp = dir.join(format!(".{name}.tmp.{}", std::process::id()));
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, dir.join(name))
}
