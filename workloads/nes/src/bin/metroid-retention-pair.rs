// SPDX-License-Identifier: AGPL-3.0-or-later
//! Finite shared continuations from two exact captured states. Never fresh search.
use nes_workload::{
    metroid::{
        archive::sample_chord,
        boss_interval::{BossIntervalObserver, BossSlot, IntervalKind, classify},
        boss_probe::BossContext,
        target::{
            ButtonChord, MetroidMechanicalState, MetroidSnapshot, MetroidTarget,
            MetroidTerminalPolicy,
        },
    },
    search::rand::RomuDuoJrRand,
    target::{ExitKind, Target},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};
type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Pinned {
    path: PathBuf,
    sha256: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Bank {
    seeds: Vec<u64>,
    suffixes: Vec<Vec<ButtonChord>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    roots: [Pinned; 2],
    contexts: [serde_json::Value; 2],
    states: [MetroidMechanicalState; 2],
    core: Pinned,
    rom: Pinned,
    bank: Pinned,
    frames_per_arm: u64,
    physical_frame_ceiling: u64,
    wall_seconds: u64,
}
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn read(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("input exceeds 4 MiB".into());
    }
    Ok(bytes)
}
fn pinned(p: &Pinned) -> Result<Vec<u8>> {
    let bytes = read(&p.path)?;
    if sha(&bytes) != p.sha256 {
        return Err("pinned input changed".into());
    }
    Ok(bytes)
}
fn write(path: &Path, value: &impl Serialize) -> Result<()> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(&serde_json::to_vec_pretty(value)?)?;
    Ok(())
}
fn generate(seeds: Vec<u64>) -> Result<Bank> {
    if seeds.is_empty()
        || seeds.len() > 32
        || seeds
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != seeds.len()
    {
        return Err("bank needs 1..32 distinct seeds".into());
    }
    let suffixes = seeds
        .iter()
        .map(|seed| {
            let mut rng = RomuDuoJrRand::with_seed(*seed);
            (0..128)
                .map(|_| sample_chord(&mut rng))
                .collect::<Result<Vec<_>>>()
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Bank { seeds, suffixes })
}
fn validate(q: &Request, bank: &Bank) -> Result<()> {
    if !(1..=8192).contains(&q.frames_per_arm)
        || !(1..=120).contains(&q.wall_seconds)
        || q.physical_frame_ceiling > 1_100_000
        || q.physical_frame_ceiling < 929 + 4 * bank.seeds.len() as u64 * q.frames_per_arm
        || serde_json::to_vec(&generate(bank.seeds.clone())?)? != serde_json::to_vec(bank)?
    {
        return Err("invalid bounds or bank differs from ordinary draws".into());
    }
    Ok(())
}
fn emulator_bytes(snapshot: &MetroidSnapshot) -> Result<Vec<u8>> {
    Ok(serde_json::from_value(
        serde_json::to_value(snapshot)?["emulator_state"].clone(),
    )?)
}
fn same_boss(a: BossSlot, b: BossSlot) -> bool {
    (a.area, a.offset, a.data_index, a.attributes) == (b.area, b.offset, b.data_index, b.attributes)
}
fn defeated(context: &BossContext, area: u8) -> bool {
    match area {
        0x12 => context.memory.kraid_status & 1 != 0,
        0x14 => context.memory.ridley_status & 2 != 0,
        _ => false,
    }
}
fn restore(
    target: &mut MetroidTarget,
    snapshot: &MetroidSnapshot,
    context: &BossContext,
) -> Result<()> {
    let frames = target.frames_clocked();
    target.restore(snapshot)?;
    if target.snapshot().as_ref() != Some(snapshot)
        || target.diagnostic_boss_context()? != *context
        || target.frames_clocked() != frames
    {
        return Err("root restore mismatch".into());
    }
    Ok(())
}
#[derive(Default, Serialize)]
struct Cost {
    constructor_started: bool,
    setup: Option<u64>,
    continuation: u64,
    held_verification: u64,
}
impl Cost {
    fn used(&self) -> u64 {
        self.setup.unwrap_or(0) + self.continuation + self.held_verification
    }
}
fn apply(
    target: &mut MetroidTarget,
    action: &ButtonChord,
    cost: &mut Cost,
    held: bool,
    ceiling: u64,
) -> Result<()> {
    let duration = u64::from(action.bounded_hold_frames());
    if cost
        .used()
        .checked_add(duration)
        .is_none_or(|n| n > ceiling)
    {
        return Err("physical ceiling".into());
    }
    let before = target.frames_clocked();
    target.apply(action);
    let frames = target.frames_clocked() - before;
    if held {
        cost.held_verification += frames;
    } else {
        cost.continuation += frames;
    }
    if target.exit_kind() != ExitKind::Ok || frames != duration {
        return Err("emulator failure or frame mismatch".into());
    }
    Ok(())
}
#[derive(Serialize)]
struct Point {
    action: usize,
    frames: u64,
    complete: bool,
    dead: bool,
    defeat: bool,
    root_interval_valid: bool,
    new_hp_loss: Option<u64>,
    hp: Option<u8>,
    state: MetroidMechanicalState,
    emulator_sha256: String,
    context_sha256: String,
}
// A missing interval, reload, increase or unexplained drop invalidates the
// root-to-endpoint comparison permanently. A new restore starts a fresh baseline.
fn extend_chain(
    valid: &mut bool,
    loss: &mut u64,
    interval: Option<nes_workload::metroid::boss_interval::Interval>,
) {
    match interval {
        Some(i) if i.kind == IntervalKind::HpDrop => *loss += u64::from(i.hp_loss.unwrap_or(0)),
        Some(i) if i.kind == IntervalKind::Continuous => {}
        _ => *valid = false,
    }
}
#[allow(clippy::too_many_arguments)]
fn trial(
    target: &mut MetroidTarget,
    snapshot: &MetroidSnapshot,
    context: &BossContext,
    root: BossSlot,
    suffix: &[ButtonChord],
    q: &Request,
    cost: &mut Cost,
    epoch: u64,
) -> Result<serde_json::Value> {
    restore(target, snapshot, context)?;
    let mut observer = BossIntervalObserver::default();
    observer.observe(epoch, 0, context);
    let (mut frames, mut loss, mut valid) = (0, 0, true);
    let mut points = Vec::new();
    let mut executed = Vec::new();
    let mut stop = "action_limit";
    let mut end_context = context.clone();
    let mut first_invalid = None;
    for (i, action) in suffix.iter().enumerate() {
        let hold = action.bounded_hold_frames();
        if frames + u64::from(hold) > q.frames_per_arm {
            stop = "frame_limit";
            break;
        }
        let mut actual = 0;
        for _ in 0..hold {
            apply(
                target,
                &ButtonChord::new(action.buttons, 1),
                cost,
                false,
                q.physical_frame_ceiling,
            )?;
            frames += 1;
            actual += 1;
            end_context = target.diagnostic_boss_context()?;
            let interval =
                observer.observe(epoch, frames, &end_context)[usize::from(root.offset / 16)];
            extend_chain(&mut valid, &mut loss, interval);
            if !valid && first_invalid.is_none() {
                first_invalid = Some(json!({"frame":frames,"interval":interval}));
            }
            if target.is_dead() {
                stop = "death";
                break;
            }
            if end_context.memory.area != root.area {
                stop = "area_exit";
                break;
            }
        }
        executed.push(ButtonChord::new(action.buttons, actual));
        let end_slot = classify(&end_context, usize::from(root.offset / 16))
            .filter(|s| same_boss(*s, root) && s.hp != 255);
        points.push(Point {
            action: i + 1,
            frames,
            complete: actual == hold,
            dead: target.is_dead(),
            defeat: defeated(&end_context, root.area),
            root_interval_valid: valid,
            new_hp_loss: valid.then_some(loss),
            hp: end_slot.map(|s| s.hp),
            state: target.mechanical_state(),
            emulator_sha256: sha(&emulator_bytes(
                &target.snapshot().ok_or("boundary snapshot failed")?,
            )?),
            context_sha256: sha(&serde_json::to_vec(&end_context)?),
        });
        if stop == "death" || stop == "area_exit" {
            break;
        }
        if defeated(&end_context, root.area) {
            stop = "surviving_defeat";
            break;
        }
        if target.is_victory() {
            stop = "ending";
            break;
        }
    }
    let endpoint = target.snapshot().ok_or("endpoint snapshot failed")?;
    let endpoint_sha = sha(&emulator_bytes(&endpoint)?);
    let state = target.mechanical_state();
    let dead = target.is_dead();
    // Every episode gets a held-action endpoint replay, including clipped death
    // commands. A clipped command never becomes a completed paired boundary.
    restore(target, snapshot, context)?;
    for (action, point) in executed.iter().zip(&points) {
        apply(target, action, cost, true, q.physical_frame_ceiling)?;
        if target.mechanical_state() != point.state
            || target.is_dead() != point.dead
            || sha(&emulator_bytes(
                &target.snapshot().ok_or("held boundary failed")?,
            )?) != point.emulator_sha256
            || sha(&serde_json::to_vec(&target.diagnostic_boss_context()?)?) != point.context_sha256
        {
            return Err("held replay boundary differs".into());
        }
    }
    if sha(&emulator_bytes(
        &target.snapshot().ok_or("held snapshot failed")?,
    )?) != endpoint_sha
        || target.mechanical_state() != state
        || target.is_dead() != dead
        || target.diagnostic_boss_context()? != end_context
    {
        return Err("held replay differs".into());
    }
    Ok(
        json!({"points":points,"stop_reason":stop,"frames":frames,"dead":dead,
        "endpoint":state,"context":end_context,"endpoint_emulator_sha256":endpoint_sha,
        "executed":executed,"held_endpoint_verified":true,"held_boundaries_verified":points.len(),
        "first_invalid_root_interval":first_invalid,"root_hp":root.hp}),
    )
}
fn run(q: &Request, out: &Path, cost: &mut Cost) -> Result<()> {
    let bank: Bank = serde_json::from_slice(&pinned(&q.bank)?)?;
    validate(q, &bank)?;
    let rom = pinned(&q.rom)?;
    pinned(&q.core)?;
    let roots = q
        .roots
        .iter()
        .map(|p| {
            let bytes = pinned(p)?;
            let (root, remaining) = postcard::take_from_bytes::<MetroidSnapshot>(&bytes)?;
            if !remaining.is_empty() {
                return Err("snapshot has trailing bytes".into());
            }
            let emulator = emulator_bytes(&root)?;
            if emulator.get(48..112) != Some(q.core.sha256.as_bytes()) {
                return Err("snapshot core identity differs".into());
            }
            Ok(root)
        })
        .collect::<Result<Vec<_>>>()?;
    let wall = q.wall_seconds;
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(wall));
        std::process::exit(124);
    });
    cost.constructor_started = true;
    let mut target = MetroidTarget::from_rom_bytes_headless(&rom, &q.core.path, &q.core.sha256)?
        .with_terminal_policy(MetroidTerminalPolicy::BcdUnderflow);
    cost.setup = Some(target.frames_clocked());
    if cost.setup != Some(929) {
        return Err("setup changed".into());
    }
    let mut bosses = Vec::new();
    let mut contexts = Vec::new();
    for (i, root) in roots.iter().enumerate() {
        target.restore(root)?;
        let context = target.diagnostic_boss_context()?;
        restore(&mut target, root, &context)?;
        if serde_json::to_value(&context)? != q.contexts[i] {
            return Err("root context differs".into());
        }
        let slots: Vec<_> = (0..6).filter_map(|slot| classify(&context, slot)).collect();
        if target.is_dead()
            || target.is_victory()
            || target.mechanical_state() != q.states[i]
            || slots.len() != 1
            || slots[0].hp == 255
            || defeated(&context, slots[0].area)
        {
            return Err("root is not the qualified living undefeated single-boss state".into());
        }
        bosses.push(slots[0]);
        contexts.push(context);
    }
    if !same_boss(bosses[0], bosses[1]) {
        return Err("root boss identities differ".into());
    }
    let mut log = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out.join("trials.jsonl"))?;
    writeln!(log, "{}", json!({"kind":"qualified_roots","cost":cost}))?;
    log.flush()?;
    for (i, suffix) in bank.suffixes.iter().enumerate() {
        for arm in if i % 2 == 0 { [0, 1] } else { [1, 0] } {
            let outcome = trial(
                &mut target,
                &roots[arm],
                &contexts[arm],
                bosses[arm],
                suffix,
                q,
                cost,
                (2 * i + arm) as u64,
            )?;
            writeln!(
                log,
                "{}",
                json!({"kind":"trial","trial":i,"seed":bank.seeds[i],
                "arm":if arm==0 {"candidate"} else {"incumbent"},"outcome":outcome,"cost":cost})
            )?;
            log.flush()?;
        }
    }
    write(
        &out.join("complete.json"),
        &json!({"format":"metroid-retention-pair-v1","trials":bank.seeds.len(),
        "episodes":2*bank.seeds.len(),"cost":cost,"physical_frames":cost.used(),
        "scope":"One selected captured pair and finite shared suffix bank; not fresh search or an HP-only causal intervention."}),
    )
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [mode, seeds, out] if mode == "draws" => write(
            Path::new(out),
            &generate(serde_json::from_slice(&read(Path::new(seeds))?)?)?,
        ),
        [mode, request, out] if mode == "run" => {
            let bytes = read(Path::new(request))?;
            let q: Request = serde_json::from_slice(&bytes)?;
            let out = Path::new(out);
            fs::create_dir(out)?;
            let mut cost = Cost::default();
            let result = run(&q, out, &mut cost);
            write(
                &out.join("usage.json"),
                &json!({"request_sha256":sha(&bytes),"cost":cost,
                "physical_frames_known":cost.used(),"complete":result.is_ok(),
                "error":result.as_ref().err().map(ToString::to_string),
                "unknown":"Failed constructor work is unknown. Watchdog/interruption can omit work after the last complete trial receipt."}),
            )?;
            result
        }
        _ => Err("usage: metroid-retention-pair draws SEEDS OUT | run REQUEST OUTDIR".into()),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use nes_workload::metroid::boss_interval::Interval;
    #[test]
    fn frozen_bank_and_bounds_are_checked_before_emulator_construction() {
        let mut q: Request = serde_json::from_str(include_str!(
            "../../../../benchmarks/search/continuation-reassessment/pc01-request.json"
        ))
        .unwrap();
        let mut bank: Bank = serde_json::from_str(include_str!(
            "../../../../benchmarks/search/continuation-reassessment/pc01-bank.json"
        ))
        .unwrap();
        validate(&q, &bank).unwrap();
        q.physical_frame_ceiling = 4 * 32 * 8192; // Forgetting setup must fail.
        assert!(validate(&q, &bank).is_err());
        q.physical_frame_ceiling = 1_100_000;
        bank.suffixes[0][0].buttons ^= 1;
        assert!(validate(&q, &bank).is_err());
    }
    #[test]
    fn inherited_hp_cannot_count_and_invalid_intervals_cannot_recover() {
        let (mut valid, mut loss) = (true, 0);
        extend_chain(
            &mut valid,
            &mut loss,
            Some(Interval {
                kind: IntervalKind::Continuous,
                hp_loss: Some(0),
            }),
        );
        assert!(valid);
        assert_eq!(loss, 0);
        extend_chain(
            &mut valid,
            &mut loss,
            Some(Interval {
                kind: IntervalKind::HpDrop,
                hp_loss: Some(3),
            }),
        );
        assert!(valid);
        assert_eq!(loss, 3);
        extend_chain(&mut valid, &mut loss, None);
        extend_chain(
            &mut valid,
            &mut loss,
            Some(Interval {
                kind: IntervalKind::HpDrop,
                hp_loss: Some(2),
            }),
        );
        assert!(!valid);
        assert_eq!(loss, 5);
    }
    #[test]
    fn bank_is_reproducible_and_repeated_seeds_fail() {
        assert_eq!(
            serde_json::to_vec(&generate(vec![11, 12]).unwrap()).unwrap(),
            serde_json::to_vec(&generate(vec![11, 12]).unwrap()).unwrap()
        );
        assert!(generate(vec![11, 11]).is_err());
        assert!(generate(vec![]).is_err());
    }
}
