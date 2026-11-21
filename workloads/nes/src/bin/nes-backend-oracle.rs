// SPDX-License-Identifier: AGPL-3.0-or-later
use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
};

use machine::nes::ButtonChord;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

const USAGE: &str = "usage: nes-backend-oracle native ROM QUICKNES_CORE OUT\n       nes-backend-oracle vm ROM KERNEL PLATFORM_INITRAMFS NES_OCI_IMAGE OUT\n       nes-backend-oracle compare NATIVE_OUT VM_OUT";
const LEG_FORMAT: &str = "nes-backend-oracle-leg-v1";
const COMPARE_FORMAT: &str = "nes-backend-oracle-compare-v1";
const RECORDS_FILE: &str = "target.jsonl";
const CAMPAIGN_DIR: &str = "campaign";
const SUMMARY_FILE: &str = "compare.json";
const SEQUENCES: u8 = 8;
const CHORDS: u8 = 16;
const BUTTONS: [u8; 8] = [0, 0x80, 0x81, 0x83, 0x40, 0x41, 0x01, 0x82];
const HOLDS: [u8; 5] = [1, 4, 16, 60, 120];
const BACKEND_HEADER_FIELDS: [&str; 2] = ["emulator_backend", "executable_identity"];

type Outcome = Result<(), Box<dyn Error>>;

fn main() -> Outcome {
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let Some((mode, rest)) = args.split_first() else {
        return Err(USAGE.into());
    };
    match mode.to_str() {
        Some("native") => legs::native(rest),
        Some("vm") => legs::vm(rest),
        Some("compare") => compare(rest),
        _ => Err(USAGE.into()),
    }
}

fn planned(sequence: u8, index: u8) -> ButtonChord {
    ButtonChord::new(
        BUTTONS[usize::from(sequence.wrapping_add(index) % 8)],
        HOLDS[usize::from(index % 5)],
    )
}

fn plan() -> Value {
    json!({
        "sequences": SEQUENCES,
        "chords": CHORDS,
        "buttons": BUTTONS,
        "holds": HOLDS,
    })
}

#[cfg(not(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
)))]
mod legs {
    use std::path::PathBuf;

    use super::Outcome;

    const UNAVAILABLE: &str = "the native and vm legs require Linux KVM and the consonance feature";

    pub fn native(_: &[PathBuf]) -> Outcome {
        Err(UNAVAILABLE.into())
    }

    pub fn vm(_: &[PathBuf]) -> Outcome {
        Err(UNAVAILABLE.into())
    }
}

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
mod legs {
    use std::{
        fs,
        io::{BufWriter, Write},
        path::{Path, PathBuf},
    };

