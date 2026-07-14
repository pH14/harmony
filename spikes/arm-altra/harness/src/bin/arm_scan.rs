// SPDX-License-Identifier: AGPL-3.0-or-later
//! `arm-scan` — the offline verification gate.
//!
//! Three modes, all pure-logic and all runnable on the development Mac:
//!
//! - `windows <ELF-dir>` — for every payload, decode its counting window out of
//!   the built ELF and assert the branch sequence matches the oracle model. This
//!   is what makes "the taken-branch count is known by construction" a checked
//!   claim. It is the payload build's acceptance gate.
//! - `exclusives <image>` — scan an image for `LDXR`/`STXR`-family instructions
//!   (stage AA-4 level 2). On the LSE-only payloads it must come up clean; on the
//!   LL/SC payload it must find them. Given the guest kernel image on arrival day,
//!   it is the enforceable half of the LSE-only ruling.
//! - `counter-reads <image>` — scan for raw `CNTVCT`/`CNTPCT` reads (stage AA-5's
//!   closure check). On silicon without FEAT_ECV there is no trap to fall back on,
//!   so a clean scan of the shipped guest kernel *is* the enforcement.
//!
//! Exit status is the gate: nonzero if any check failed. The RC propagates — a
//! "scanned N files" line is never a success condition
//! (`docs/ARM-ALTRA.md` §Evidence integrity #1).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use arm_harness::elf::Elf;
use arm_harness::scan::{HitKind, scan};
use arm_harness::verify::verify;
use clap::{Parser, Subcommand};
use oracle_model::{ALL_PAYLOADS, ALL_SCALES, Payload, expected};
use serde::Serialize;

#[derive(Parser)]
#[command(
    name = "arm-scan",
    about = "Offline verification of the arm64 oracle payloads and guest images (untested on silicon)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Verify every payload's window against the oracle model.
    Windows {
        /// Directory of built payload ELFs (…/release).
        elf_dir: PathBuf,
    },
    /// Scan an image for LL/SC exclusive instructions (AA-4 level 2).
    Exclusives {
        /// The ELF to scan.
        image: PathBuf,
        /// Require the scan to find at least one (for the LL/SC payload itself).
        #[arg(long)]
        expect_present: bool,
    },
    /// Scan an image for raw counter reads (AA-5 closure).
    CounterReads {
        /// The ELF to scan.
        image: PathBuf,
    },
    /// Emit the per-payload expected-count manifest as stable JSON.
    Manifest,
}

/// The expected-count manifest — one entry per (payload, scale).
///
/// This is deliverable 1's "machine-readable expected-count manifest per payload,
/// with the derivation documented". It carries the count decomposition with the
/// four ambiguity weights left **as unknowns**: `certain_taken` is exact, the
/// `*_count` fields say how many times each unknown-weight event occurs, and there
/// is deliberately **no total** — a total needs measured weights, and those are
/// stage AA-1's to produce, never this apparatus's to assume. The manifest is what
/// silicon's measurements are checked against; before silicon it is the derivation,
/// written down and generator-tested.
#[derive(Serialize)]
struct ManifestEntry {
    payload: String,
    scale: String,
    seed: u64,
    trips: u64,
    /// Taken branches known exactly, no unknowns.
    certain_taken: u64,
    /// Occurrences of each unknown-weight event.
    exception_entry_count: u64,
    exception_return_count: u64,
    svc_count: u64,
    wfi_count: u64,
    /// The window's constant offset is itself a measured unknown (AA-1(a)).
    window_offset: &'static str,
    /// Whether the count includes a term the payload can only report at runtime.
    has_reported_term: bool,
    /// The window's branch instructions, in address order.
    inline_branches: Vec<String>,
}

fn emit_manifest() -> Result<(), String> {
    let mut entries = Vec::new();
    for payload in ALL_PAYLOADS {
        if !payload.has_window() {
            continue;
        }
        for scale in ALL_SCALES {
            let e = expected(payload, scale, oracle_model::DEFAULT_SEED);
            entries.push(ManifestEntry {
                payload: payload.name().to_string(),
                scale: scale.name().to_string(),
                seed: e.seed,
                trips: e.trips,
                certain_taken: e.certain_taken,
                exception_entry_count: e.exception_entries,
                exception_return_count: e.exception_returns,
                svc_count: e.svc_instructions,
                wfi_count: e.wfi_instructions,
                window_offset: "measured-AA-1 (unknown pre-silicon)",
                has_reported_term: e.has_reported_term,
                inline_branches: e
                    .inline_branch_seq
                    .iter()
                    .map(|b| format!("{b:?}"))
                    .collect(),
            });
        }
    }
    let json =
        serde_json::to_string_pretty(&entries).map_err(|e| format!("serialize manifest: {e}"))?;
    println!("{json}");
    Ok(())
}

