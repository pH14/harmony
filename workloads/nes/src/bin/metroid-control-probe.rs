// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounded conditional control from one searched encounter; never fresh validation.
use nes_workload::{
    metroid::{
        archive::sample_chord,
        boss_interval::{BossIntervalObserver, BossSlot, Interval, IntervalKind, classify},
        boss_probe::BossContext,
        target::{
            ButtonChord, MetroidInput, MetroidMechanicalState, MetroidSnapshot, MetroidTarget,
            MetroidTerminalPolicy,
        },
    },
    search::rand::RomuDuoJrRand,
    target::{ExitKind, Target},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Resources {
    health: u16,
    missiles: u8,
}

#[derive(Clone, Debug, Serialize)]
struct ResourceOperation {
    prefix_actions: usize,
    resources: Resources,
    before_snapshot_sha256: String,
    after_snapshot_sha256: String,
}

#[derive(Deserialize)]
struct Request {
    input: PathBuf,
    input_sha256: String,
    rom_sha256: String,
    core_sha256: String,
    expected_endpoint: MetroidMechanicalState,
    expected_context: Value,
    expected_emulator_sha256: String,
    trial_seeds: Vec<u64>,
    actions: usize,
    frames_per_arm: u64,
    total_frame_limit: u64,
    expected_suffix_sha256: Option<String>,
    counterfactual_resources: Option<Resources>,
}
impl Request {
    fn validate(&self) -> Result<()> {
        let unique: std::collections::BTreeSet<_> = self.trial_seeds.iter().collect();
        if self.trial_seeds.is_empty()
            || self.trial_seeds.len() > 32
            || unique.len() != self.trial_seeds.len()
            || !(1..=128).contains(&self.actions)
            || !(1..=8192).contains(&self.frames_per_arm)
            || !(1..=2_000_000).contains(&self.total_frame_limit)
        {
            return Err("conditional probe exceeds fixed limits or repeats a seed".into());
        }
        if let Some(resources) = &self.counterfactual_resources
            && (self.expected_endpoint.energy_tanks > 6
                || resources.health == 0
                || resources.health
                    > (u16::from(self.expected_endpoint.energy_tanks) + 1) * 1000 - 1
                || resources.missiles > self.expected_endpoint.missile_capacity)
        {
            return Err("counterfactual resources exceed earned capacities".into());
        }
        Ok(())
    }
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn write(path: &Path, value: &impl Serialize) -> Result<()> {
    fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}
fn emulator_hash(snapshot: &MetroidSnapshot) -> Result<String> {
    let value = serde_json::to_value(snapshot)?;
    let bytes: Vec<u8> = serde_json::from_value(value["emulator_state"].clone())?;
    Ok(hash(&bytes))
}
fn same_boss(a: BossSlot, b: BossSlot) -> bool {
    (a.area, a.offset, a.data_index, a.attributes) == (b.area, b.offset, b.data_index, b.attributes)
}
#[derive(Default)]
struct CommandDamage {
    start: Option<BossSlot>,
    valid: bool,
    dropped: bool,
}
impl CommandDamage {
    fn new(start: Option<BossSlot>, root: BossSlot) -> Self {
        Self {
            start,
            valid: start.is_some_and(|s| same_boss(s, root) && s.hp != 255),
            dropped: false,
        }
    }
    fn observe(&mut self, interval: Option<Interval>) {
        match interval.map(|x| x.kind) {
            Some(IntervalKind::HpDrop) => self.dropped = true,
            Some(IntervalKind::Continuous) => {}
            _ => self.valid = false,
        }
    }
    fn survives(&self, dead: bool, completed: bool, end: Option<BossSlot>) -> bool {
        !dead
            && completed
            && self.valid
            && self.dropped
            && self
                .start
                .zip(end)
                .is_some_and(|(a, b)| same_boss(a, b) && b.hp < a.hp)
    }
}
fn passive(suffix: &[ButtonChord]) -> Vec<ButtonChord> {
    suffix
        .iter()
        .map(|a| ButtonChord::new(0, a.bounded_hold_frames()))
        .collect()
}
fn fits(used: u64, duration: u64, limit: u64) -> bool {
    used.checked_add(duration).is_some_and(|n| n <= limit)
}
fn boss_defeated(context: &BossContext, area: u8) -> bool {
    match area {
        0x12 => context.memory.kraid_status & 1 != 0,
        0x14 => context.memory.ridley_status & 2 != 0,
        _ => false,
    }
}
struct Budget {
    used: u64,
    limit: u64,
}
impl Budget {
    fn room(&self, frames: u64) -> Result<()> {
        if !fits(self.used, frames, self.limit) {
            return Err("total physical frame limit reached".into());
        }
        Ok(())
    }
    fn apply(&mut self, target: &mut MetroidTarget, action: &ButtonChord) -> Result<()> {
        self.room(u64::from(action.bounded_hold_frames()))?;
        let before = target.frames_clocked();
        target.apply(action);
        self.used += target.frames_clocked() - before;
        if target.exit_kind() != ExitKind::Ok {
            return Err("probe emulator failure".into());
        }
        Ok(())
    }
}
fn prepare(
    core: &Path,
    rom: &[u8],
    core_hash: &str,
    input: &MetroidInput,
    budget: &mut Budget,
) -> Result<MetroidTarget> {
    budget.room(929)?; // Pinned ordinary new-game setup, checked immediately below.
    let mut target = MetroidTarget::from_rom_bytes_headless(rom, core, core_hash)?
        .with_terminal_policy(MetroidTerminalPolicy::BcdUnderflow);
    let setup = target.frames_clocked();
    budget.used += setup;
    if setup != 929 {
        return Err("ordinary genesis setup changed".into());
    }
    for action in &input.actions {
        if target.is_dead() || target.is_victory() {
            return Err("input continues after terminal".into());
        }
        budget.apply(&mut target, action)?;
    }
    Ok(target)
}
fn restore_verified(
    target: &mut MetroidTarget,
    snapshot: &MetroidSnapshot,
    context: &BossContext,
) -> Result<()> {
    target.restore(snapshot)?;
    let again = target.snapshot().ok_or("restored snapshot failed")?;
    if &again != snapshot || target.diagnostic_boss_context()? != *context {
        return Err("positive-root restore changed snapshot or same-boundary context".into());
    }
    Ok(())
}
#[derive(Clone, Serialize)]
struct Witness {
    input: MetroidInput,
    endpoint: MetroidMechanicalState,
    context: BossContext,
    emulator_sha256: String,
    continuation_frames: u64,
    completed_actions: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    resource_operation: Option<ResourceOperation>,
}
fn witness(
    target: &mut MetroidTarget,
    prefix: &MetroidInput,
    executed: &[ButtonChord],
    frames: u64,
) -> Result<Witness> {
    let mut actions = prefix.actions.clone();
    actions.extend_from_slice(executed);
    Ok(Witness {
        input: MetroidInput { actions },
        endpoint: target.mechanical_state(),
        context: target.diagnostic_boss_context()?,
        emulator_sha256: emulator_hash(&target.snapshot().ok_or("witness snapshot failed")?)?,
        continuation_frames: frames,
        completed_actions: executed.len(),
        resource_operation: None,
    })
}
#[derive(Serialize)]
struct Outcome {
    frames: u64,
    completed_actions: usize,
    stop_reason: &'static str,
    dead: bool,
    observed_hp_drop_events: u64,
    observed_hp_loss: u64,
    first_hp_drop_frame: Option<u64>,
    first_surviving_damage_frame: Option<u64>,
    observed_defeat_flag: bool,
    surviving_defeat_endpoint: bool,
    endpoint: MetroidMechanicalState,
    context: BossContext,
}
#[allow(clippy::too_many_arguments)]
fn trial(
    target: &mut MetroidTarget,
    snapshot: &MetroidSnapshot,
    root_context: &BossContext,
    root: BossSlot,
    prefix: &MetroidInput,
    suffix: &[ButtonChord],
    frame_limit: u64,
    epoch: u64,
    budget: &mut Budget,
) -> Result<(Outcome, Option<Witness>, Option<Witness>)> {
    restore_verified(target, snapshot, root_context)?;
    let slot = usize::from(root.offset / 16);
    let mut observer = BossIntervalObserver::default();
    observer.observe(epoch, 0, root_context);
    let mut out = Outcome {
        frames: 0,
        completed_actions: 0,
        stop_reason: "action_limit",
        dead: false,
        observed_hp_drop_events: 0,
        observed_hp_loss: 0,
        first_hp_drop_frame: None,
        first_surviving_damage_frame: None,
        observed_defeat_flag: false,
        surviving_defeat_endpoint: false,
        endpoint: target.mechanical_state(),
        context: root_context.clone(),
    };
    let mut executed = Vec::new();
    let (mut damage_witness, mut defeat_witness) = (None, None);
    'commands: for action in suffix {
        let hold = u64::from(action.bounded_hold_frames());
        if !fits(out.frames, hold, frame_limit) {
            out.stop_reason = "frame_limit";
            break;
        }
        let mut damage = CommandDamage::new(classify(&out.context, slot), root);
        for _ in 0..hold {
            budget.apply(target, &ButtonChord::new(action.buttons, 1))?;
            out.frames += 1;
            out.endpoint = target.mechanical_state();
            out.context = target.diagnostic_boss_context()?;
            let interval = observer.observe(epoch, out.frames, &out.context)[slot];
            damage.observe(interval);
            if let Some(Interval {
                kind: IntervalKind::HpDrop,
                hp_loss: Some(loss),
            }) = interval
            {
                out.observed_hp_drop_events += 1;
                out.observed_hp_loss += u64::from(loss);
                out.first_hp_drop_frame.get_or_insert(out.frames);
            }
            out.observed_defeat_flag |= boss_defeated(&out.context, root.area);
            out.dead = target.is_dead();
            if out.dead {
                out.stop_reason = "death";
                break 'commands;
            }
            if out.context.memory.area != root.area {
                out.stop_reason = "area_exit";
                break 'commands;
            }
        }
        executed.push(*action);
        out.completed_actions += 1;
        if damage.survives(out.dead, true, classify(&out.context, slot)) && damage_witness.is_none()
        {
            out.first_surviving_damage_frame = Some(out.frames);
            damage_witness = Some(witness(target, prefix, &executed, out.frames)?);
        }
        if boss_defeated(&out.context, root.area) {
            out.surviving_defeat_endpoint = true;
            out.stop_reason = "surviving_defeat";
            defeat_witness = Some(witness(target, prefix, &executed, out.frames)?);
            break;
        }
        if target.is_victory() {
            out.stop_reason = "ending";
            break;
        }
    }
    Ok((out, damage_witness, defeat_witness))
}
fn verify_witness(
    core: &Path,
    rom: &[u8],
    core_hash: &str,
    value: &Witness,
    budget: &mut Budget,
) -> Result<u64> {
    let before = budget.used;
    for _ in 0..2 {
        let mut target = if let Some(operation) = &value.resource_operation {
            let (prefix, suffix) =
                split_counterfactual_input(&value.input, operation.prefix_actions)?;
            let mut target = prepare(core, rom, core_hash, &prefix, budget)?;
            replay_resource_operation(&mut target, operation)?;
            for action in &suffix.actions {
                if target.is_dead() || target.is_victory() {
                    return Err("counterfactual witness continues after terminal".into());
                }
                budget.apply(&mut target, action)?;
            }
            target
        } else {
            prepare(core, rom, core_hash, &value.input, budget)?
        };
        if target.is_dead()
            || target.mechanical_state() != value.endpoint
            || target.diagnostic_boss_context()? != value.context
            || emulator_hash(&target.snapshot().ok_or("verification snapshot failed")?)?
                != value.emulator_sha256
        {
            return Err("held-action witness differs from the one-frame surviving endpoint".into());
        }
    }
    Ok(budget.used - before)
}

fn split_counterfactual_input(
    input: &MetroidInput,
    boundary: usize,
) -> Result<(MetroidInput, MetroidInput)> {
    if boundary == 0 || boundary >= input.actions.len() {
        return Err("counterfactual witness requires an interior prefix boundary".into());
    }
    let (prefix, suffix) = input.actions.split_at(boundary);
    Ok((
        MetroidInput {
            actions: prefix.to_vec(),
        },
        MetroidInput {
            actions: suffix.to_vec(),
        },
    ))
}

fn snapshot_hash(target: &mut MetroidTarget) -> Result<String> {
    Ok(hash(&postcard::to_allocvec(
        &target.snapshot().ok_or("resource snapshot failed")?,
    )?))
}

fn replay_resource_operation(
    target: &mut MetroidTarget,
    operation: &ResourceOperation,
) -> Result<()> {
    if snapshot_hash(target)? != operation.before_snapshot_sha256 {
        return Err("counterfactual witness has the wrong pre-intervention snapshot".into());
    }
    target.diagnostic_set_resources(operation.resources.health, operation.resources.missiles)?;
    if snapshot_hash(target)? != operation.after_snapshot_sha256 {
        return Err("counterfactual witness has the wrong intervened snapshot".into());
    }
    Ok(())
}
fn generate(request: &Request) -> Result<Vec<Vec<ButtonChord>>> {
    request
        .trial_seeds
        .iter()
        .map(|seed| {
            let mut rng = RomuDuoJrRand::with_seed(*seed);
            (0..request.actions)
                .map(|_| sample_chord(&mut rng))
                .collect()
        })
        .collect()
}
#[allow(clippy::too_many_arguments)]
fn run(
    core: &Path,
    rom: &[u8],
    request: &Request,
    suffixes: &[Vec<ButtonChord>],
    output: &Path,
    budget: &mut Budget,
) -> Result<Value> {
    if hash(rom) != request.rom_sha256 || hash(&fs::read(core)?) != request.core_sha256 {
        return Err("ROM/core identity differs from the frozen request".into());
    }
    let bytes = fs::read(&request.input)?;
    if hash(&bytes) != request.input_sha256 {
        return Err("searched input identity changed".into());
    }
    let prefix: MetroidInput = serde_json::from_slice(&bytes)?;
    if prefix.actions.is_empty()
        || prefix.actions.len() > 8192
        || prefix
            .actions
            .iter()
            .map(|a| u64::from(a.bounded_hold_frames()))
            .sum::<u64>()
            > 250_000
    {
        return Err("searched prefix exceeds fixed bounds".into());
    }
    let mut target = prepare(core, rom, &request.core_sha256, &prefix, budget)?;
    let context = target.diagnostic_boss_context()?;
    let snapshot = target.snapshot().ok_or("root snapshot failed")?;
    if target.is_dead()
        || target.mechanical_state() != request.expected_endpoint
        || serde_json::to_value(&context)? != request.expected_context
        || emulator_hash(&snapshot)? != request.expected_emulator_sha256
    {
        return Err("root differs from qualified positive encounter".into());
    }
    let bosses: Vec<_> = (0..6).filter_map(|slot| classify(&context, slot)).collect();
    if bosses.len() != 1 || boss_defeated(&context, bosses[0].area) {
        return Err("root must contain exactly one undefeated classified boss".into());
    }
    let root = bosses[0];
    let prefix_frames = budget.used;
    write(
        &output.join("root.json"),
        &json!({"endpoint":target.mechanical_state(),"context":context,
        "emulator_sha256":request.expected_emulator_sha256,"physical_frames":prefix_frames}),
    )?;
    let operation = if let Some(resources) = &request.counterfactual_resources {
        write(&output.join("resource-before-snapshot.json"), &snapshot)?;
        let before_snapshot_sha256 = snapshot_hash(&mut target)?;
        let old = target.mechanical_state();
        target.diagnostic_set_resources(old.health, old.missiles)?;
        if snapshot_hash(&mut target)? != before_snapshot_sha256 {
            return Err("no-op resource intervention changed the full snapshot".into());
        }
        target.diagnostic_set_resources(resources.health, resources.missiles)?;
        if target.diagnostic_boss_context()? != context {
            return Err("resource intervention changed boss context".into());
        }
        let operation = ResourceOperation {
            prefix_actions: prefix.actions.len(),
            resources: resources.clone(),
            before_snapshot_sha256,
            after_snapshot_sha256: snapshot_hash(&mut target)?,
        };
        write(&output.join("resource-operation.json"), &operation)?;
        Some(operation)
    } else {
        None
    };
    let snapshot = if operation.is_some() {
        target.snapshot().ok_or("intervened root snapshot failed")?
    } else {
        snapshot
    };
    if operation.is_some() {
        write(&output.join("resource-root-snapshot.json"), &snapshot)?;
    }
    let mut log = fs::File::create(output.join("trials.jsonl"))?;
    let mut saved: [[Option<Witness>; 2]; 2] = Default::default();
    let mut rows = Vec::new();
    for (index, suffix) in suffixes.iter().enumerate() {
        let released = passive(suffix);
        for arm in if index % 2 == 0 { [0, 1] } else { [1, 0] } {
            let commands = if arm == 0 {
                suffix.as_slice()
            } else {
                &released
            };
            let (outcome, damage, defeat) = trial(
                &mut target,
                &snapshot,
                &context,
                root,
                &prefix,
                commands,
                request.frames_per_arm,
                (index * 2 + arm) as u64,
                budget,
            )?;
            for (kind, mut candidate) in [damage, defeat].into_iter().enumerate() {
                if let Some(value) = &mut candidate {
                    value.resource_operation = operation.clone();
                }
                if saved[arm][kind].is_none() {
                    saved[arm][kind] = candidate;
                }
            }
            let row = json!({"trial":index,"seed":request.trial_seeds[index],
                "arm":if arm==0 {"ordinary"} else {"passive"},"outcome":outcome,
                "cumulative_physical_frames":budget.used});
            writeln!(log, "{row}")?;
            log.flush()?;
            rows.push(row);
        }
    }
    let trial_frames = budget.used - prefix_frames;
    drop(target);
    let mut witnesses = Vec::new();
    for (arm, kinds) in saved.iter().enumerate() {
        for (kind, value) in kinds.iter().enumerate() {
            if let Some(value) = value {
                let name = format!(
                    "{}-{}.json",
                    if arm == 0 { "ordinary" } else { "passive" },
                    if kind == 0 {
                        "surviving-damage"
                    } else {
                        "surviving-defeat"
                    }
                );
                let frames = verify_witness(core, rom, &request.core_sha256, value, budget)?;
                write(&output.join(&name), value)?;
                witnesses.push(
                    json!({"file":name,"sha256":hash(&fs::read(output.join(&name))?),
                    "verified_held_replays":2,"physical_verification_frames":frames,
                    "continuation_frames":value.continuation_frames}),
                );
            }
        }
    }
    let mut result = json!({"format":"metroid-conditional-control-v1","scope":"one searched encounter; diagnostic only",
        "prefix_physical_frames":prefix_frames,"continuation_physical_frames":trial_frames,
        "physical_frames":budget.used,"verified_positive_root_restores":rows.len(),
        "trials":rows,"witnesses":witnesses,
        "limitations":["One-frame diagnostics stop at corrected death; this is not held-campaign throughput.",
            "Observed HP losses are not exact lifetime damage or a proof against invisible same-key reloads.",
            "One weak-resource root and a finite suffix list cannot establish fight impossibility or a global retention/selection cause."]});
    if let Some(operation) = operation {
        result["format"] = json!("metroid-resource-counterfactual-v1");
        result["scope"] = json!(
            "Artificial resource intervention at a searched boundary; never a generated state, ordinary input witness or fresh-search result."
        );
        result["resource_operation"] = serde_json::to_value(operation)?;
    }
    Ok(result)
}
fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (request_path, output, execute) = match args.as_slice() {
        [mode, request, output] if mode == "draws" => (request, output, None),
        [mode, core, rom, request, output] if mode == "run" => (request, output, Some((core, rom))),
        _ => {
            return Err(
                "usage: metroid-control-probe draws REQUEST OUT | run CORE ROM REQUEST OUT".into(),
            );
        }
    };
    let request_bytes = fs::read(request_path)?;
    let request: Request = serde_json::from_slice(&request_bytes)?;
    request.validate()?;
    if execute.is_some() && request.expected_suffix_sha256.is_none() {
        return Err("run requires the independently frozen suffix hash".into());
    }
    let suffixes = generate(&request)?;
    let draw_bytes = serde_json::to_vec(&suffixes)?;
    if let Some(expected) = &request.expected_suffix_sha256
        && *expected != hash(&draw_bytes)
    {
        return Err("generated suffix identity changed".into());
    }
    let output = Path::new(output);
    fs::create_dir(output)?;
    fs::write(output.join("suffixes.json"), &draw_bytes)?;
    if let Some((core, rom)) = execute {
        let mut budget = Budget {
            used: 0,
            limit: request.total_frame_limit,
        };
        let result = run(
            Path::new(core),
            &fs::read(rom)?,
            &request,
            &suffixes,
            output,
            &mut budget,
        );
        write(
            &output.join("usage.json"),
            &json!({"physical_frames_known":budget.used,
            "complete":result.is_ok(),"request_sha256":hash(&request_bytes),
            "suffix_sha256":hash(&draw_bytes),"error":result.as_ref().err().map(ToString::to_string),
            "unknown":"An interrupted process can omit the currently executing trial or verification; preserve the last complete log and bound that gap separately."}),
        )?;
        write(&output.join("summary.json"), &result?)?;
    }
    println!(
        "{}",
        json!({"suffix_sha256":hash(&draw_bytes),"trials":suffixes.len()})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn intervention_requires_an_interior_boundary_and_preserves_both_tape_parts() {
        let input = MetroidInput {
            actions: vec![ButtonChord::new(1, 2), ButtonChord::new(2, 3)],
        };
        for boundary in [0, 2, 3, usize::MAX] {
            assert!(split_counterfactual_input(&input, boundary).is_err());
        }
        let (prefix, suffix) = split_counterfactual_input(&input, 1).unwrap();
        assert_eq!(prefix.actions, input.actions[..1]);
        assert_eq!(suffix.actions, input.actions[1..]);
    }
    fn boss(hp: u8) -> BossSlot {
        BossSlot {
            area: 20,
            offset: 0,
            data_index: 9,
            attributes: 64,
            status: 1,
            hp,
        }
    }
    fn drop() -> Option<Interval> {
        Some(Interval {
            kind: IntervalKind::HpDrop,
            hp_loss: Some(4),
        })
    }
    #[test]
    fn death_and_incomplete_commands_never_make_surviving_progress() {
        let mut d = CommandDamage::new(Some(boss(140)), boss(140));
        d.observe(drop());
        assert!(d.survives(false, true, Some(boss(136))));
        assert!(!d.survives(true, true, Some(boss(136))));
        assert!(!d.survives(false, false, Some(boss(136))));
        assert!(!d.survives(false, true, Some(boss(140))));
    }
    #[test]
    fn missing_changed_or_reset_context_cannot_inherit_a_drop() {
        for invalid in [
            None,
            Some(Interval {
                kind: IntervalKind::Baseline,
                hp_loss: None,
            }),
            Some(Interval {
                kind: IntervalKind::HpIncrease,
                hp_loss: None,
            }),
            Some(Interval {
                kind: IntervalKind::LeftOrUnclassified,
                hp_loss: None,
            }),
        ] {
            let mut d = CommandDamage::new(Some(boss(140)), boss(140));
            d.observe(drop());
            d.observe(invalid);
            assert!(!d.survives(false, true, Some(boss(136))));
        }
        let mut changed = boss(136);
        changed.data_index = 3;
        let mut d = CommandDamage::new(Some(boss(140)), boss(140));
        d.observe(drop());
        assert!(!d.survives(false, true, Some(changed)));
        let prior = CommandDamage::new(Some(boss(136)), boss(140));
        assert!(!prior.survives(false, true, Some(boss(136))));
    }
    #[test]
    fn passive_control_preserves_all_durations_and_budget_admission() {
        let suffix = [
            ButtonChord::new(4, 2),
            ButtonChord::new(0x81, 120),
            ButtonChord::new(3, 9),
        ];
        let zero = passive(&suffix);
        assert!(zero.iter().all(|a| a.buttons == 0));
        for (a, b) in suffix.iter().zip(&zero) {
            assert_eq!(a.bounded_hold_frames(), b.bounded_hold_frames());
        }
        assert!(fits(8000, 120, 8192));
        assert!(!fits(8100, 120, 8192));
        assert!(!fits(u64::MAX, 1, u64::MAX));
    }
}
