//! `gen-fixtures` — (re)emit the checked-in fixture run-sets under
//! `schemas/fixtures/`.
//!
//! The fixtures are generated from the oracle model, then committed as files (they
//! are the evidence the integration tests read). Run this whenever the model or the
//! evidence shapes change; the `fixtures_match_committed` drift test fails the build
//! if the committed files fall out of step with what this would emit.
//!
//! Deterministic by construction: same model in, byte-identical files out.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use floor_check::fixtures::all_fixtures;

/// Emit the fixture run-sets.
#[derive(Parser, Debug)]
#[command(
    name = "gen-fixtures",
    about = "Regenerate the checked-in floor-check fixtures under schemas/fixtures/"
)]
struct Cli {
    /// Where to write the fixtures. Defaults to `schemas/fixtures/` relative to
    /// this crate.
    #[arg(long)]
    out_dir: Option<PathBuf>,
}

/// The default fixtures directory: `<crate>/../fixtures`.
fn default_out_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("fixtures")
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let out_dir = cli.out_dir.unwrap_or_else(default_out_dir);

    for f in all_fixtures() {
        let dir = out_dir.join(f.name);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("gen-fixtures: cannot create {}: {e}", dir.display());
            return ExitCode::FAILURE;
        }
        let manifest = dir.join("run-set.json");
        if let Err(e) = std::fs::write(&manifest, f.manifest_json.as_bytes()) {
            eprintln!("gen-fixtures: cannot write {}: {e}", manifest.display());
            return ExitCode::FAILURE;
        }
        let records = dir.join("records.jsonl");
        if let Err(e) = std::fs::write(&records, f.records_jsonl.as_bytes()) {
            eprintln!("gen-fixtures: cannot write {}: {e}", records.display());
            return ExitCode::FAILURE;
        }
        println!("wrote {}", dir.display());
    }

    ExitCode::SUCCESS
}
