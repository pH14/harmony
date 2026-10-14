// SPDX-License-Identifier: AGPL-3.0-or-later

use oci_support::{bundle, image};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn image_files(
    root: &Path,
    directory: &Path,
    files: &mut BTreeMap<String, Value>,
) -> Result<(), Box<dyn std::error::Error>> {
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            image_files(root, &path, files)?;
        } else if metadata.is_file() {
            let bytes = std::fs::read(&path)?;
            files.insert(
                path.strip_prefix(root)?.to_string_lossy().into_owned(),
                json!({"sha256": sha(&bytes), "size": bytes.len()}),
            );
        } else {
            return Err(format!("unsupported OCI input entry: {}", path.display()).into());
        }
    }
    Ok(())
}

fn postgres_code_inputs(
    staged: &image::StagedImage,
) -> Result<BTreeMap<String, Value>, Box<dyn std::error::Error>> {
    image::with_staged_read_access(staged, || -> Result<_, Box<dyn std::error::Error>> {
        let mut code_inputs = BTreeMap::new();
        let root = staged.rootfs.canonicalize()?;
        for name in ["/workload.sql", "/usr/local/bin/postgres-workload.sh"] {
            let path = root.join(name.trim_start_matches('/')).canonicalize()?;
            if !path.starts_with(&root) {
                return Err(format!("PostgreSQL source escapes the staged image: {name}").into());
            }
            let bytes = std::fs::read(path)?;
            code_inputs.insert(
                name.to_owned(),
                json!({"sha256": sha(&bytes), "size": bytes.len()}),
            );
        }
        Ok(code_inputs)
    })
}

