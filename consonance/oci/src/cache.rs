// SPDX-License-Identifier: AGPL-3.0-or-later

use super::image::{self, RuntimeConfig};
use std::path::{Path, PathBuf};

const FORMAT: &str = "v2";

pub fn dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("HARMONY_CACHE_DIR") {
        return Some(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".cache/harmony/segments"))
}

pub fn key(image: &str) -> Option<String> {
    let tool = image::tool()?;
    let id = image::local_image_id(tool, image)?;
    Some(format!("{}-{FORMAT}", id.replace([':', '/'], "_")))
}

pub fn load(dir: &Path, key: &str) -> Option<(Vec<u8>, RuntimeConfig)> {
    let segment = std::fs::read(dir.join(format!("{key}.cpio.gz"))).ok()?;
    let config = std::fs::read(dir.join(format!("{key}.json"))).ok()?;
    let config: RuntimeConfig = serde_json::from_slice(&config).ok()?;
    Some((segment, config))
}

pub fn store(dir: &Path, key: &str, segment: &[u8], config: &RuntimeConfig) {
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let Ok(json) = serde_json::to_vec_pretty(config) else {
        return;
    };
    let _ = write_atomic(dir, &format!("{key}.json"), &json);
    let _ = write_atomic(dir, &format!("{key}.cpio.gz"), segment);
}

fn write_atomic(dir: &Path, name: &str, data: &[u8]) -> std::io::Result<()> {
    let tmp = dir.join(format!(".{name}.tmp.{}", std::process::id()));
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, dir.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_resolves_from_environment() {
        match std::env::var("HARMONY_CACHE_DIR") {
            Ok(set) => assert_eq!(dir(), Some(PathBuf::from(set))),
            Err(_) => {
                let got = dir().unwrap();
                assert!(got.ends_with(".cache/harmony/segments"));
            }
        }
    }

    #[test]
    fn store_then_load_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("segments");
        let config = RuntimeConfig {
            cmd: vec!["service".into()],
            ..RuntimeConfig::default()
        };
        assert!(load(&cache, "k-v2").is_none());
        store(&cache, "k-v2", b"segment-bytes", &config);
        let (segment, loaded) = load(&cache, "k-v2").unwrap();
        assert_eq!(segment, b"segment-bytes");
        assert_eq!(loaded.cmd, ["service"]);
        let stray: Vec<_> = std::fs::read_dir(&cache)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with('.')
            })
            .collect();
        assert!(stray.is_empty());
    }
}