    use nes_games::{
        nes_backend::{NesBackend, SnapshotState},
        smb::game::SuperMarioBros,
        workload::{Game, NesObservation, NesTarget, NesWorkload},
    };
    use nes_workload::{
        package::{self, RomKind, SearchOptions, SearchStart},
        target::Target,
    };
    use searcher::search::campaign::{Reporting, TargetExecution};
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};

    use super::{
        CAMPAIGN_DIR, CHORDS, LEG_FORMAT, Outcome, RECORDS_FILE, SEQUENCES, USAGE, plan, planned,
    };

    const CAMPAIGN_SEED: u64 = 1;
    const CAMPAIGN_EXECUTIONS: u64 = 400;

    pub fn native(args: &[PathBuf]) -> Outcome {
        let [rom, core, out] = args else {
            return Err(USAGE.into());
        };
        if cfg!(target_env = "musl") {
            return Err(
                "the native leg loads the QuickNES core, so it runs in the GNU build".into(),
            );
        }
        let rom = fs::read(rom)?;
        RomKind::identify(&rom)?;
        let core_sha256 = format!("{:x}", Sha256::digest(fs::read(core)?));
        let execution_sha256 = machine::quicknes::prepared_execution_sha256(core, &core_sha256)?;
        let workload =
            NesWorkload::new(SuperMarioBros, &rom, core, &core_sha256, &execution_sha256)?;
        start_leg(out)?;
        write_records(&workload, "native", &rom, out)?;
        package::search_native(
            &rom,
            core,
            &core_sha256,
            &campaign_options(out),
            &SearchStart::Genesis,
        )?;
        Ok(())
    }

    pub fn vm(args: &[PathBuf]) -> Outcome {
        let [rom, kernel, platform, image, out] = args else {
            return Err(USAGE.into());
        };
        if !cfg!(target_env = "musl") {
            return Err(
                "the vm leg runs in the musl build, started through scripts/linux-sealed-test-runner.py"
                    .into(),
            );
        }
        let rom = fs::read(rom)?;
        RomKind::identify(&rom)?;
        let kernel = fs::read(kernel)?;
        let platform = fs::read(platform)?;
        let prepared = nes_workload::prepare::stage_and_prepare(
            image.to_str().ok_or("NES OCI image path must be UTF-8")?,
            &rom,
        )?;
        let initramfs = prepared.initramfs(&platform);
        let workload = NesWorkload::new_consonance(SuperMarioBros, &rom, &kernel, &initramfs)?;
        start_leg(out)?;
        write_records(&workload, "vm", &rom, out)?;
        package::search_consonance(
            &rom,
            &kernel,
            &prepared,
            &platform,
            &campaign_options(out),
            &SearchStart::Genesis,
        )?;
        Ok(())
    }

    fn start_leg(out: &Path) -> Outcome {
        fs::create_dir_all(out)?;
        if fs::read_dir(out)?.next().is_some() {
            return Err(format!("{} must be empty", out.display()).into());
        }
        Ok(())
    }

    fn campaign_options(out: &Path) -> SearchOptions {
        SearchOptions {
            seed: CAMPAIGN_SEED,
            workers: 1,
            executions: CAMPAIGN_EXECUTIONS,
            output: out.join(CAMPAIGN_DIR),
            wall_seconds: None,
            checkpoint_owner: None,
        }
    }

    fn outcome<S>(observation: &NesObservation<S>) -> Option<&'static str> {
        if observation.goal {
            Some("goal")
        } else if observation.rejected.is_some() {
            Some("rejected")
        } else if observation.overran {
            Some("overran")
        } else {
            None
        }
    }

    fn applied<M: NesBackend<P>, P: SnapshotState>(
        target: &NesTarget<SuperMarioBros, M, P>,
    ) -> Value {
        let observation = target.observe();
        json!({
            "exit_kind": format!("{:?}", target.exit_kind()),
            "actions": target.last_action_observations(),
            "observation": observation,
            "outcome": outcome(&observation),
        })
    }

    fn write_records<M: NesBackend<P>, P: SnapshotState>(
        workload: &NesWorkload<SuperMarioBros, M, P>,
        leg: &str,
        rom: &[u8],
        out: &Path,
    ) -> Outcome {
        let mut file = BufWriter::new(fs::File::create(out.join(RECORDS_FILE))?);
        let mut line = |value: Value| -> Outcome {
            serde_json::to_writer(&mut file, &value)?;
            file.write_all(b"\n")?;
            Ok(())
        };
        line(json!({
            "format": LEG_FORMAT,
            "leg": leg,
            "game": SuperMarioBros::NAME,
            "rom_sha256": format!("{:x}", Sha256::digest(rom)),
            "plan": plan(),
            "emulator_identity": workload.emulator_identity(),
            "executable_identity": workload.executable_identity()?,
        }))?;
        let mut target = workload.new_target()?;
        line(json!({"record": "genesis", "observation": target.observe()}))?;
        let genesis = target.snapshot().ok_or("the genesis snapshot failed")?;
        let (mut sequences, mut actions) = (0_u64, 0_u64);
        for sequence in 0..SEQUENCES {
            target.restore(&genesis)?;
            line(json!({
                "record": "sequence",
                "sequence": sequence,
                "observation": target.observe(),
            }))?;
            sequences += 1;
            for index in 0..CHORDS {
                let chord = planned(sequence, index);
                let before = target.snapshot().ok_or_else(|| {
                    format!("the snapshot before sequence {sequence} action {index} failed")
                })?;
                target.apply(&chord);
                let first = applied(&target);
                target.restore(&before)?;
                let restored = target.observe();
                target.apply(&chord);
                let again = applied(&target);
                let stops = target.observe().stops();
                line(json!({
                    "record": "action",
                    "sequence": sequence,
                    "index": index,
                    "chord": chord,
                    "applied": first,
                    "restored": restored,
                    "reapplied": again,
                }))?;
                actions += 1;
                if stops {
                    break;
                }
            }
        }
        line(json!({"record": "end", "sequences": sequences, "actions": actions}))?;
        file.flush()?;
        Ok(())
    }
}

fn lines(path: &Path) -> Result<Vec<String>, Box<dyn Error>> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    Ok(text.lines().map(str::to_owned).collect())
}

fn parse(line: &str, what: &str) -> Result<Value, Box<dyn Error>> {
    serde_json::from_str(line).map_err(|error| format!("{what} is not JSON: {error}").into())
}

fn sha256_after_header(lines: &[String]) -> String {
    let mut digest = Sha256::new();
    for line in lines.iter().skip(1) {
        digest.update(line.as_bytes());
        digest.update(b"\n");
    }
    format!("{:x}", digest.finalize())
}

fn joined(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_owned()
    } else {
        format!("{path}.{key}")
    }
}

