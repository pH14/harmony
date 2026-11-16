// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    error::Error,
    fs,
    io::{BufWriter, Write},
    ops::Range,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Instant,
};

use machine::{
    Answer, Machine, MachineError, Moment, Reproducer, SnapId, StopConditions, StopReason,
    nes::WRAM_SIZE,
    quicknes::{QuickNesMachine, SharedState},
};
use nes_workload::{
    metroid::{
        action::MetroidAction,
        archive::MAX_ARCHIVE_ENTRIES as METROID_ARCHIVE_ENTRIES,
        campaign::{
            MetroidCampaignConfig, MetroidCampaignOrigin, MetroidGame,
            run_metroid_campaign_checkpointed,
        },
        target::{GenesisDepth, MetroidInput, MetroidTarget, missile_door_in_room},
    },
    nes_backend::{NesBackend, NesMemory, native_identity_sha256},
    search::{
        archive::{MAX_ARCHIVE_ENTRIES, RetentionPolicy},
        campaign::default_window,
        draw::{DrawMixture, draw_mixture_from_identifier},
    },
    smb::{
        campaign::{
            SmbButtonVocabulary, SmbCampaignConfig, SmbCampaignOrigin, SmbGame,
            SmbTerminalPredicate, run_smb_campaign_checkpointed,
        },
        target::{SmbInput, SmbTarget},
    },
    target::Target,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const USAGE: &str = "usage: nes-coverage tape <smb|metroid> <input.json> <out.jsonl> [--quiet]\n       nes-coverage campaign <smb|metroid> <seed> <jobs> <out-dir> [--mixture <id>]";
const SMB_BYTES: [(&str, usize); 6] = [
    ("player_state", 0x001d),
    ("player_status", 0x0756),
    ("warp_zone_control", 0x06d6),
    ("oper_mode", 0x0770),
    ("oper_mode_task", 0x0772),
    ("area_type", 0x074e),
];

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    let mode = args.first().ok_or(USAGE)?;
    let game = args.get(1).ok_or(USAGE)?;
    let core = Core::from_environment()?;
    match (mode.as_str(), game.as_str()) {
        ("tape", "smb" | "metroid") => {
            let input = PathBuf::from(args.get(2).ok_or(USAGE)?);
            let output = PathBuf::from(args.get(3).ok_or(USAGE)?);
            let quiet = args[4..].iter().any(|flag| flag == "--quiet");
            if game == "smb" {
                tape_smb(&core, &input, &output, quiet)
            } else {
                tape_metroid(&core, &input, &output, quiet)
            }
        }
        ("campaign", "smb" | "metroid") => {
            let seed: u64 = args.get(2).ok_or(USAGE)?.parse()?;
            let jobs: u64 = args.get(3).ok_or(USAGE)?.parse()?;
            let output = PathBuf::from(args.get(4).ok_or(USAGE)?);
            let mixture = match args.iter().position(|flag| flag == "--mixture") {
                Some(index) => Some(draw_mixture_from_identifier(
                    args.get(index + 1).ok_or(USAGE)?,
                )?),
                None => None,
            };
            fs::create_dir_all(&output)?;
            if game == "smb" {
                campaign_smb(&core, seed, jobs, &output, mixture)
            } else {
                campaign_metroid(&core, seed, jobs, &output, mixture)
            }
        }
        _ => Err(USAGE.into()),
    }
}

struct Core {
    path: PathBuf,
    sha256: String,
    execution_sha256: String,
}

impl Core {
    fn from_environment() -> Result<Self, Box<dyn Error>> {
        let path = PathBuf::from(
            env::var_os("HARMONY_QUICKNES_CORE")
                .ok_or("HARMONY_QUICKNES_CORE must name the core")?,
        );
        let sha256 = format!("{:x}", Sha256::digest(fs::read(&path)?));
        let execution_sha256 = machine::quicknes::prepared_execution_sha256(&path, &sha256)?;
        Ok(Self {
            path,
            sha256,
            execution_sha256,
        })
    }