fn load(path: &Path) -> Result<Elf, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    Elf::parse(bytes).map_err(|e| format!("parse {}: {e}", path.display()))
}

fn verify_windows(elf_dir: &Path) -> Result<(), String> {
    let mut all_ok = true;
    for payload in ALL_PAYLOADS {
        if !payload.has_window() {
            println!("skip   {:<16} (no counting window)", payload.name());
            continue;
        }
        let path = elf_dir.join(payload.name());
        let elf = load(&path)?;
        let verdict = verify(&elf, payload).map_err(|e| format!("{}: {e}", payload.name()))?;
        if verdict.ok {
            println!(
                "ok     {:<16} window branches {:?}",
                payload.name(),
                verdict.found_branches
            );
        } else {
            all_ok = false;
            eprintln!("FAIL   {:<16}", payload.name());
            for f in &verdict.failures {
                eprintln!("         {f}");
            }
        }
    }
    if all_ok {
        Ok(())
    } else {
        Err("one or more payload windows disagree with the oracle model".into())
    }
}

/// The image's executable bytes, or a hard failure.
///
/// **Fail closed** (`docs/ARM-ALTRA.md` §Evidence integrity #1). A stripped image —
/// no section table, executable `PT_LOAD` segments only, which real distro and
/// vendor kernel images routinely are — used to yield an empty scan surface, and an
/// empty surface scans clean: `found 0 raw counter read(s)`, exit 0, a green AA-5
/// enforcement over an image not a byte of which was read. A scan of nothing is not
/// a clean scan, and this is where that is enforced for both scan modes.
fn scan_surface(elf: &Elf) -> Result<Vec<(u64, &[u8])>, String> {
    elf.executable_ranges()
        .map_err(|e| format!("{e} — the scan is the enforcement, so an unscannable image fails"))
}

fn scan_exclusives(image: &Path, expect_present: bool) -> Result<(), String> {
    let elf = load(image)?;
    let mut count = 0usize;
    for (addr, code) in scan_surface(&elf)? {
        for hit in scan(addr, code) {
            if hit.kind == HitKind::Exclusive {
                count += 1;
                println!("exclusive at {:#x}: {:#010x}", hit.addr, hit.word);
            }
        }
    }
    println!("found {count} exclusive instruction(s)");
    match (expect_present, count) {
        (true, 0) => Err("expected at least one exclusive, found none".into()),
        (false, n) if n > 0 => Err(format!(
            "LSE-only violation: {n} exclusive instruction(s) present"
        )),
        _ => Ok(()),
    }
}

fn scan_counter_reads(image: &Path) -> Result<(), String> {
    let elf = load(image)?;
    let mut count = 0usize;
    for (addr, code) in scan_surface(&elf)? {
        for hit in scan(addr, code) {
            if let HitKind::CounterRead(reg) = hit.kind {
                count += 1;
                println!(
                    "counter read {} at {:#x}: {:#010x}",
                    reg.name(),
                    hit.addr,
                    hit.word
                );
            }
        }
    }
    println!("found {count} raw counter read(s)");
    if count == 0 {
        Ok(())
    } else {
        // On silicon without FEAT_ECV every one of these is a hole in the paravirt
        // clock's closure, because there is no trap behind it. A clean scan is the
        // acceptance criterion; anything else must be triaged to an unreachable or
        // patched-out site (AA-5(b)) before a disposition.
        Err(format!(
            "{count} raw counter read(s) present: each must be triaged (AA-5(b)) — \
             there is no trap fallback on non-ECV silicon"
        ))
    }
}

fn run() -> Result<(), String> {
    let _ = Payload::Ident; // keep the import meaningful if the set shrinks
    match Cli::parse().command {
        Command::Windows { elf_dir } => verify_windows(&elf_dir),
        Command::Exclusives {
            image,
            expect_present,
        } => scan_exclusives(&image, expect_present),
        Command::CounterReads { image } => scan_counter_reads(&image),
        Command::Manifest => emit_manifest(),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => {
            println!("PASS");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("FAIL: {e}");
            ExitCode::FAILURE
        }
    }
}
