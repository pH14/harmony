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

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 5
        || !matches!(args[0].as_str(), "nes" | "postgres")
        || args.len() != if args[0] == "nes" { 6 } else { 5 }
    {
        return Err("usage: prepare-admission nes KERNEL PLATFORM OCI OUTPUT ROM | postgres KERNEL PLATFORM OCI OUTPUT".into());
    }
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        return Err("admission dump supports only the default Linux x86_64 KVM session".into());
    }
    let session = consonance_client::session::SessionConfig::default();
    let session_json = serde_json::to_vec(&session)?;
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
        nes_workload::prepare::prepare_oci(&staged, rom)?
    } else {
        bundle::prepare(&staged, &bundle::LaunchRequest::new(Vec::new()))?
    };
    let mut code_inputs = BTreeMap::new();
    if mode == "postgres" {
        for name in ["/workload.sql", "/usr/local/bin/postgres-workload.sh"] {
            let bytes = std::fs::read(staged.rootfs.join(name.trim_start_matches('/')))?;
            code_inputs.insert(
                name.to_owned(),
                json!({"sha256": sha(&bytes), "size": bytes.len()}),
            );
        }
    }
    let execution = prepared.execution.to_vec()?;
    let composed = prepared.initramfs(&platform);
    let runtime_config = serde_json::to_vec(&staged.config)?;
    let input_manifest = serde_json::to_vec(&inputs)?;
    let blobs: [(&str, &[u8]); 8] = [
        ("kernel.bin", &kernel),
        ("session-config.json", &session_json),
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

fn main() {
    if let Err(error) = run() {
        eprintln!("prepare-admission: {error}");
        std::process::exit(1);
    }
}
