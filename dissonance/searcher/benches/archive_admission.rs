// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{cmp::Ordering, hint::black_box};

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use searcher::search::archive::{Archive, ArchiveCandidate, ArchiveKey};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
struct Key<const P: usize = 4>([u64; 4]);

impl<const P: usize> ArchiveKey for Key<P> {
    type Place = ();
    type Progress = ();
    type Identity = ();
    type Lineage = ();

    fn place(self) {}
    fn progress(self) {}
    fn identity(self) {}
    fn preferences() -> usize {
        P
    }
    fn preference_cmp(self, preference: usize, other: Self) -> Ordering {
        if P == 0 {
            Ordering::Equal
        } else {
            self.0[preference].cmp(&other.0[preference])
        }
    }
    fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
        self
    }
    fn record(_lineage: &mut Self::Lineage, _key: Self) {}
}

type BenchArchive<const P: usize = 4> = Archive<u64, Key<P>, (), ()>;

fn candidate<const P: usize>(
    action: u64,
    key: Key<P>,
    length: usize,
) -> ArchiveCandidate<u64, Key<P>, ()> {
    ArchiveCandidate {
        suffix: vec![action; length],
        key,
        milestones: (),
    }
}

fn populated<const P: usize>() -> BenchArchive<P> {
    let mut archive = BenchArchive::<P>::new(|_| 1);
    for preference in 0..P.max(1) {
        for rank in 0..2 {
            let mut scores = [0; 4];
            scores[preference] = 10 + rank;
            archive
                .insert(
                    None,
                    0,
                    candidate(1 + preference as u64 * 2 + rank, Key(scores), 1),
                    (),
                )
                .unwrap();
        }
    }
    archive
}

fn rejected<const P: usize>(c: &mut Criterion) {
    let mut archive = populated::<P>();
    c.bench_function(&format!("archive_rejected_{P}_preferences"), |b| {
        b.iter(|| {
            black_box(
                archive
                    .insert(None, 1, candidate(99, Key([0; 4]), 1), ())
                    .unwrap(),
            )
        });
    });
}

fn admission(c: &mut Criterion) {
    rejected::<0>(c);
    rejected::<1>(c);
    let mut archive = populated::<4>();
    c.bench_function("archive_rejected_four_preferences", |b| {
        b.iter(|| {
            black_box(
                archive
                    .insert(None, 1, candidate(99, Key([0; 4]), 1), ())
                    .unwrap(),
            )
        });
    });
    for length in [1, 128] {
        c.bench_function(
            &format!("archive_accepted_four_preferences_suffix_{length}"),
            |b| {
                b.iter_batched(
                    || (populated::<4>(), candidate(99, Key([20; 4]), length)),
                    |(mut archive, candidate)| {
                        black_box(archive.insert(None, 1, candidate, ()).unwrap());
                        black_box(archive)
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
}

criterion_group!(benches, admission);
criterion_main!(benches);
