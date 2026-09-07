// SPDX-License-Identifier: AGPL-3.0-or-later
//! Fault package composition and artifact identity.

use std::{error::Error, fs, path::PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
use searcher::search::{
    archive::{MAX_ARCHIVE_ENTRIES, RetentionPolicy, SelectorPolicy},
    campaign::{
        CampaignConfig, CampaignOrigin, DEFAULT_ADMISSION_RESERVATIONS_PER_WORKER,
        run_campaign_checkpointed,
    },
    draw::{DrawMixture, SuffixShape},
};
#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
use std::io::BufWriter;

use crate::DELAY_FORWARD_LATENCY_MS;
use crate::spec::WorkloadSpec;

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
use crate::game::{FaultCampaignRun, FaultGame};

/// Logical limits for one deterministic fault campaign.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SearchOptions {
    pub seed: u64,
    pub workers: u32,
    pub executions: u64,
    pub actions: usize,
    pub output: PathBuf,
}

/// Search parameters that affect generated candidates and campaign identity.
/// The output directory is an artifact destination and is intentionally not
/// part of this value.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SearchIdentity {
    pub seed: u64,
    pub workers: u32,
    pub executions: u64,
    pub actions: usize,
}

impl From<&SearchOptions> for SearchIdentity {
    fn from(options: &SearchOptions) -> Self {
        Self {
            seed: options.seed,
            workers: options.workers,
            executions: options.executions,
            actions: options.actions,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkloadIdentity {
    pub package: String,
    pub workload_sha256: String,
    pub semantics: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExecutionIdentity {
    pub backend: String,
    pub isa: String,
    pub core_contract: String,
    pub artifacts: std::collections::BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PreparedIdentity {
    pub workload: WorkloadIdentity,
    pub execution: ExecutionIdentity,
    pub search: SearchIdentity,
    pub search_policy: String,
}

fn validate_output(options: &SearchOptions) -> Result<(), Box<dyn Error>> {
    if options.workers == 0 || options.actions == 0 || options.executions == 0 {
        return Err("workers, actions, and executions must be positive".into());
    }
    if options.output.exists() && fs::read_dir(&options.output)?.next().is_some() {
        return Err("search output directory must be empty; choose a new --out path".into());
    }
    Ok(())
}

fn record_identity(
    spec: &WorkloadSpec,
    execution: ExecutionIdentity,
    options: &SearchOptions,
) -> Result<(), Box<dyn Error>> {
    let identity = PreparedIdentity {
        workload: WorkloadIdentity {
            package: "faults-v2".into(),
            workload_sha256: spec.identity_sha256()?,
            semantics: format!(
                "fault-action-v2;catalog-v2;workload-schema-v2;delay-forward-{DELAY_FORWARD_LATENCY_MS}ms;pending-work-v1;runtime-action-v1;sdk-check-v1"
            ),
        },
        execution,
        search: SearchIdentity::from(options),
        search_policy: "campaign-v1;bounded-catalog;admit-alive;group-uniform".into(),
    };
    fs::create_dir_all(&options.output)?;
    serde_json::to_writer_pretty(
        fs::File::create(options.output.join("prepared.json"))?,
        &identity,
    )?;
    Ok(())
}

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
fn run_campaign(game: FaultGame, options: &SearchOptions) -> Result<(), Box<dyn Error>> {
    let run = FaultCampaignRun::default();
    let config = CampaignConfig {
        campaign_seed: options.seed,
        workers: options.workers,
        execution_budget: options.executions,
        action_limit: options.actions,
        host: "harmony-search".into(),
        wall_budget: None,
        continue_after_victory: false,
        archive_entry_limit: MAX_ARCHIVE_ENTRIES,
        reservations_per_worker: DEFAULT_ADMISSION_RESERVATIONS_PER_WORKER,
        memory_budget_mib: None,
        materialize_final_artifacts: true,
        run,
        suffix: SuffixShape::OneOrTwo,
        mixture: DrawMixture::AlphabetOnly,
        retention: RetentionPolicy::AdmitAlive,
        selector: SelectorPolicy::GroupUniform,
        victory_input_path: Some(options.output.join("victory.json")),
    };
    let mut stream = BufWriter::new(fs::File::create(options.output.join("stream.jsonl"))?);
    let (report, checkpoint) =
        run_campaign_checkpointed(&game, &config, &CampaignOrigin::Genesis, &mut stream, None)?;
    serde_json::to_writer_pretty(
        fs::File::create(options.output.join("report.json"))?,
        &report,
    )?;
    serde_json::to_writer(
        fs::File::create(options.output.join("checkpoint.json"))?,
        &checkpoint,
    )?;
    Ok(())
}

/// Search the declared workload in one single-vCPU Consonance VM per worker.
pub fn search_consonance(
    spec: &WorkloadSpec,
    kernel: &[u8],
    initramfs: &[u8],
    options: &SearchOptions,
) -> Result<(), Box<dyn Error>> {
    validate_output(options)?;
    record_identity(
        spec,
        ExecutionIdentity {
            backend: "consonance".into(),
            isa: std::env::consts::ARCH.into(),
            core_contract: {
                #[cfg(all(
                    feature = "consonance",
                    target_os = "linux",
                    any(target_arch = "x86_64", target_arch = "aarch64"),
                    not(miri)
                ))]
                {
                    consonance_client::session::Session::identity_with_config(
                        kernel,
                        initramfs,
                        &crate::game::fault_session_config(),
                    )
                }
                #[cfg(not(all(
                    feature = "consonance",
                    target_os = "linux",
                    any(target_arch = "x86_64", target_arch = "aarch64"),
                    not(miri)
                )))]
                {
                    "consonance-session-unavailable-on-this-target".into()
                }
            },
            artifacts: [
                ("kernel".into(), format!("{:x}", Sha256::digest(kernel))),
                (
                    "initramfs".into(),
                    format!("{:x}", Sha256::digest(initramfs)),
                ),
            ]
            .into(),
        },
        options,
    )?;

    #[cfg(all(
        feature = "consonance",
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ))]
    {
        let game = FaultGame::new_consonance(spec, kernel, initramfs)?;
        run_campaign(game, options)
    }

    #[cfg(not(all(
        feature = "consonance",
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    )))]
    {
        let _ = (kernel, initramfs);
        Err("fault Consonance search requires Linux x86_64/aarch64".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_options_require_a_fresh_output_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let mut options = SearchOptions {
            seed: 1,
            workers: 1,
            executions: 1,
            actions: 1,
            output: tmp.path().to_owned(),
        };
        assert!(validate_output(&options).is_ok());
        fs::write(tmp.path().join("existing"), b"x").unwrap();
        assert!(validate_output(&options).is_err());
        options.output = tmp.path().join("new");
        assert!(validate_output(&options).is_ok());
    }

    #[test]
    fn search_identity_excludes_artifact_destination() {
        let mut first = SearchOptions {
            seed: 7,
            workers: 2,
            executions: 9,
            actions: 11,
            output: PathBuf::from("first-artifacts"),
        };
        let first_identity = SearchIdentity::from(&first);
        first.output = PathBuf::from("second-artifacts");
        let second_identity = SearchIdentity::from(&first);
        assert_eq!(first_identity, second_identity);
        let encoded = serde_json::to_string(&first_identity).unwrap();
        assert!(!encoded.contains("artifacts"));
    }
}
