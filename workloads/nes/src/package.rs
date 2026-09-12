// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::{
    nova::campaign::{NovaCampaignRun, NovaGame},
    smb::campaign::{SmbCampaignRun, SmbGame},
};
use searcher::search::{
    archive::{MAX_ARCHIVE_ENTRIES, RetentionPolicy, SelectorPolicy},
    campaign::{
        CampaignConfig, CampaignOrigin, DEFAULT_ADMISSION_RESERVATIONS_PER_WORKER, Game,
        run_campaign_checkpointed,
    },
    draw::{DrawMixture, SuffixShape},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fs,
    io::BufWriter,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RomKind {
    Smb,
    Nova,
}
impl RomKind {
    pub fn identify(rom: &[u8]) -> Result<Self, Box<dyn Error>> {
        let digest = format!("{:x}", Sha256::digest(rom));
        match digest.as_str() {
            "0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea" => Ok(Self::Smb),
            "9107be62a08a0ae51a01f900bd18a95a52fe043f53e9cae4a64d1e8e73114b08" => Ok(Self::Nova),
            _ => Err(format!("unsupported NES ROM SHA-256 {digest}").into()),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct SearchOptions {
    pub seed: u64,
    pub workers: u32,
    pub executions: u64,
    pub actions: usize,
    pub output: PathBuf,
}
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
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
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct WorkloadIdentity {
    pub package: String,
    pub adapter: RomKind,
    pub rom_sha256: String,
    pub semantics: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct ExecutionIdentity {
    pub backend: String,
    pub isa: String,
    pub core_contract: String,
    pub artifacts: std::collections::BTreeMap<String, String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct PreparedIdentity {
    pub workload: WorkloadIdentity,
    pub execution: ExecutionIdentity,
    pub search: SearchIdentity,
    pub search_policy: String,
}
impl PreparedIdentity {
    pub fn validate_execution(&self, candidate: &Self) -> Result<(), Box<dyn Error>> {
        if self.workload != candidate.workload || self.execution != candidate.execution {
            return Err("prepared workload or execution identity mismatch".into());
        }
        Ok(())
    }
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
    rom: &[u8],
    execution: ExecutionIdentity,
    options: &SearchOptions,
) -> Result<(), Box<dyn Error>> {
    let identity = PreparedIdentity {
        workload: WorkloadIdentity {
            package: "nes-v1".into(),
            adapter: RomKind::identify(rom)?,
            rom_sha256: format!("{:x}", Sha256::digest(rom)),
            semantics: "game-adapter-v1;action=button-hold-v1;observation=nes-ram-ring-v2".into(),
        },
        execution,
        search: SearchIdentity::from(options),
        search_policy: "campaign-v1;alphabet-only;group-uniform;admit-alive;default-suffix".into(),
    };
    fs::create_dir_all(&options.output)?;
    serde_json::to_writer_pretty(
        fs::File::create(options.output.join("prepared.json"))?,
        &identity,
    )?;
    Ok(())
}
fn search<G: Game>(game: G, run: G::Run, options: &SearchOptions) -> Result<(), Box<dyn Error>>
where
    G::ArchiveReport: Serialize + serde::de::DeserializeOwned,
{
    fs::create_dir_all(&options.output)?;
    let mut stream = BufWriter::new(fs::File::create(options.output.join("stream.jsonl"))?);
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
        suffix: SuffixShape::default(),
        mixture: DrawMixture::default(),
        retention: RetentionPolicy::AdmitAlive,
        selector: SelectorPolicy::GroupUniform,
        victory_input_path: Some(options.output.join("victory.json")),
    };
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
fn smb_run() -> SmbCampaignRun {
    SmbCampaignRun {
        chord: Default::default(),
        vocabulary: Default::default(),
        terminal: None,
    }
}
pub fn search_native(
    rom: &[u8],
    core: &Path,
    options: &SearchOptions,
) -> Result<(), Box<dyn Error>> {
    validate_output(options)?;
    let kind = RomKind::identify(rom)?;
    let core_hash = format!("{:x}", Sha256::digest(fs::read(core)?));
    record_identity(
        rom,
        ExecutionIdentity {
            backend: "native".into(),
            isa: std::env::consts::ARCH.into(),
            core_contract: format!(
                "{};{};{}",
                machine::quicknes::QUICKNES_REVISION,
                machine::quicknes::QUICKNES_BUILD,
                machine::quicknes::QUICKNES_OPTIONS
            ),
            artifacts: [("core".into(), core_hash.clone())].into(),
        },
        options,
    )?;
    match kind {
        RomKind::Smb => search(SmbGame::new(rom, core, &core_hash), smb_run(), options),
        RomKind::Nova => search(
            NovaGame::new(rom, core, &core_hash),
            NovaCampaignRun,
            options,
        ),
    }
}
#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
pub fn search_consonance(
    rom: &[u8],
    kernel: &[u8],
    initramfs: &[u8],
    options: &SearchOptions,
) -> Result<(), Box<dyn Error>> {
    validate_output(options)?;
    record_identity(
        rom,
        ExecutionIdentity {
            backend: "consonance".into(),
            isa: std::env::consts::ARCH.into(),
            core_contract: machine::consonance::identity(kernel, initramfs),
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
    match RomKind::identify(rom)? {
        RomKind::Smb => search(
            SmbGame::new_consonance(rom, kernel, initramfs),
            smb_run(),
            options,
        ),
        RomKind::Nova => search(
            NovaGame::new_consonance(rom, kernel, initramfs),
            NovaCampaignRun,
            options,
        ),
    }
}