pub fn dump(args: &[String], session_json: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    if args.len() < 5
        || !matches!(args[0].as_str(), "nes" | "postgres")
        || args.len() != if args[0] == "nes" { 6 } else { 5 }
    {
        return Err("usage: prepare-admission nes KERNEL PLATFORM OCI OUTPUT ROM | postgres KERNEL PLATFORM OCI OUTPUT".into());
    }
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        return Err("admission dump supports only the default Linux x86_64 KVM session".into());
    }
    let mode = &args[0];
    let kernel = std::fs::read(&args[1])?;
    let platform = std::fs::read(&args[2])?;
    let image_path = PathBuf::from(&args[3]).canonicalize()?;
    if !image_path.is_dir() || !image_path.join("oci-layout").is_file() {
        return Err("admission dump requires a local OCI layout directory".into());
    }
    let output = PathBuf::from(&args[4]);
    if output.exists() {
        return Err("output directory must not exist".into());
    }
    let output_parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()?;
    if output_parent.starts_with(&image_path) {
        return Err("output must stay outside the OCI input".into());
    }
    let mut inputs = BTreeMap::new();
    image_files(&image_path, &image_path, &mut inputs)?;
    let stage = tempfile::tempdir()?;
    let staged = image::stage(
        image_path.to_str().ok_or("non-UTF8 OCI path")?,
        stage.path(),
    )?;
    let rom = if mode == "nes" {
        Some(std::fs::read(&args[5])?)
    } else {
        None
    };
    let prepared = if let Some(rom) = &rom {
        crate::prepare::prepare_oci(&staged, rom)?
    } else {
        bundle::prepare(&staged, &bundle::LaunchRequest::new(Vec::new()))?
    };
    let code_inputs = if mode == "postgres" {
        postgres_code_inputs(&staged)?
    } else {
        BTreeMap::new()
    };
    let execution = prepared.execution.to_vec()?;
    let composed = prepared.initramfs(&platform);
    let runtime_config = serde_json::to_vec(&staged.config)?;
    let input_manifest = serde_json::to_vec(&inputs)?;
    let blobs: [(&str, &[u8]); 8] = [
        ("kernel.bin", &kernel),
        ("session-config.json", session_json),
        ("platform.cpio.gz", &platform),
        ("rootfs.cpio.gz", &prepared.rootfs_segment),
        ("control.cpio.gz", &prepared.control_segment),
        ("execution.json", &execution),
        ("image-runtime-config.json", &runtime_config),
        ("image-inputs.json", &input_manifest),
    ];
    std::fs::create_dir_all(&output)?;
    let mut files = BTreeMap::new();
    for (name, bytes) in blobs {
        std::fs::write(output.join(name), bytes)?;
        files.insert(name, json!({"sha256": sha(bytes), "size": bytes.len()}));
    }
    std::fs::write(output.join("composed-initramfs.bin"), &composed)?;
    files.insert(
        "composed-initramfs.bin",
        json!({"sha256": sha(&composed), "size": composed.len()}),
    );
    let manifest = json!({
        "version": 1, "mode": mode, "prepared_identity": prepared.identity_hex(),
        "engine_scope": "default-linux-x86_64-kvm-session",
        "files": files,
        "composition": [
            {"file": "platform.cpio.gz", "offset": 0, "size": platform.len()},
            {"file": "rootfs.cpio.gz", "offset": platform.len(), "size": prepared.rootfs_segment.len()},
            {"file": "control.cpio.gz", "offset": platform.len() + prepared.rootfs_segment.len(), "size": prepared.control_segment.len()}
        ],
        "external_inputs": rom.as_ref().map(|bytes| json!({"/game.nes": {"sha256": sha(bytes), "size": bytes.len()}})).unwrap_or_else(|| json!({})),
        "code_inputs": code_inputs,
        "scope": "controlled NES or PostgreSQL; candidate dump, not approval"
    });
    std::fs::write(
        output.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    println!("{}", output.join("manifest.json").display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use oci_support::image::{Ownership, RuntimeConfig};
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn postgres_code_inputs_read_unreadable_staged_files_and_restore_modes() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("usr/local/bin")).unwrap();
        std::fs::write(root.path().join("workload.sql"), b"SELECT 1;").unwrap();
        std::fs::write(
            root.path().join("usr/local/bin/postgres-workload.sh"),
            b"run",
        )
        .unwrap();
        for path in [
            "workload.sql",
            "usr/local/bin/postgres-workload.sh",
            "usr/local/bin",
        ] {
            std::fs::set_permissions(root.path().join(path), std::fs::Permissions::from_mode(0o0))
                .unwrap();
        }
        let staged = image::StagedImage {
            rootfs: root.path().to_path_buf(),
            owners: Ownership::default(),
            config: RuntimeConfig::default(),
        };
        let inputs = postgres_code_inputs(&staged).unwrap();
        assert_eq!(inputs["/workload.sql"]["sha256"], sha(b"SELECT 1;"));
        for path in ["workload.sql", "usr/local/bin"] {
            let mode = std::fs::symlink_metadata(root.path().join(path))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777;
            assert_eq!(mode, 0, "{path}");
        }
        std::fs::set_permissions(
            root.path().join("usr/local/bin"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let mode =
            std::fs::symlink_metadata(root.path().join("usr/local/bin/postgres-workload.sh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777;
        assert_eq!(mode, 0);
    }

    #[test]
    fn postgres_code_inputs_reject_host_symlink_targets() {
        let root = tempfile::tempdir().unwrap();
        let host = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("usr/local/bin")).unwrap();
        std::fs::write(
            root.path().join("usr/local/bin/postgres-workload.sh"),
            b"run",
        )
        .unwrap();
        let outside = host.path().join("host.sql");
        std::fs::write(&outside, b"host secret").unwrap();
        std::os::unix::fs::symlink(&outside, root.path().join("workload.sql")).unwrap();
        let staged = image::StagedImage {
            rootfs: root.path().to_path_buf(),
            owners: Ownership::default(),
            config: RuntimeConfig::default(),
        };
        assert!(postgres_code_inputs(&staged).is_err());
        std::fs::remove_file(root.path().join("workload.sql")).unwrap();
        std::fs::write(root.path().join("safe.sql"), b"SELECT 1;").unwrap();
        std::os::unix::fs::symlink("safe.sql", root.path().join("workload.sql")).unwrap();
        let inputs = postgres_code_inputs(&staged).unwrap();
        assert_eq!(inputs["/workload.sql"]["sha256"], sha(b"SELECT 1;"));
    }
}
