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
            bundle::{FaultVocabulary, parse_places},
            campaign::{
                FaultCampaignConfig, FaultCampaignOrigin, FaultGame,
                run_faultlab_campaign_checkpointed,
            },
            consonance::{BackendKind, DEFAULT_RAM_MIB, FaultlabConfig},
            report::write_bug_reports,
            target::ActionWindows,
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
        config: FaultlabConfig,
        bundle: PathBuf,
        places: Option<PathBuf>,
        workers: u32,
        executions: u64,
        wall_budget: Option<std::time::Duration>,
        output: PathBuf,
    }

    impl Args {
        fn parse() -> Result<Self, Box<dyn Error>> {
            let mut kernel = None;
            let mut initramfs = None;
            let mut rdinit = None;
            let mut knobs = Vec::new();
            let mut backend = None;
            let mut horizon_ms = 2_000_u64;
            let mut ram_mib = DEFAULT_RAM_MIB;
            let mut pvclock = true;
            let mut bundle = None;
            let mut places = None;
            let mut output = None;
            let mut workers = 1_u32;
            let mut executions = 10_000_u64;
            let mut wall_minutes = None;
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
                    "--knob" => {
                        let knob = value.into_string().map_err(|_| "knob is not UTF-8")?;
                        if !knob.contains('=') || knob.contains(char::is_whitespace) {
                            return Err(format!("knob {knob:?} is not one key=value word").into());
                        }
                        knobs.push(knob);
                    }
                    "--backend" => {
                        backend = Some(match value.to_string_lossy().as_ref() {
                            "stock" => BackendKind::Stock,
                            "patched" => BackendKind::Patched,
                            other => return Err(format!("unknown backend {other:?}").into()),
                        });
                    }
                    "--horizon-ms" => horizon_ms = parse_number("horizon-ms", value)?,
                    "--ram-mib" => ram_mib = parse_number("ram-mib", value)?,
                    "--pvclock" => {
                        pvclock = match value.to_string_lossy().as_ref() {
                            "on" => true,
                            "off" => false,
                            other => return Err(format!("unknown pvclock {other:?}").into()),
                        };
                    }
                    "--bundle" => bundle = Some(PathBuf::from(value)),
                    "--places" => places = Some(PathBuf::from(value)),
                    "--output" => output = Some(PathBuf::from(value)),
                    "--workers" => workers = parse_number("workers", value)?,
                    "--executions" => executions = parse_number("executions", value)?,
                    "--wall-minutes" => {
                        wall_minutes = Some(parse_number::<u64>("wall-minutes", value)?);
                    }
                    other => return Err(format!("unknown argument {other:?}").into()),
                }
            }
            Ok(Self {
                kernel: kernel.ok_or("missing --kernel")?,
                initramfs: initramfs.ok_or("missing --initramfs")?,
                config: FaultlabConfig {
                    rdinit: rdinit.ok_or("missing --rdinit")?,
                    knobs,
                    backend: backend.ok_or("missing --backend (stock or patched)")?,
                    horizon_nanos: horizon_ms
                        .checked_mul(1_000_000)
                        .ok_or("horizon-ms is too large")?,
                    ram_mib,
                    pvclock,
                },
                bundle: bundle.ok_or("missing --bundle")?,
                places,
                workers,
                executions,
                wall_budget: wall_minutes
                    .map(|minutes| std::time::Duration::from_secs(minutes * 60)),
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
        let mut vocabulary = FaultVocabulary::parse(&fs::read_to_string(&args.bundle)?)?;
        if let Some(places) = &args.places {
            vocabulary = vocabulary.with_places(parse_places(&fs::read_to_string(places)?)?)?;
        }
        let game = FaultGame::new(&kernel, &initramfs, &args.config);
        let config = FaultCampaignConfig {
            campaign_seed: CAMPAIGN_SEED,
            vocabulary: vocabulary.clone(),
            workers: args.workers,
            execution_budget: args.executions,
            action_limit: ACTION_LIMIT,
            host: "faultlab".to_owned(),
            wall_budget: args.wall_budget,
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
            ActionWindows {
                root_seal: report.campaign.archive.root_seal,
                horizon_nanos: report.campaign.archive.horizon_nanos,
            },
            &report.campaign.archive.bugs,
            &args.output,
        )?;
        let summary = json!({
            "mode": "faultlab_campaign",
            "image": game.image_identity(),
            "horizon_nanos": game.config().horizon_nanos,
            "root_seal": report.campaign.archive.root_seal,
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
