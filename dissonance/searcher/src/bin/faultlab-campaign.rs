// SPDX-License-Identifier: AGPL-3.0-or-later

//! Run a fault-library campaign over one Consonance workload image.

#[cfg(all(target_os = "linux", target_arch = "x86_64", not(miri)))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    real::run()
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64", not(miri))))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    Err("faultlab-campaign requires Linux/x86-64 KVM outside Miri".into())
}

#[cfg(all(target_os = "linux", target_arch = "x86_64", not(miri)))]
mod real {
    use std::{
        env,
        error::Error,
        ffi::OsString,
        fs,
        io::{self},
        path::PathBuf,
    };

    use searcher::{
        faultlab::{
            archive::MAX_ARCHIVE_ENTRIES,
            bundle::FaultVocabulary,
            campaign::{
                FaultCampaignConfig, FaultCampaignOrigin, FaultGame,
                run_faultlab_campaign_checkpointed,
            },
            report::write_bug_reports,
        },
        search::{
            archive::{RetentionPolicy, RetireThresholds, SelectorPolicy},
            draw::{DrawMixture, SuffixShape},
        },
    };
    use serde_json::json;

    /// Campaign seed. The search perturbs the fault schedule, so the seed is
    /// fixed rather than an operator knob.
    const CAMPAIGN_SEED: u64 = 1;
    const ACTION_LIMIT: usize = 64;

    struct Args {
        kernel: PathBuf,
        initramfs: PathBuf,
        rdinit: String,
        bundle: PathBuf,
        workers: u32,
        executions: u64,
        output: PathBuf,
    }

    impl Args {
        fn parse() -> Result<Self, Box<dyn Error>> {
            let mut kernel = None;
            let mut initramfs = None;
            let mut rdinit = None;
            let mut bundle = None;
            let mut output = None;
            let mut workers = 1_u32;
            let mut executions = 10_000_u64;
            let mut args = env::args_os().skip(1);
            while let Some(flag) = args.next() {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value after {}", flag.to_string_lossy()))?;
                match flag.to_string_lossy().as_ref() {
                    "--kernel" => kernel = Some(PathBuf::from(value)),
                    "--initramfs" => initramfs = Some(PathBuf::from(value)),
                    "--rdinit" => {
                        rdinit = Some(value.into_string().map_err(|_| "rdinit is not UTF-8")?);
                    }
                    "--bundle" => bundle = Some(PathBuf::from(value)),
                    "--output" => output = Some(PathBuf::from(value)),
                    "--workers" => workers = parse_number("workers", value)?,
                    "--executions" => executions = parse_number("executions", value)?,
                    other => return Err(format!("unknown argument {other:?}").into()),
                }
            }
            Ok(Self {
                kernel: kernel.ok_or("missing --kernel")?,
                initramfs: initramfs.ok_or("missing --initramfs")?,
                rdinit: rdinit.ok_or("missing --rdinit")?,
                bundle: bundle.ok_or("missing --bundle")?,
                workers,
                executions,
                output: output.ok_or("missing --output")?,
            })
        }
    }

    fn parse_number<T>(name: &str, value: OsString) -> Result<T, Box<dyn Error>>
    where
        T: std::str::FromStr,
        T::Err: Error + 'static,
    {
        Ok(value
            .into_string()
            .map_err(|_| format!("{name} is not UTF-8"))?
            .replace('_', "")
            .parse()?)
    }

    pub fn run() -> Result<(), Box<dyn Error>> {
        let args = Args::parse()?;
        fs::create_dir_all(&args.output)?;
        let kernel = fs::read(&args.kernel)?;
        let initramfs = fs::read(&args.initramfs)?;
        // The alphabet is exactly what the workload's bundle declares: an
        // action naming an absent node or hook is skipped by the guest agent,
        // so drawing one would spend budget without perturbing anything.
        let vocabulary = FaultVocabulary::parse(&fs::read_to_string(&args.bundle)?)?;
        let game = FaultGame::new(&kernel, &initramfs, &args.rdinit);
        let config = FaultCampaignConfig {
            campaign_seed: CAMPAIGN_SEED,
            vocabulary: vocabulary.clone(),
            workers: args.workers,
            execution_budget: args.executions,
            action_limit: ACTION_LIMIT,
            host: "faultlab".to_owned(),
            wall_budget: None,
            archive_entry_limit: MAX_ARCHIVE_ENTRIES,
            memory_budget_mib: Some(512),
            materialize_final_artifacts: true,
            retention: RetentionPolicy::AdmitAlive,
            selector: SelectorPolicy::EnergyFrontierCheapest(RetireThresholds {
                entry: 3,
                groups: vec![6, 2],
            }),
            suffix: SuffixShape::OneToSix,
            mixture: DrawMixture::AlphabetOnly,
            victory_input_path: Some(args.output.join("first-bug-input.json")),
        };
        let mut stream = io::BufWriter::new(fs::File::create(args.output.join("stream.jsonl"))?);
        let mut progress = fs::File::create(args.output.join("progress.jsonl"))?;
        let (report, checkpoint) = run_faultlab_campaign_checkpointed(
            &game,
            &config,
            &FaultCampaignOrigin::Genesis,
            &mut stream,
            Some(&mut progress),
        )?;
        drop(checkpoint);
        let bugs = write_bug_reports(
            report.campaign.archive.root_seal,
            &report.campaign.archive.bugs,
            &args.output,
        )?;
        let summary = json!({
            "mode": "faultlab_campaign",
            "image": game.image_identity(),
            "vocabulary": vocabulary.identifier(),
            "campaign_seed": report.campaign.campaign_seed,
            "workers": report.campaign.workers,
            "execution_budget": report.campaign.execution_budget,
            "executions": report.campaign.executions_completed,
            "horizons": report.campaign.frames_emulated,
            "stream_sha256": report.campaign.stream_sha256,
            "archive_entries": report.campaign.archive.entries.len(),
            "progress": report.campaign.archive.progress_watermark,
            "milestones": report.campaign.archive.milestones,
            "bugs_found": report.bugs_found,
            "executions_to_first_bug": report.executions_to_first_bug,
            "bug_reports": bugs.iter().map(|bug| bug.file_name()).collect::<Vec<_>>(),
        });
        fs::write(
            args.output.join("campaign-summary.json"),
            serde_json::to_vec_pretty(&summary)?,
        )?;
        println!("{}", serde_json::to_string_pretty(&summary)?);
        Ok(())
    }
}