    fn machine(&self, rom: &[u8]) -> Result<QuickNesMachine, MachineError> {
        QuickNesMachine::from_rom_bytes_with_identity(
            rom,
            &self.path,
            &self.sha256,
            &self.execution_sha256,
        )
    }
}

fn rom(variable: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    let path = env::var_os(variable).ok_or_else(|| format!("{variable} must name the ROM"))?;
    Ok(fs::read(path)?)
}

struct TapeWriter {
    out: Option<BufWriter<fs::File>>,
    coverage: bool,
    ids: Vec<u64>,
    frames: u64,
    started: Instant,
    emulating: f64,
    total_ids: usize,
}

impl TapeWriter {
    fn new(output: &Path, quiet: bool, coverage: bool) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            out: if quiet {
                None
            } else {
                Some(BufWriter::new(fs::File::create(output)?))
            },
            coverage,
            ids: Vec::new(),
            frames: 0,
            started: now(),
            emulating: 0.0,
            total_ids: 0,
        })
    }

    fn timed<T>(&mut self, run: impl FnOnce() -> T) -> T {
        let started = now();
        let value = run();
        self.emulating += started.elapsed().as_secs_f64();
        value
    }

    fn record(&mut self, mut line: Value, wram: &[u8]) -> Result<(), Box<dyn Error>> {
        self.total_ids += self.ids.len();
        let Some(out) = self.out.as_mut() else {
            return Ok(());
        };
        line["frame"] = json!(self.frames);
        line["wram_sha256"] = json!(format!("{:x}", Sha256::digest(wram)));
        if self.coverage {
            line["ids"] = json!(self.ids);
        }
        serde_json::to_writer(&mut *out, &line)?;
        out.write_all(b"\n")?;
        Ok(())
    }

    fn finish(mut self, actions: usize) -> Result<(), Box<dyn Error>> {
        if let Some(out) = self.out.as_mut() {
            out.flush()?;
        }
        println!(
            "{}",
            json!({
                "actions": actions,
                "frames": self.frames,
                "coverage": self.coverage,
                "ids_per_action": if actions == 0 { 0.0 } else { self.total_ids as f64 / actions as f64 },
                "emulating_seconds": self.emulating,
                "wall_seconds": self.started.elapsed().as_secs_f64(),
                "frames_per_second": self.frames as f64 / self.emulating,
            })
        );
        Ok(())
    }
}

fn tape_smb(core: &Core, input: &Path, output: &Path, quiet: bool) -> Result<(), Box<dyn Error>> {
    let rom = rom("HARMONY_SMB_ROM")?;
    let tape: SmbInput = serde_json::from_slice(&fs::read(input)?)?;
    let mut target = SmbTarget::from_machine(core.machine(&rom)?)?;
    target.reset();
    let mut writer = TapeWriter::new(output, quiet, target.has_coverage())?;
    if writer.coverage {
        target.take_coverage(&mut writer.ids)?;
    }
    let mut applied = 0;
    for (index, action) in tape.actions.iter().enumerate() {
        if target.is_dead() || target.is_victory() {
            break;
        }
        writer.timed(|| target.apply(action));
        if writer.coverage {
            let ids = &mut writer.ids;
            writer.emulating += {
                let started = now();
                target.take_coverage(ids)?;
                started.elapsed().as_secs_f64()
            };
        }
        writer.frames += u64::from(action.bounded_hold_frames());
        applied = index + 1;
        if writer.out.is_none() {
            writer.total_ids += writer.ids.len();
            continue;
        }
        let wram = target.wram();
        let bytes: serde_json::Map<String, Value> = SMB_BYTES
            .iter()
            .map(|(name, addr)| ((*name).to_owned(), json!(wram[*addr])))
            .collect();
        let state = target
            .last_action_observations()
            .last()
            .map(|observation| json!(observation.decoded))
            .unwrap_or(Value::Null);
        let line = json!({
            "action": index,
            "buttons": action.buttons,
            "hold": action.bounded_hold_frames(),
            "state": state,
            "bytes": bytes,
            "dead": target.is_dead(),
            "victory": target.is_victory(),
        });
        writer.record(line, &wram)?;
    }
    writer.finish(applied)
}

