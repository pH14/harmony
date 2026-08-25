// SPDX-License-Identifier: AGPL-3.0-or-later

use criterion::{Criterion, criterion_group, criterion_main};
use fuzzer::search::empirical_steps::{EmpiricalStepParameters, EmpiricalStepTables};

const C120_APPROXIMATE_STEPS: usize = 1_690_000;

fn cached_checkpoint(c: &mut Criterion) {
    let parameters = EmpiricalStepParameters {
        prefix_steps: 0,
        recent_successes: 1,
        recent_weight: 1,
        all_history_weight: 1,
        update_every_records: 64,
        hash_every_records: 64,
    };
    let mut tables = EmpiricalStepTables::new(parameters).expect("valid benchmark parameters");
    let sequence = (0..C120_APPROXIMATE_STEPS)
        .map(|index| u16::try_from(index % 1024).expect("bounded benchmark step"))
        .collect::<Vec<_>>();
    tables
        .fold_retained(&sequence)
        .expect("fold benchmark sequence");
    tables.flush().expect("make benchmark table visible");

    c.bench_function("cached_checkpoint_1_690_000_steps", |b| {
        b.iter(|| tables.checkpoint().expect("cached checkpoint"));
    });
}

criterion_group!(benches, cached_checkpoint);
criterion_main!(benches);