fn first_difference(left: &Value, right: &Value, path: &str) -> String {
    if let (Value::Object(left), Value::Object(right)) = (left, right) {
        for (key, value) in left {
            match right.get(key) {
                Some(other) if other == value => {}
                Some(other) => return first_difference(value, other, &joined(path, key)),
                None => return joined(path, key),
            }
        }
        if let Some(key) = right.keys().find(|key| !left.contains_key(*key)) {
            return joined(path, key);
        }
    }
    if path.is_empty() {
        "the whole record".to_owned()
    } else {
        path.to_owned()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Boundary {
    Genesis,
    Sequence(u64),
    Action(u64, u64),
    End,
}

impl Boundary {
    fn of(record: &Value) -> Result<Self, Box<dyn Error>> {
        let number = |field: &str| {
            record[field]
                .as_u64()
                .ok_or_else(|| format!("a record has no {field} number"))
        };
        match record["record"].as_str() {
            Some("genesis") => Ok(Self::Genesis),
            Some("sequence") => Ok(Self::Sequence(number("sequence")?)),
            Some("action") => Ok(Self::Action(number("sequence")?, number("index")?)),
            Some("end") => Ok(Self::End),
            _ => Err("a record names no known record kind".into()),
        }
    }

    fn order(self) -> (u64, u64, u64) {
        match self {
            Self::Genesis => (0, 0, 0),
            Self::Sequence(sequence) => (sequence.saturating_add(1), 0, 0),
            Self::Action(sequence, index) => (sequence.saturating_add(1), 1, index),
            Self::End => (u64::MAX, 0, 0),
        }
    }

    fn name(self) -> String {
        match self {
            Self::Genesis => "the genesis record".to_owned(),
            Self::Sequence(sequence) => format!("sequence {sequence}"),
            Self::Action(sequence, index) => format!("sequence {sequence} action {index}"),
            Self::End => "the end record".to_owned(),
        }
    }
}

fn header(lines: &[String], leg: &str) -> Result<Map<String, Value>, Box<dyn Error>> {
    let first = lines
        .first()
        .ok_or_else(|| format!("the {leg} leg wrote no records"))?;
    let Value::Object(header) = parse(first, &format!("the {leg} header"))? else {
        return Err(format!("the {leg} header is not an object").into());
    };
    if header.get("format").and_then(Value::as_str) != Some(LEG_FORMAT) {
        return Err(format!("the {leg} records are not {LEG_FORMAT}").into());
    }
    if header.get("leg").and_then(Value::as_str) != Some(leg) {
        return Err(format!("the {leg} directory holds another leg's records").into());
    }
    if header.get("plan") != Some(&plan()) {
        return Err(format!("the {leg} leg ran another plan").into());
    }
    Ok(header)
}

fn compare_boundaries(native: &[String], vm: &[String]) -> Outcome {
    let parsed =
        |line: Option<&String>, leg: &str| -> Result<Option<(Value, Boundary)>, Box<dyn Error>> {
            line.map(|line| {
                let record = parse(line, &format!("a {leg} record"))?;
                let boundary = Boundary::of(&record)?;
                Ok((record, boundary))
            })
            .transpose()
        };
    for position in 1..native.len().max(vm.len()) {
        if native.get(position) == vm.get(position) {
            continue;
        }
        let message = match (
            parsed(native.get(position), "native")?,
            parsed(vm.get(position), "vm")?,
        ) {
            (Some((left, boundary)), Some((right, other))) if boundary == other => format!(
                "the legs differ at {}: {}",
                boundary.name(),
                first_difference(&left, &right, "")
            ),
            (Some((_, boundary)), Some((_, other))) if other.order() < boundary.order() => {
                format!("the vm leg has an extra boundary at {}", other.name())
            }
            (Some((_, boundary)), _) => format!("the vm leg is missing {}", boundary.name()),
            (None, Some((_, other))) => format!("the native leg is missing {}", other.name()),
            (None, None) => continue,
        };
        return Err(message.into());
    }
    Ok(())
}

struct OpenSequence {
    number: u64,
    actions: u64,
    stopped: bool,
}

fn close_sequence(open: Option<&OpenSequence>, leg: &str) -> Outcome {
    match open {
        Some(open) if !open.stopped && open.actions < u64::from(CHORDS) => Err(format!(
            "the {leg} leg's sequence {} ends after {} of {CHORDS} chords without a terminal outcome",
            open.number, open.actions
        )
        .into()),
        _ => Ok(()),
    }
}

fn check_leg(lines: &[String], leg: &str) -> Result<u64, Box<dyn Error>> {
    let genesis = match lines
        .get(1)
        .map(|line| parse(line, &format!("a {leg} record")))
    {
        Some(record) => {
            let record = record?;
            if Boundary::of(&record)? != Boundary::Genesis {
                return Err(format!("the {leg} leg does not start with its genesis record").into());
            }
            record["observation"].clone()
        }
        None => return Err(format!("the {leg} leg has no genesis record").into()),
    };
    let mut open: Option<OpenSequence> = None;
    let mut previous = genesis.clone();
    let (mut sequences, mut actions) = (0_u64, 0_u64);
    for (position, line) in lines.iter().enumerate().skip(2) {
        let record = parse(line, &format!("a {leg} record"))?;
        let boundary = Boundary::of(&record)?;
        let at = boundary.name();
        match boundary {
            Boundary::Sequence(sequence) => {
                close_sequence(open.as_ref(), leg)?;
                if sequence != sequences || sequence >= u64::from(SEQUENCES) {
                    return Err(format!("the {leg} leg records {at} out of order").into());
                }
                if record["observation"] != genesis {
                    return Err(format!(
                        "the {leg} leg's restored genesis at {at} differs from its genesis"
                    )
                    .into());
                }
                open = Some(OpenSequence {
                    number: sequence,
                    actions: 0,
                    stopped: false,
                });
                previous = record["observation"].clone();
                sequences += 1;
            }
            Boundary::Action(sequence, index) => {
                let Some(current) = open.as_mut().filter(|current| {
                    current.number == sequence
                        && current.actions == index
                        && !current.stopped
                        && index < u64::from(CHORDS)
                }) else {
                    return Err(format!("the {leg} leg records {at} out of place").into());
                };
                let expected = planned(u8::try_from(sequence)?, u8::try_from(index)?);
                if record["chord"] != serde_json::to_value(expected)? {
                    return Err(format!("the {leg} leg's chord at {at} is not the plan's").into());
                }
                for run in ["applied", "reapplied"] {
                    if record[run]["exit_kind"] != "Ok" {
                        return Err(
                            format!("the {leg} leg's {run} chord at {at} did not exit Ok").into(),
                        );
                    }
                }
                if record["reapplied"] != record["applied"] {
                    return Err(format!(
                        "the {leg} leg's reapplied chord at {at} differs from its first run: {}",
                        first_difference(&record["applied"], &record["reapplied"], "")
                    )
                    .into());
                }
                if record["restored"] != previous {
                    return Err(format!(
                        "the {leg} leg's restore before {at} did not return to the observation before the chord"
                    )
                    .into());
                }
                previous = record["reapplied"]["observation"].clone();
                current.stopped = !record["reapplied"]["outcome"].is_null();
                current.actions += 1;
                actions += 1;
            }
            Boundary::End if position + 1 == lines.len() => {
                close_sequence(open.as_ref(), leg)?;
                if sequences != u64::from(SEQUENCES) {
                    return Err(format!(
                        "the {leg} leg ran {sequences} of the plan's {SEQUENCES} sequences"
                    )
                    .into());
                }
                if record["sequences"].as_u64() != Some(sequences)
                    || record["actions"].as_u64() != Some(actions)
                {
                    return Err(format!(
                        "the {leg} leg's end record does not count the records it follows"
                    )
                    .into());
                }
                return Ok(actions);
            }
            _ => return Err(format!("the {leg} leg records {at} out of place").into()),
        }
    }
    Err(format!("the {leg} leg has no end record, so it stopped early").into())
}

fn compare_records(
    native_dir: &Path,
    vm_dir: &Path,
) -> Result<(u64, String, Value), Box<dyn Error>> {
    let native = lines(&native_dir.join(RECORDS_FILE))?;
    let vm = lines(&vm_dir.join(RECORDS_FILE))?;
    let native_header = header(&native, "native")?;
    let vm_header = header(&vm, "vm")?;
    for field in ["game", "rom_sha256"] {
        let Some(value) = native_header.get(field) else {
            return Err(format!("the native header has no {field}").into());
        };
        if vm_header.get(field) != Some(value) {
            return Err(format!("the legs ran with another {field}").into());
        }
    }
    compare_boundaries(&native, &vm)?;
    let actions = check_leg(&native, "native")?;
    check_leg(&vm, "vm")?;
    let identities = json!({
        "native": {
            "emulator_identity": native_header.get("emulator_identity"),
            "executable_identity": native_header.get("executable_identity"),
        },
        "vm": {
            "emulator_identity": vm_header.get("emulator_identity"),
            "executable_identity": vm_header.get("executable_identity"),
        },
    });
    Ok((actions, sha256_after_header(&native), identities))
}

fn stream_header(lines: &[String], leg: &str) -> Result<Value, Box<dyn Error>> {
    let mut header = parse(
        lines
            .first()
            .ok_or_else(|| format!("the {leg} campaign stream is empty"))?,
        &format!("the {leg} campaign stream header"),
    )?;
    let object = header
        .as_object_mut()
        .ok_or_else(|| format!("the {leg} campaign stream header is not an object"))?;
    for field in BACKEND_HEADER_FIELDS {
        object
            .remove(field)
            .ok_or_else(|| format!("the {leg} campaign stream header has no {field}"))?;
    }
    Ok(header)
}

fn campaign_result(dir: &Path, leg: &str) -> Result<Value, Box<dyn Error>> {
    let path = dir.join("result.json");
    let mut result = parse(
        &fs::read_to_string(&path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?,
        &format!("the {leg} result.json"),
    )?;
    let object = result
        .as_object_mut()
        .ok_or_else(|| format!("the {leg} result.json is not an object"))?;
    object
        .remove("backend")
        .ok_or_else(|| format!("the {leg} result.json has no backend"))?;
    object
        .get_mut("witness")
        .and_then(Value::as_object_mut)
        .and_then(|witness| witness.remove("snapshot_sha256"))
        .ok_or_else(|| format!("the {leg} result.json has no witness snapshot_sha256"))?;
    Ok(result)
}

fn compare_campaigns(native_dir: &Path, vm_dir: &Path) -> Result<(u64, String), Box<dyn Error>> {
    let native = lines(&native_dir.join("stream.jsonl"))?;
    let vm = lines(&vm_dir.join("stream.jsonl"))?;
    let (native_header, vm_header) = (stream_header(&native, "native")?, stream_header(&vm, "vm")?);
    if native_header != vm_header {
        return Err(format!(
            "the campaign stream headers differ at {}",
            first_difference(&native_header, &vm_header, "")
        )
        .into());
    }
    for position in 1..native.len().max(vm.len()) {
        match (native.get(position), vm.get(position)) {
            (Some(left), Some(right)) if left == right => {}
            (Some(left), Some(right)) => {
                return Err(format!(
                    "the campaign streams differ at line {}: {}",
                    position + 1,
                    first_difference(
                        &parse(left, "a native stream line")?,
                        &parse(right, "a vm stream line")?,
                        ""
                    )
                )
                .into());
            }
            (Some(_), None) => {
                return Err(
                    format!("the vm campaign stream is missing line {}", position + 1).into(),
                );
            }
            (None, _) => {
                return Err(
                    format!("the vm campaign stream has an extra line {}", position + 1).into(),
                );
            }
        }
    }
    let end = native
        .last()
        .filter(|_| native.len() > 1)
        .map(|line| parse(line, "the last stream line"))
        .transpose()?
        .filter(|record| record["event"] == "end")
        .ok_or("the campaign streams do not end with the searcher's end record")?;
    let executions = end["executions_completed"]
        .as_u64()
        .ok_or("the campaign end record has no executions_completed")?;
    let witness = |dir: &Path| {
        let path = dir.join("witness-input.json");
        fs::read(&path).map_err(|error| format!("cannot read {}: {error}", path.display()))
    };
    if witness(native_dir)? != witness(vm_dir)? {
        return Err("the legs' witness-input.json differ".into());
    }
    let (native_result, vm_result) = (
        campaign_result(native_dir, "native")?,
        campaign_result(vm_dir, "vm")?,
    );
    if native_result != vm_result {
        return Err(format!(
            "the legs' result.json differ at {}",
            first_difference(&native_result, &vm_result, "")
        )
        .into());
    }
    Ok((executions, sha256_after_header(&native)))
}

fn compare(args: &[PathBuf]) -> Outcome {
    let [native, vm] = args else {
        return Err(USAGE.into());
    };
    let (actions, records_sha256, identities) = compare_records(native, vm)?;
    let (executions, stream_sha256) =
        compare_campaigns(&native.join(CAMPAIGN_DIR), &vm.join(CAMPAIGN_DIR))?;
    let summary = json!({
        "format": COMPARE_FORMAT,
        "actions": actions,
        "records_sha256": records_sha256,
        "executions": executions,
        "stream_sha256": stream_sha256,
        "identities": identities,
    });
    let written = native
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .join(SUMMARY_FILE);
    fs::write(&written, serde_json::to_vec_pretty(&summary)?)?;
    println!(
        "NES_BACKEND_ORACLE_OK actions={actions} sha256={records_sha256} executions={executions} stream_sha256={stream_sha256}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEGS: [&str; 2] = ["native", "vm"];
    const GOAL_SEQUENCE: u8 = 3;
    const GOAL_INDEX: u8 = 5;
    const JOBS: u64 = 3;

    fn observation(frame: u64) -> Value {
        json!({"frame_count": frame, "state": {"x": frame % 7}, "goal": false})
    }

    fn ran(frame: u64, outcome: Option<&str>) -> Value {
        json!({
            "exit_kind": "Ok",
            "actions": [{"cost": frame}],
            "observation": observation(frame),
            "outcome": outcome,
        })
    }

    fn records(leg: &str) -> Vec<Value> {
        let mut records = vec![
            json!({
                "format": LEG_FORMAT,
                "leg": leg,
                "game": "smb",
                "rom_sha256": "00",
                "plan": plan(),
                "emulator_identity": format!("{leg}-emulator"),
                "executable_identity": format!("{leg}-executable"),
            }),
            json!({"record": "genesis", "observation": observation(0)}),
        ];
        let mut actions = 0_u64;
        for sequence in 0..SEQUENCES {
            records.push(
                json!({"record": "sequence", "sequence": sequence, "observation": observation(0)}),
            );
            let mut frame = 0;
            for index in 0..CHORDS {
                let chord = planned(sequence, index);
                let before = observation(frame);
                frame += u64::from(chord.hold_frames);
                let outcome = (sequence == GOAL_SEQUENCE && index == GOAL_INDEX).then_some("goal");
                records.push(json!({
                    "record": "action",
                    "sequence": sequence,
                    "index": index,
                    "chord": chord,
                    "applied": ran(frame, outcome),
                    "restored": before,
                    "reapplied": ran(frame, outcome),
                }));
                actions += 1;
                if outcome.is_some() {
                    break;
                }
            }
        }
        records.push(json!({"record": "end", "sequences": SEQUENCES, "actions": actions}));
        records
    }

    fn stream(leg: &str) -> Vec<Value> {
        let mut stream = vec![json!({
            "format": "stream-format",
            "campaign_seed": 1,
            "emulator_backend": format!("{leg}-emulator"),
            "executable_identity": format!("{leg}-executable"),
            "workload_identity_sha256": "00",
        })];
        for id in 0..JOBS {
            stream.push(json!({"event": "job", "id": id, "result_sha256": format!("{id:064x}")}));
        }
        stream.push(json!({"event": "end", "executions_completed": JOBS}));
        stream
    }

    fn position(records: &[Value], sequence: u8, index: u8) -> usize {
        records
            .iter()
            .position(|record| {
                record["record"] == "action"
                    && record["sequence"] == sequence
                    && record["index"] == index
            })
            .unwrap()
    }

    fn sequence_position(records: &[Value], sequence: u8) -> usize {
        records
            .iter()
            .position(|record| record["record"] == "sequence" && record["sequence"] == sequence)
            .unwrap()
    }

    fn write_lines(path: &Path, values: &[Value]) {
        let mut text = String::new();
        for value in values {
            text.push_str(&serde_json::to_string(value).unwrap());
            text.push('\n');
        }
        fs::write(path, text).unwrap();
    }

    fn read_lines(path: &Path) -> Vec<Value> {
        fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    struct Legs {
        root: tempfile::TempDir,
    }

    impl Legs {
        fn new() -> Self {
            let legs = Self {
                root: tempfile::tempdir().unwrap(),
            };
            for leg in LEGS {
                let campaign = legs.dir(leg).join(CAMPAIGN_DIR);
                fs::create_dir_all(&campaign).unwrap();
                write_lines(&legs.dir(leg).join(RECORDS_FILE), &records(leg));
                write_lines(&campaign.join("stream.jsonl"), &stream(leg));
                fs::write(campaign.join("witness-input.json"), b"{\"actions\":[]}").unwrap();
                fs::write(
                    campaign.join("result.json"),
                    serde_json::to_vec(&json!({
                        "format": "result-format",
                        "backend": leg,
                        "solved": false,
                        "witness": {"victory": false, "snapshot_sha256": format!("{leg}-snapshot")},
                        "endpoint": {"x": 1},
                    }))
                    .unwrap(),
                )
                .unwrap();
            }
            legs
        }

        fn dir(&self, leg: &str) -> PathBuf {
            self.root.path().join(leg)
        }

        fn edit(&self, leg: &str, file: &str, edit: impl FnOnce(&mut Vec<Value>)) {
            let path = self.dir(leg).join(file);
            let mut values = read_lines(&path);
            edit(&mut values);
            write_lines(&path, &values);
        }

        fn edit_records(&self, leg: &str, edit: impl FnOnce(&mut Vec<Value>)) {
            self.edit(leg, RECORDS_FILE, edit);
        }

        fn edit_both(&self, edit: impl Fn(&mut Vec<Value>)) {
            for leg in LEGS {
                self.edit_records(leg, &edit);
            }
        }

        fn edit_stream(&self, leg: &str, edit: impl FnOnce(&mut Vec<Value>)) {
            self.edit(leg, &format!("{CAMPAIGN_DIR}/stream.jsonl"), edit);
        }

        fn edit_result(&self, leg: &str, edit: impl FnOnce(&mut Value)) {
            let path = self.dir(leg).join(CAMPAIGN_DIR).join("result.json");
            let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            edit(&mut value);
            fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        }

        fn compare(&self) -> Result<(), String> {
            compare(&[self.dir("native"), self.dir("vm")]).map_err(|error| error.to_string())
        }

        fn refusal(&self) -> String {
            self.compare().unwrap_err()
        }
    }

    #[test]
    fn agreeing_legs_pass_and_write_the_summary() {
        let legs = Legs::new();
        legs.compare().unwrap();
        let summary: Value =
            serde_json::from_slice(&fs::read(legs.root.path().join(SUMMARY_FILE)).unwrap())
                .unwrap();
        let native = lines(&legs.dir("native").join(RECORDS_FILE)).unwrap();
        let vm = lines(&legs.dir("vm").join(RECORDS_FILE)).unwrap();
        assert_eq!(sha256_after_header(&native), sha256_after_header(&vm));
        assert_eq!(summary["format"], COMPARE_FORMAT);
        assert_eq!(
            summary["actions"],
            u64::from(SEQUENCES - 1) * u64::from(CHORDS) + u64::from(GOAL_INDEX) + 1
        );
        assert_eq!(summary["records_sha256"], sha256_after_header(&native));
        assert_eq!(summary["executions"], JOBS);
        assert_eq!(
            summary["identities"]["vm"]["emulator_identity"],
            "vm-emulator"
        );
        assert_eq!(
            summary["identities"]["native"]["executable_identity"],
            "native-executable"
        );
    }

    #[test]
    fn a_differing_genesis_fails_at_the_genesis_record() {
        let legs = Legs::new();
        legs.edit_records("vm", |records| {
            records[1]["observation"]["frame_count"] = json!(1);
        });
        assert_eq!(
            legs.refusal(),
            "the legs differ at the genesis record: observation.frame_count"
        );
    }

    #[test]
    fn a_differing_action_names_its_sequence_index_and_field() {
        let legs = Legs::new();
        legs.edit_records("vm", |records| {
            let at = position(records, 2, 9);
            records[at]["applied"]["observation"]["state"]["x"] = json!(99);
        });
        assert_eq!(
            legs.refusal(),
            "the legs differ at sequence 2 action 9: applied.observation.state.x"
        );
    }

    #[test]
    fn a_vm_leg_without_its_last_action_and_end_record_is_missing_that_boundary() {
        let legs = Legs::new();
        legs.edit_records("vm", |records| {
            records.truncate(records.len() - 2);
        });
        assert_eq!(
            legs.refusal(),
            format!(
                "the vm leg is missing sequence {} action {}",
                SEQUENCES - 1,
                CHORDS - 1
            )
        );
    }

    #[test]
    fn a_native_leg_that_runs_out_is_missing_the_vm_boundary() {
        let legs = Legs::new();
        legs.edit_records("native", |records| {
            records.truncate(records.len() - 1);
        });
        assert_eq!(legs.refusal(), "the native leg is missing the end record");
    }

    #[test]
    fn a_duplicated_vm_action_is_an_extra_boundary() {
        let legs = Legs::new();
        legs.edit_records("vm", |records| {
            let at = position(records, 2, 4);
            records.insert(at, records[at].clone());
        });
        assert_eq!(
            legs.refusal(),
            "the vm leg has an extra boundary at sequence 2 action 4"
        );
    }

    #[test]
    fn a_vm_action_where_the_native_leg_ends_is_an_extra_boundary() {
        let legs = Legs::new();
        legs.edit_records("vm", |records| {
            let at = records.len() - 1;
            records.insert(at, records[at - 1].clone());
        });
        assert_eq!(
            legs.refusal(),
            format!(
                "the vm leg has an extra boundary at sequence {} action {}",
                SEQUENCES - 1,
                CHORDS - 1
            )
        );
    }

    #[test]
    fn a_vm_leg_that_skips_an_action_is_missing_it() {
        let legs = Legs::new();
        legs.edit_records("vm", |records| {
            let at = position(records, 5, 7);
            records.remove(at);
        });
        assert_eq!(legs.refusal(), "the vm leg is missing sequence 5 action 7");
    }

    #[test]
    fn the_headers_must_name_the_format_the_leg_and_the_plan() {
        let legs = Legs::new();
        legs.edit_records("vm", |records| records[0]["leg"] = json!("native"));
        assert_eq!(
            legs.refusal(),
            "the vm directory holds another leg's records"
        );
        let legs = Legs::new();
        legs.edit_records("native", |records| records[0]["format"] = json!("other"));
        assert_eq!(
            legs.refusal(),
            format!("the native records are not {LEG_FORMAT}")
        );
        let legs = Legs::new();
        legs.edit_records("vm", |records| records[0]["plan"]["chords"] = json!(4));
        assert_eq!(legs.refusal(), "the vm leg ran another plan");
    }

    #[test]
    fn the_legs_must_run_the_same_game_and_rom() {
        let legs = Legs::new();
        legs.edit_records("vm", |records| records[0]["rom_sha256"] = json!("01"));
        assert_eq!(legs.refusal(), "the legs ran with another rom_sha256");
        let legs = Legs::new();
        legs.edit_both(|records| {
            records[0].as_object_mut().unwrap().remove("game");
        });
        assert_eq!(legs.refusal(), "the native header has no game");
    }

    #[test]
    fn legs_that_both_stopped_early_fail() {
        let legs = Legs::new();
        legs.edit_both(|records| {
            records.pop();
        });
        assert_eq!(
            legs.refusal(),
            "the native leg has no end record, so it stopped early"
        );
    }

    #[test]
    fn an_end_record_must_count_the_records_it_follows() {
        let legs = Legs::new();
        legs.edit_both(|records| {
            let end = records.len() - 1;
            records[end]["actions"] = json!(1);
        });
        assert_eq!(
            legs.refusal(),
            "the native leg's end record does not count the records it follows"
        );
    }

    #[test]
    fn a_chord_must_be_the_plans_chord() {
        let legs = Legs::new();
        legs.edit_both(|records| {
            let at = position(records, 0, 3);
            records[at]["chord"]["buttons"] = json!(0xff);
        });
        assert_eq!(
            legs.refusal(),
            "the native leg's chord at sequence 0 action 3 is not the plan's"
        );
    }

    #[test]
    fn every_chord_must_exit_ok() {
        let legs = Legs::new();
        legs.edit_both(|records| {
            let at = position(records, 6, 0);
            records[at]["reapplied"]["exit_kind"] = json!("Crash");
        });
        assert_eq!(
            legs.refusal(),
            "the native leg's reapplied chord at sequence 6 action 0 did not exit Ok"
        );
    }

    #[test]
    fn a_reapplied_chord_must_equal_its_first_run() {
        let legs = Legs::new();
        legs.edit_both(|records| {
            let at = position(records, 1, 2);
            records[at]["reapplied"]["observation"]["frame_count"] = json!(0);
        });
        assert_eq!(
            legs.refusal(),
            "the native leg's reapplied chord at sequence 1 action 2 differs from its first run: observation.frame_count"
        );
    }

    #[test]
    fn a_restore_must_return_to_the_observation_before_the_chord() {
        let legs = Legs::new();
        legs.edit_both(|records| {
            let at = position(records, 1, 2);
            records[at]["restored"]["state"]["x"] = json!(99);
        });
        assert_eq!(
            legs.refusal(),
            "the native leg's restore before sequence 1 action 2 did not return to the observation before the chord"
        );
    }

    #[test]
    fn a_restored_genesis_must_equal_the_genesis() {
        let legs = Legs::new();
        legs.edit_both(|records| {
            let at = sequence_position(records, 4);
            records[at]["observation"]["frame_count"] = json!(1);
        });
        assert_eq!(
            legs.refusal(),
            "the native leg's restored genesis at sequence 4 differs from its genesis"
        );
    }

    #[test]
    fn a_sequence_must_run_every_chord_unless_it_reaches_an_outcome() {
        let legs = Legs::new();
        legs.edit_both(|records| {
            let at = position(records, 5, CHORDS - 1);
            records.remove(at);
            let end = records.len() - 1;
            records[end]["actions"] = json!(records[end]["actions"].as_u64().unwrap() - 1);
        });
        assert_eq!(
            legs.refusal(),
            format!(
                "the native leg's sequence 5 ends after {} of {CHORDS} chords without a terminal outcome",
                CHORDS - 1
            )
        );
    }

    #[test]
    fn no_chord_may_follow_an_outcome() {
        let legs = Legs::new();
        legs.edit_both(|records| {
            let at = position(records, GOAL_SEQUENCE, GOAL_INDEX);
            let mut next = records[at].clone();
            next["index"] = json!(GOAL_INDEX + 1);
            records.insert(at + 1, next);
        });
        assert_eq!(
            legs.refusal(),
            format!(
                "the native leg records sequence {GOAL_SEQUENCE} action {} out of place",
                GOAL_INDEX + 1
            )
        );
    }

    #[test]
    fn a_leg_must_run_every_sequence() {
        let legs = Legs::new();
        legs.edit_both(|records| {
            let at = sequence_position(records, SEQUENCES - 1);
            let end = records.pop().unwrap();
            records.truncate(at);
            records.push(end);
        });
        assert_eq!(
            legs.refusal(),
            format!(
                "the native leg ran {} of the plan's {SEQUENCES} sequences",
                SEQUENCES - 1
            )
        );
    }

    #[test]
    fn a_changed_job_result_fails_at_that_stream_line() {
        let legs = Legs::new();
        legs.edit_stream("vm", |stream| {
            stream[2]["result_sha256"] = json!(format!("{:064x}", 7));
        });
        assert_eq!(
            legs.refusal(),
            "the campaign streams differ at line 3: result_sha256"
        );
    }

    #[test]
    fn a_vm_stream_without_its_last_line_is_missing_that_line() {
        let legs = Legs::new();
        legs.edit_stream("vm", |stream| {
            stream.pop();
        });
        assert_eq!(
            legs.refusal(),
            format!("the vm campaign stream is missing line {}", JOBS + 2)
        );
    }

    #[test]
    fn a_vm_stream_with_another_line_has_an_extra_line() {
        let legs = Legs::new();
        legs.edit_stream("vm", |stream| {
            let end = stream.last().unwrap().clone();
            stream.push(end);
        });
        assert_eq!(
            legs.refusal(),
            format!("the vm campaign stream has an extra line {}", JOBS + 3)
        );
    }

    #[test]
    fn streams_must_end_with_the_searchers_end_record() {
        let legs = Legs::new();
        for leg in LEGS {
            legs.edit_stream(leg, |stream| {
                stream.pop();
            });
        }
        assert_eq!(
            legs.refusal(),
            "the campaign streams do not end with the searcher's end record"
        );
    }

    #[test]
    fn stream_headers_may_differ_only_in_the_backend_fields() {
        let legs = Legs::new();
        legs.edit_stream("vm", |stream| stream[0]["campaign_seed"] = json!(2));
        assert_eq!(
            legs.refusal(),
            "the campaign stream headers differ at campaign_seed"
        );
        let legs = Legs::new();
        legs.edit_stream("vm", |stream| {
            stream[0]
                .as_object_mut()
                .unwrap()
                .remove("executable_identity");
        });
        assert_eq!(
            legs.refusal(),
            "the vm campaign stream header has no executable_identity"
        );
    }

    #[test]
    fn witness_inputs_must_be_byte_equal() {
        let legs = Legs::new();
        fs::write(
            legs.dir("vm").join(CAMPAIGN_DIR).join("witness-input.json"),
            b"{\"actions\": []}",
        )
        .unwrap();
        assert_eq!(legs.refusal(), "the legs' witness-input.json differ");
    }

    #[test]
    fn results_may_differ_only_in_the_backend_and_the_witness_snapshot() {
        let legs = Legs::new();
        legs.edit_result("vm", |result| result["endpoint"]["x"] = json!(2));
        assert_eq!(legs.refusal(), "the legs' result.json differ at endpoint.x");
        let legs = Legs::new();
        legs.edit_result("vm", |result| {
            result["witness"]
                .as_object_mut()
                .unwrap()
                .remove("snapshot_sha256");
        });
        assert_eq!(
            legs.refusal(),
            "the vm result.json has no witness snapshot_sha256"
        );
    }
}