fn tape_metroid(
    core: &Core,
    input: &Path,
    output: &Path,
    quiet: bool,
) -> Result<(), Box<dyn Error>> {
    let rom = machine::nes::with_cartridge_ram(&rom("HARMONY_METROID_ROM")?)?;
    let tape: MetroidInput = serde_json::from_slice(&fs::read(input)?)?;
    let prefix = metroid_power_on();
    let mut target =
        MetroidTarget::from_machine(core.machine(&rom)?, &prefix, GenesisDepth::NewGame)?;
    target.reset();
    let mut writer = TapeWriter::new(output, quiet, target.has_coverage())?;
    if writer.coverage {
        target.take_coverage(&mut writer.ids)?;
    }
    let mut applied = 0;
    for (index, action) in tape.actions.iter().enumerate() {
        if target.is_dead() || target.is_victory() {
            break;
        }
        let before = target.frames_clocked();
        writer.timed(|| target.apply(action));
        if writer.coverage {
            let ids = &mut writer.ids;
            writer.emulating += {
                let started = now();
                target.take_coverage(ids)?;
                started.elapsed().as_secs_f64()
            };
        }
        writer.frames += target.frames_clocked().saturating_sub(before);
        applied = index + 1;
        if writer.out.is_none() {
            writer.total_ids += writer.ids.len();
            continue;
        }
        let wram = target.current_wram().to_vec();
        let line = json!({
            "action": index,
            "input": action,
            "state": target.mechanical_state(),
            "missile_door_in_room": missile_door_in_room(&wram),
            "dead": target.is_dead(),
            "victory": target.is_victory(),
        });
        writer.record(line, &wram)?;
    }
    writer.finish(applied)
}

fn metroid_power_on() -> Vec<MetroidAction> {
    nes_workload::metroid::target::power_on_walk()
        .into_iter()
        .map(MetroidAction::Chord)
        .collect()
}

const KIND_SHIFT: u32 = 62;

#[allow(clippy::disallowed_methods)]
fn now() -> Instant {
    Instant::now()
}
const SMB_PLACE: [usize; 2] = [0x075f, 0x075c];
const METROID_PLACE: [usize; 3] = [0x0074, 0x0050, 0x004f];

#[derive(Default)]
struct Log {
    actions: Vec<(Vec<u64>, usize, Vec<u8>)>,
    counts: BTreeMap<u64, u32>,
    distinct: [u32; 3],
    jobs: u32,
    out: Option<BufWriter<fs::File>>,
    error: Option<String>,
}

impl Log {
    fn action(&mut self, ids: &[u64], frames: usize, place: Vec<u8>) {
        self.actions.push((ids.to_vec(), frames, place));
    }

    fn end_job(&mut self) {
        if let Err(error) = self.flush_job() {
            self.error.get_or_insert(error.to_string());
        }
    }

