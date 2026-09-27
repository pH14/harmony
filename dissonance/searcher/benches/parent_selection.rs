// SPDX-License-Identifier: AGPL-3.0-or-later

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use searcher::search::{
    archive::{Archive, ArchiveCandidate, ArchiveKey},
    rand::RomuDuoJrRand,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
struct Key<const SHIFT: u32>(u64);

impl<const SHIFT: u32> ArchiveKey for Key<SHIFT> {
    type Place = ();
    type Progress = u64;
    type Identity = ();
    type Lineage = ();
    fn place(self) {}
    fn progress(self) -> u64 {
        self.0
    }
    fn identity(self) {}
    fn tier_rank_shift() -> u32 {
        SHIFT
    }
    fn complete(self, _: Option<(Self, &Self::Lineage)>) -> Self {
        self
    }
    fn record(_: &mut Self::Lineage, _: Self) {}
}

fn selection<const SHIFT: u32>(c: &mut Criterion, tiers: u64) {
    let mut archive = Archive::<u64, Key<SHIFT>, (), ()>::new(|_| 1);
    for tier in 0..tiers {
        archive
            .insert(
                None,
                tier,
                ArchiveCandidate {
                    suffix: vec![tier],
                    key: Key(tier),
                    milestones: (),
                },
                (),
            )
            .unwrap();
    }
    let mut rand = RomuDuoJrRand::with_seed(0x1234);
    archive.select_parent(&mut rand).unwrap();
    c.bench_function(
        &format!("parent_selection_{tiers}_tiers_shift_{SHIFT}"),
        |b| {
            b.iter(|| black_box(archive.select_parent(black_box(&mut rand)).unwrap()));
        },
    );
}

fn parents(c: &mut Criterion) {
    for tiers in [1, 16, 256, 4096] {
        selection::<3>(c, tiers);
        selection::<1>(c, tiers);
        selection::<0>(c, tiers);
    }
}

criterion_group!(benches, parents);
criterion_main!(benches);
