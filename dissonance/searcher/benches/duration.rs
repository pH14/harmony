// SPDX-License-Identifier: AGPL-3.0-or-later

use criterion::{Criterion, criterion_group, criterion_main};
use searcher::search::duration::DurationPolicies;
use std::{hint::black_box, num::NonZeroU64};

fn duration_observation(c: &mut Criterion) {
    let one = NonZeroU64::new(1).unwrap();
    for contexts in [1_u16, 256] {
        let mut policies = DurationPolicies::new();
        for context in 0..contexts {
            policies.observe(context, one, true, one).unwrap();
        }
        let mut next = 0_u16;
        c.bench_function(&format!("duration_observe_{contexts}_contexts"), |b| {
            b.iter(|| {
                let context = next % contexts;
                next = next.wrapping_add(1);
                black_box(&mut policies)
                    .observe(black_box(context), one, true, one)
                    .unwrap();
            });
        });
    }
}

criterion_group!(benches, duration_observation);
criterion_main!(benches);