    fn flush_job(&mut self) -> Result<(), Box<dyn Error>> {
        let actions = std::mem::take(&mut self.actions);
        let union: BTreeSet<u64> = actions
            .iter()
            .flat_map(|(ids, _, _)| ids.iter().copied())
            .collect();
        if union.is_empty() {
            return Ok(());
        }
        self.jobs += 1;
        for id in &union {
            let count = self.counts.entry(*id).or_default();
            *count += 1;
            if *count == 1
                && let Some(kind) = self.distinct.get_mut(usize::try_from(id >> KIND_SHIFT)?)
            {
                *kind += 1;
            }
        }
        let Some(out) = self.out.as_mut() else {
            return Ok(());
        };
        for (index, (ids, frames, place)) in actions.iter().enumerate() {
            let mut rarest: [Option<(u32, u64)>; 4] = [None; 4];
            let mut new = 0_u32;
            for id in ids {
                let count = self.counts[id];
                if count == 1 {
                    new += 1;
                }
                let kind = usize::try_from(id >> KIND_SHIFT)? + 1;
                for slot in [0, kind] {
                    if rarest[slot].is_none_or(|(best, _)| count < best) {
                        rarest[slot] = Some((count, *id));
                    }
                }
            }
            let line = json!({
                "job": self.jobs,
                "action": index,
                "frames": frames,
                "place": place,
                "ids": ids.len(),
                "new": new,
                "c": rarest.map(|slot| slot.map(|(count, _)| count)),
                "distinct": self.distinct,
                "id": rarest.map(|slot| slot.map(|(_, id)| id)),
            });
            serde_json::to_writer(&mut *out, &line)?;
            out.write_all(b"\n")?;
        }
        Ok(())
    }

    fn finish(&mut self, path: &Path) -> Result<(), Box<dyn Error>> {
        self.end_job();
        if let Some(error) = self.error.take() {
            return Err(error.into());
        }
        if let Some(out) = self.out.as_mut() {
            out.flush()?;
        }
        let mut counts: Vec<(u64, u32)> = self.counts.iter().map(|(id, c)| (*id, *c)).collect();
        counts.sort_unstable();
        let mut kinds = [0_u32; 3];
        for (id, _) in &counts {
            if let Some(kind) = kinds.get_mut(usize::try_from(id >> KIND_SHIFT)?) {
                *kind += 1;
            }
        }
        fs::write(
            path,
            serde_json::to_vec(&json!({
                "jobs": self.jobs,
                "distinct": {"branch": kinds[0], "jump": kinds[1], "data": kinds[2]},
                "counts": counts,
            }))?,
        )?;
        Ok(())
    }
}

struct Covered {
    inner: QuickNesMachine,
    log: Arc<Mutex<Log>>,
    ids: Vec<u64>,
    place: &'static [usize],
}

impl Covered {
    fn end_job(&self) {
        if let Ok(mut log) = self.log.lock() {
            log.end_job();
        }
    }
}

impl Machine for Covered {
    type Portable = SharedState;

    fn snapshot(&mut self) -> Result<SnapId, MachineError> {
        self.inner.snapshot()
    }

    fn drop_snapshot(&mut self, snap: SnapId) -> Result<(), MachineError> {
        self.inner.drop_snapshot(snap)
    }

    fn branch(&mut self, snap: SnapId, env: &Reproducer) -> Result<(), MachineError> {
        self.inner.branch(snap, env)
    }

    fn replay(&mut self, snap: SnapId) -> Result<(), MachineError> {
        self.end_job();
        self.inner.replay(snap)
    }

    fn run(
        &mut self,
        until: StopConditions,
        resolve: Option<&Answer>,
    ) -> Result<StopReason, MachineError> {
        let stop = self.inner.run(until, resolve)?;
        self.inner.take_coverage(&mut self.ids)?;
        let frames = self.inner.frames().len();
        let wram = self.inner.read_wram()?;
        let place = self.place.iter().map(|addr| wram[*addr]).collect();
        self.log
            .lock()
            .map_err(|_| MachineError::Backend("coverage log lock poisoned".to_owned()))?
            .action(&self.ids, frames, place);
        Ok(stop)
    }

    fn read(&self, addr: u64, len: u32) -> Result<Vec<u8>, MachineError> {
        self.inner.read(addr, len)
    }

    fn export(
        &mut self,
        snap: SnapId,
        base: Option<&Self::Portable>,
    ) -> Result<Self::Portable, MachineError> {
        self.inner.export(snap, base)
    }

    fn import(&mut self, portable: &Self::Portable) -> Result<SnapId, MachineError> {
        self.inner.import(portable)
    }

    fn portable_memory_charge(portable: &Self::Portable) -> usize {
        QuickNesMachine::portable_memory_charge(portable)
    }

