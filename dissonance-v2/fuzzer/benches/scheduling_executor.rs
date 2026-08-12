// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{env, fs, path::PathBuf};

use criterion::{Criterion, criterion_group, criterion_main};
use fuzzer::phase4b::{SmbExecutorMode, run_smb_ratchet_with_executor};

const SEED: u64 = 0x5eed_ee01;
const EXECUTION_BUDGET: u64 = 50;

fn scheduling_executor(c: &mut Criterion) {
    let Some(rom_path) = env::var_os("HARMONY_SMB_ROM").map(PathBuf::from) else {
        eprintln!("skipping SMB executor benchmark: HARMONY_SMB_ROM is unset");
        return;
    };
    let rom = fs::read(rom_path).expect("read SMB ROM");
    let mut group = c.benchmark_group("smb_scheduling_executor");
    group.sample_size(10);
    group.bench_function("legacy", |b| {
        b.iter(|| {
            run_smb_ratchet_with_executor(&rom, SEED, EXECUTION_BUDGET, SmbExecutorMode::Legacy)
                .expect("legacy benchmark campaign")
        });
    });
    group.bench_function("snapshot_resume", |b| {
        b.iter(|| {
            run_smb_ratchet_with_executor(
                &rom,
                SEED,
                EXECUTION_BUDGET,
                SmbExecutorMode::SnapshotResume,
            )
            .expect("snapshot-resume benchmark campaign")
        });
    });
    group.finish();
}

criterion_group!(benches, scheduling_executor);
criterion_main!(benches);
