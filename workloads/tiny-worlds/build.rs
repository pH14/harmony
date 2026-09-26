// SPDX-License-Identifier: AGPL-3.0-or-later

use sha2::{Digest, Sha256};
use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn sources(root: &Path, path: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(path).expect("source directory") {
        let path = entry.expect("source entry").path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "src")
                || path.starts_with(root.join("src"))
            {
                sources(root, &path, files);
            }
        } else if path.extension().is_some_and(|ext| ext == "rs")
            || path
                .file_name()
                .is_some_and(|name| name == "Cargo.toml" || name == "Cargo.lock")
        {
            files.push(path);
        }
    }
}

fn digest(root: &Path, workspace: bool) -> String {
    let mut files = Vec::new();
    sources(root, root, &mut files);
    if workspace {
        files.push(root.join("../Cargo.toml"));
    }
    files.sort();
    let mut hash = Sha256::new();
    for path in files {
        println!("cargo:rerun-if-changed={}", path.display());
        hash.update(
            path.strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .as_bytes(),
        );
        hash.update([0]);
        hash.update(fs::read(path).expect("source bytes"));
        hash.update([0]);
    }
    format!("{:x}", hash.finalize())
}

fn main() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    println!(
        "cargo:rustc-env=TINY_ENGINE_SOURCE_SHA256={}",
        digest(&root.join("../../dissonance/searcher"), true)
    );
    println!(
        "cargo:rustc-env=TINY_WORKLOAD_SOURCE_SHA256={}",
        digest(&root, false)
    );
}
