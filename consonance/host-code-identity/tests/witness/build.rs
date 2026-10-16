// SPDX-License-Identifier: AGPL-3.0-or-later

use std::path::PathBuf;
use std::process::Command;

fn compile(source: &str, output: PathBuf, definitions: &[&str]) {
    std::fs::create_dir_all(output.parent().unwrap()).unwrap();
    let mut command = Command::new("cc");
    command.args([
        "-shared",
        "-fPIC",
        "-O2",
        "-Wl,-z,separate-code",
        "-Wl,-soname,libwitness.so",
    ]);
    for definition in definitions {
        command.arg(format!("-D{definition}"));
    }
    assert!(
        command
            .arg(source)
            .arg("-o")
            .arg(output)
            .status()
            .unwrap()
            .success(),
        "failed to compile loaded-code witness library"
    );
}

fn main() {
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    compile(
        "witness.c",
        out.join("A/libwitness.so"),
        &["WITNESS_FLIP=0"],
    );
    compile(
        "witness.c",
        out.join("B/libwitness.so"),
        &["WITNESS_FLIP=1"],
    );
    compile(
        "witness.c",
        out.join("preload/libclear.so"),
        &["WITNESS_FLIP=1", "WITNESS_CLEAR_PRELOAD=1"],
    );
    compile(
        "witness.c",
        out.join("rodata/libwitness.so"),
        &["WITNESS_RODATA=1"],
    );
    println!("cargo:rustc-link-search=native={}", out.join("A").display());
    println!("cargo:rustc-link-arg-bin=loaded-code-witness=-Wl,-rpath,$ORIGIN");
    println!("cargo:rerun-if-changed=witness.c");
}