    fn now(&self) -> Moment {
        Machine::now(&self.inner)
    }

    fn frames(&self) -> &[[u8; WRAM_SIZE]] {
        Machine::frames(&self.inner)
    }
}

impl NesBackend<Vec<u8>> for Covered {
    fn export_nes(
        &mut self,
        snapshot: SnapId,
        base: Option<&Vec<u8>>,
    ) -> Result<Vec<u8>, MachineError> {
        NesBackend::export_nes(&mut self.inner, snapshot, base)
    }

    fn import_nes(&mut self, portable: &Vec<u8>) -> Result<SnapId, MachineError> {
        NesBackend::import_nes(&mut self.inner, portable)
    }

    fn release_exported(&mut self, snapshot: SnapId) -> Result<(), MachineError> {
        NesBackend::release_exported(&mut self.inner, snapshot)
    }

    fn keep_run_capture(&mut self, frames: usize) {
        NesBackend::keep_run_capture(&mut self.inner, frames);
    }

    fn capture_nes(&mut self, base: Option<&Vec<u8>>) -> Result<Vec<u8>, MachineError> {
        NesBackend::capture_nes(&mut self.inner, base)
    }

    fn restore_nes(&mut self, state: &Vec<u8>) -> Result<(), MachineError> {
        self.end_job();
        NesBackend::restore_nes(&mut self.inner, state)
    }
}

impl NesMemory for Covered {
    fn read_wram(&self) -> Result<[u8; WRAM_SIZE], MachineError> {
        NesMemory::read_wram(&self.inner)
    }

    fn poke_wram(&mut self, addr: usize, byte: u8) -> Result<(), MachineError> {
        NesMemory::poke_wram(&mut self.inner, addr, byte)
    }

    fn capture_save_ram_per_frame(&mut self, range: Range<usize>) {
        NesMemory::capture_save_ram_per_frame(&mut self.inner, range);
    }

    fn save_ram_frames(&self) -> &[u8] {
        NesMemory::save_ram_frames(&self.inner)
    }

    fn read_save_ram(&self) -> Result<Vec<u8>, MachineError> {
        NesMemory::read_save_ram(&self.inner)
    }

    fn write_save_ram(&mut self, offset: usize, bytes: &[u8]) -> Result<(), MachineError> {
        NesMemory::write_save_ram(&mut self.inner, offset, bytes)
    }
}

fn shared_log(output: &Path) -> Result<Arc<Mutex<Log>>, Box<dyn Error>> {
    Ok(Arc::new(Mutex::new(Log {
        out: Some(BufWriter::new(fs::File::create(
            output.join("actions.jsonl"),
        )?)),
        ..Log::default()
    })))
}

