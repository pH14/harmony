// SPDX-License-Identifier: AGPL-3.0-or-later

//! One-shot M5 source/uninterrupted and imported/continued NES driver.

#[cfg(unix)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::{env, fs, path::PathBuf};

    use machine::{
        Machine, Moment, SnapId, StopConditions, StopMask, StopReason,
        control::{SnapshotCut, SocketMachine},
        nes,
    };
    use searcher::smb::{
        campaign::SmbButtonVocabulary, remote::RemoteSmbTarget, target::ButtonChord,
    };
    use serde::{Deserialize, Serialize};

    const FORMAT: &str = "smb-m5-portability-v1";

    #[derive(Clone, Debug, Deserialize, Serialize)]
    struct CutReport {
        at: u64,
        frame: u64,
        sdk_events: u64,
        tainted: bool,
    }

    impl From<SnapshotCut> for CutReport {
        fn from(cut: SnapshotCut) -> Self {
            Self {
                at: cut.at.0,
                frame: cut.frame.0,
                sdk_events: cut.sdk_events,
                tainted: cut.tainted,
            }
        }
    }

    impl From<&CutReport> for SnapshotCut {
        fn from(cut: &CutReport) -> Self {
            Self {
                at: Moment(cut.at),
                frame: Moment(cut.frame),
                sdk_events: cut.sdk_events,
                tainted: cut.tainted,
            }
        }
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    struct Report {
        format: String,
        role: String,
        seed: u64,
        actions: [ButtonChord; 3],
        cut: CutReport,
        state_hashes: [[u8; 32]; 3],
        frame_boundaries: [u64; 3],
    }

    fn actions(seed: u64) -> [ButtonChord; 3] {
        let masks = SmbButtonVocabulary::NesDownTen.masks();
        let mut word = seed;
        std::array::from_fn(|index| {
            // Fixed integer mixing only; the seed fully determines the three
            // held-out vocabulary actions and no game route enters the driver.
            word ^= word << 13;
            word ^= word >> 7;
            word ^= word << 17;
            let mask = masks[(word as usize).wrapping_add(index) % masks.len()];
            let hold = 2 + ((word >> 8) % 11) as u8;
            ButtonChord::new(mask, hold)
        })
    }

    fn run_point<M: Machine>(machine: &mut M) -> Result<(), Box<dyn std::error::Error>> {
        let stop = machine.run(
            StopConditions {
                deadline: None,
                on: StopMask::NONE.arm(machine::class_bit::SNAPSHOT_POINT),
            },
            None,
        )?;
        if !matches!(stop, StopReason::SnapshotPoint { .. }) {
            return Err(format!("guest did not reach the next chord boundary: {stop:?}").into());
        }
        Ok(())
    }

    fn source(
        socket: PathBuf,
        seed: u64,
        report_path: PathBuf,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let target = RemoteSmbTarget::from_machine(SocketMachine::connect(socket)?)?;
        let (mut machine, genesis) = target.into_genesis_machine();
        let actions = actions(seed);
        machine.branch(genesis, &nes::reproducer(&actions))?;

        run_point(&mut machine)?;
        let midpoint = machine.snapshot()?;
        let cut = machine
            .snapshot_cut(midpoint)
            .ok_or("midpoint snapshot did not retain its evidence cut")?;
        let first_hash = machine.state_hash()?;
        let first_frame = machine.logical_frame().0;
        run_point(&mut machine)?;
        let second_hash = machine.state_hash()?;
        let second_frame = machine.logical_frame().0;
        run_point(&mut machine)?;
        let third_hash = machine.state_hash()?;
        let third_frame = machine.logical_frame().0;

        let report = Report {
            format: FORMAT.to_string(),
            role: "source-uninterrupted".to_string(),
            seed,
            actions,
            cut: cut.into(),
            state_hashes: [first_hash, second_hash, third_hash],
            frame_boundaries: [first_frame, second_frame, third_frame],
        };
        fs::write(&report_path, serde_json::to_vec_pretty(&report)?)?;
        println!(
            "M5_PORTABILITY_SOURCE_OK snapshot={} cut_frame={} continuations=2 report={}",
            midpoint.0,
            cut.frame.0,
            report_path.display()
        );
        Ok(())
    }

    fn restore(
        socket: PathBuf,
        source_path: PathBuf,
        report_path: PathBuf,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let source: Report = serde_json::from_slice(&fs::read(&source_path)?)?;
        if source.format != FORMAT || source.role != "source-uninterrupted" {
            return Err("source report has the wrong format or role".into());
        }
        let imported = SnapId(1);
        let mut machine = SocketMachine::connect(socket)?;
        machine.register_imported_snapshot(imported, SnapshotCut::from(&source.cut))?;
        machine.replay(imported)?;
        let first_hash = machine.state_hash()?;
        let first_frame = machine.logical_frame().0;
        run_point(&mut machine)?;
        let second_hash = machine.state_hash()?;
        let second_frame = machine.logical_frame().0;
        run_point(&mut machine)?;
        let third_hash = machine.state_hash()?;
        let third_frame = machine.logical_frame().0;
        let hashes = [first_hash, second_hash, third_hash];
        let frames = [first_frame, second_frame, third_frame];
        if hashes != source.state_hashes {
            let index = hashes
                .iter()
                .zip(source.state_hashes.iter())
                .position(|(actual, expected)| actual != expected)
                .unwrap_or(0);
            return Err(
                format!("portable continuation state hash diverged at boundary {index}").into(),
            );
        }
        if frames != source.frame_boundaries {
            return Err(format!(
                "portable continuation frame sequence differs: source {:?}, restored {:?}",
                source.frame_boundaries, frames
            )
            .into());
        }

        let report = Report {
            format: FORMAT.to_string(),
            role: "destination-restored".to_string(),
            seed: source.seed,
            actions: source.actions,
            cut: source.cut,
            state_hashes: hashes,
            frame_boundaries: frames,
        };
        fs::write(&report_path, serde_json::to_vec_pretty(&report)?)?;
        println!(
            "M5_PORTABILITY_RESTORE_OK imported={} boundaries=3 report={} source={}",
            imported.0,
            report_path.display(),
            source_path.display()
        );
        Ok(())
    }

    let mut args = env::args_os().skip(1);
    let mode = args
        .next()
        .ok_or("usage: smb-m5-portability <source|restore> ...")?;
    match mode.to_string_lossy().as_ref() {
        "source" => {
            let socket = PathBuf::from(args.next().ok_or("source requires <socket>")?);
            let seed = args
                .next()
                .ok_or("source requires <seed>")?
                .to_string_lossy()
                .parse::<u64>()?;
            let report = PathBuf::from(args.next().ok_or("source requires <report>")?);
            if args.next().is_some() {
                return Err("unexpected source argument".into());
            }
            source(socket, seed, report)
        }
        "restore" => {
            let socket = PathBuf::from(args.next().ok_or("restore requires <socket>")?);
            let source_report =
                PathBuf::from(args.next().ok_or("restore requires <source-report>")?);
            let report = PathBuf::from(args.next().ok_or("restore requires <report>")?);
            if args.next().is_some() {
                return Err("unexpected restore argument".into());
            }
            restore(socket, source_report, report)
        }
        _ => Err("mode must be source or restore".into()),
    }
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("smb-m5-portability requires a Unix control socket");
    std::process::ExitCode::from(2)
}
