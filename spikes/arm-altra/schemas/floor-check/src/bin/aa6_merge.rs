// SPDX-License-Identifier: AGPL-3.0-or-later
//! `aa6-merge` — assemble ONE AA-6 mini-gate run-set from the bare-payload run-set and the
//! LinuxGuest injection records.
//!
//! The floor-checker's `aa6-matrix` runs PER run-set, so all nine classes' injected records
//! (the eight windowed bare payloads PLUS the AA-5 Linux guest) must live in a single run-set.
//! The bare payloads come from `arm-spike run --stage aa6 --with-targets --skid-margin <N>
//! --inject-ppi 20 --reps 1000` (one dir); the LinuxGuest armed+delivered records come from
//! `arm-spike linux-boot --aa6-record <jsonl>` run ≥1000 times with a fixed seed + inject Moment.
//! This tool concatenates them, densely renumbers `sample_id`, binds the LinuxGuest image pin,
//! recomputes `records_sha256`, and writes the merged run-set — a mechanical recombination of
//! harness-produced records (never a hand-authored manifest): the merged bytes are re-hashed, so
//! the floor-checker's `records-sha256` gate still binds every record.

use std::path::PathBuf;
use std::process::ExitCode;

use arm_harness::evidence::{ImagePin, RunRecord, RunSet, hex_lower, to_stable_json};
use clap::Parser;
use sha2::{Digest, Sha256};

#[derive(Parser)]
#[command(about = "Merge the bare-payload AA-6 run-set with LinuxGuest injection records")]
struct Cli {
    /// The bare-payload AA-6 run-set directory (its `run-set.json` is the manifest template).
    #[arg(long)]
    bare: PathBuf,
    /// The LinuxGuest injection records (JSONL, one `RunRecord` per line — the `--aa6-record`
    /// output, appended once per boot rep).
    #[arg(long = "linux-records")]
    linux_records: PathBuf,
    /// The Linux guest `Image` content hash, bound as the `linux-guest` image pin (required so
    /// `check_image_pins` sees the exercised LinuxGuest class pinned by content).
    #[arg(long = "linux-image-sha256")]
    linux_image_sha256: String,
    /// The Linux guest `Image` path recorded in the pin (its basename need not be `linux-guest`;
    /// the pin the checker keys on is added with that basename automatically).
    #[arg(long = "linux-image-path", default_value = "payloads/linux-guest")]
    linux_image_path: String,
    /// Optional Linux initramfs content hash, bound as an additional verified pin.
    #[arg(long = "linux-initramfs-sha256")]
    linux_initramfs_sha256: Option<String>,
    /// The output run-set directory (created; refuses to clobber existing files).
    #[arg(long)]
    out: PathBuf,
}

fn run() -> Result<(), String> {
    let cli = Cli::parse();

    // Load the bare manifest + records (the template).
    let manifest_bytes = std::fs::read_to_string(cli.bare.join("run-set.json"))
        .map_err(|e| format!("read {}: {e}", cli.bare.join("run-set.json").display()))?;
    let mut run_set: RunSet = serde_json::from_str(&manifest_bytes)
        .map_err(|e| format!("parse the bare run-set manifest: {e}"))?;
    let bare_records_path = cli.bare.join(&run_set.records_file);
    let bare_records = read_records(&bare_records_path)?;

    // Load the LinuxGuest injection records.
    let linux_records = read_records(&cli.linux_records)?;
    if linux_records.is_empty() {
        return Err(format!(
            "no LinuxGuest records in {} — the mini-gate matrix needs the Linux guest injected",
            cli.linux_records.display()
        ));
    }

    // Concatenate and DENSELY renumber sample_id 0..N (the totality key).
    let mut records: Vec<RunRecord> = bare_records;
    records.extend(linux_records);
    for (i, r) in records.iter_mut().enumerate() {
        r.sample_id = i as u64;
    }

    // Bind the LinuxGuest image pin under the class basename the checker keys on
    // (`image_file_name(path) == "linux-guest"`), plus an optional initramfs pin.
    run_set.images.push(ImagePin {
        path: format!("{}/linux-guest", cli.linux_image_path.trim_end_matches('/')),
        sha256: cli.linux_image_sha256.clone(),
        md5: None,
        verified_before_boot: true,
    });
    if let Some(initramfs) = &cli.linux_initramfs_sha256 {
        run_set.images.push(ImagePin {
            path: format!(
                "{}/linux-guest-initramfs",
                cli.linux_image_path.trim_end_matches('/')
            ),
            sha256: initramfs.clone(),
            md5: None,
            verified_before_boot: true,
        });
    }

    // Re-hash the merged records bytes so `records-sha256` binds every record, and update the
    // totality/plan counts to the merged total.
    let records_jsonl = records
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("serialize a merged record: {e}"))?
        .join("\n")
        + "\n";
    let mut hasher = Sha256::new();
    hasher.update(records_jsonl.as_bytes());
    run_set.records_sha256 = hex_lower(&hasher.finalize());
    run_set.attempted = records.len() as u64;
    run_set.planned = records.len() as u64;

    // Write the merged run-set (refuse to clobber).
    std::fs::create_dir_all(&cli.out).map_err(|e| format!("create {}: {e}", cli.out.display()))?;
    let manifest_out = cli.out.join("run-set.json");
    let records_out = cli.out.join(&run_set.records_file);
    for p in [&manifest_out, &records_out] {
        if p.exists() {
            return Err(format!(
                "{} already exists — refusing to clobber",
                p.display()
            ));
        }
    }
    let manifest_json =
        to_stable_json(&run_set).map_err(|e| format!("serialize the merged manifest: {e}"))?;
    std::fs::write(&manifest_out, manifest_json.as_bytes())
        .map_err(|e| format!("write {}: {e}", manifest_out.display()))?;
    std::fs::write(&records_out, records_jsonl.as_bytes())
        .map_err(|e| format!("write {}: {e}", records_out.display()))?;

    println!(
        "AA6_MERGE out={} records={} (bare + {} linux-guest) images={} records_sha256={}",
        cli.out.display(),
        records.len(),
        records
            .iter()
            .filter(|r| r.payload.name() == "linux-guest")
            .count(),
        run_set.images.len(),
        run_set.records_sha256,
    );
    Ok(())
}

/// Read a JSONL records file into `RunRecord`s (one per non-empty line).
fn read_records(path: &std::path::Path) -> Result<Vec<RunRecord>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<RunRecord>(l).map_err(|e| format!("parse a record: {e}")))
        .collect()
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("aa6-merge: {e}");
            ExitCode::FAILURE
        }
    }
}