fn finish_log(log: &Arc<Mutex<Log>>, output: &Path, report: Value) -> Result<(), Box<dyn Error>> {
    log.lock()
        .map_err(|_| "coverage log lock poisoned")?
        .finish(&output.join("counts.json"))?;
    fs::write(
        output.join("summary.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{report}");
    Ok(())
}

fn campaign_smb(
    core: &Core,
    seed: u64,
    jobs: u64,
    output: &Path,
    mixture: Option<DrawMixture>,
) -> Result<(), Box<dyn Error>> {
    let rom = rom("HARMONY_SMB_ROM")?;
    let reference = SmbGame::new(&rom, &core.path, &core.sha256, &core.execution_sha256);
    let log = shared_log(output)?;
    let target_log = Arc::clone(&log);
    let target_rom = rom.clone();
    let target_core = Core {
        path: core.path.clone(),
        sha256: core.sha256.clone(),
        execution_sha256: core.execution_sha256.clone(),
    };
    let game: SmbGame<Covered, Vec<u8>> = SmbGame::with_targets(
        &rom,
        reference.emulator_identity().to_owned(),
        reference.snapshot_checkpoint_format(),
        Box::new(move || {
            let inner = target_core
                .machine(&target_rom)
                .map_err(|error| error.to_string())?;
            SmbTarget::from_machine(Covered {
                inner,
                log: Arc::clone(&target_log),
                ids: Vec::new(),
                place: &SMB_PLACE,
            })
            .map_err(|error| error.to_string())
        }),
    );
    let config = SmbCampaignConfig {
        vocabulary: SmbButtonVocabulary::default(),
        terminal: SmbTerminalPredicate::GameVictory,
        campaign_seed: seed,
        workers: 1,
        execution_budget: jobs,
        host: "coverage".to_owned(),
        wall_budget: None,
        continue_after_victory: false,
        archive_entry_limit: MAX_ARCHIVE_ENTRIES,
        window: default_window(1),
        memory_budget_mib: Some(2048),
        materialize_final_artifacts: false,
        retention: RetentionPolicy::Unprobed,
        mixture: mixture.unwrap_or(DrawMixture::EnergySplice { scale: 6 }),
        victory_input_path: None,
    };
    let mut stream = BufWriter::new(fs::File::create(output.join("stream.jsonl"))?);
    let started = now();
    let (report, _) = run_smb_campaign_checkpointed(
        &game,
        &config,
        &SmbCampaignOrigin::Genesis,
        &mut stream,
        None,
    )?;
    stream.flush()?;
    finish_log(
        &log,
        output,
        json!({
            "game": "smb",
            "seed": seed,
            "executions": report.executions_completed,
            "frames": report.execution_work,
            "retained": report.archive.retained,
            "wall_seconds": started.elapsed().as_secs_f64(),
            "core_sha256": core.sha256,
        }),
    )
}

fn campaign_metroid(
    core: &Core,
    seed: u64,
    jobs: u64,
    output: &Path,
    mixture: Option<DrawMixture>,
) -> Result<(), Box<dyn Error>> {
    let rom = rom("HARMONY_METROID_ROM")?;
    let image = machine::nes::with_cartridge_ram(&rom)?;
    let log = shared_log(output)?;
    let machine_log = Arc::clone(&log);
    let machine_core = Core {
        path: core.path.clone(),
        sha256: core.sha256.clone(),
        execution_sha256: core.execution_sha256.clone(),
    };
    let game: MetroidGame<Covered> = MetroidGame::with_machines(
        &rom,
        &machine::quicknes::core_identity(),
        &native_identity_sha256(&core.sha256, &core.execution_sha256),
        metroid_power_on(),
        GenesisDepth::NewGame,
        Box::new(move || {
            Ok(Covered {
                inner: machine_core.machine(&image)?,
                log: Arc::clone(&machine_log),
                ids: Vec::new(),
                place: &METROID_PLACE,
            })
        }),
    );
    let config = MetroidCampaignConfig {
        campaign_seed: seed,
        workers: 1,
        execution_budget: jobs,
        host: "coverage".to_owned(),
        wall_budget: None,
        continue_after_victory: true,
        archive_entry_limit: METROID_ARCHIVE_ENTRIES,
        memory_budget_mib: None,
        materialize_final_artifacts: false,
        retention: RetentionPolicy::Unprobed,
        mixture: mixture.unwrap_or(DrawMixture::AlphabetOnly),
        victory_input_path: None,
    };
    let mut stream = BufWriter::new(fs::File::create(output.join("stream.jsonl"))?);
    let started = now();
    let (report, _) = run_metroid_campaign_checkpointed(
        &game,
        &config,
        &MetroidCampaignOrigin::Genesis,
        &mut stream,
        None,
    )?;
    stream.flush()?;
    finish_log(
        &log,
        output,
        json!({
            "game": "metroid",
            "seed": seed,
            "executions": report.executions_completed,
            "frames": report.execution_work,
            "retained": report.archive.retained,
            "wall_seconds": started.elapsed().as_secs_f64(),
            "core_sha256": core.sha256,
        }),
    )
}
