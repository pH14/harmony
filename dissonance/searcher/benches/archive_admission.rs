// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{cmp::Ordering, hint::black_box};

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use searcher::search::archive::{
    Archive, ArchiveCandidate, ArchiveEntryReport, ArchiveKey, Input, entries_by_suffix,
};
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
) -> ArchiveCandidate<Vec<u64>, Key<P>, ()> {
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

#[derive(Serialize)]
struct Reports {
    #[serde(serialize_with = "entries_by_suffix::serialize")]
    entries: Vec<ArchiveEntryReport<u64, Key, Vec<u8>>>,
}

fn reporting(c: &mut Criterion) {
    for (name, length, chain) in [
        ("roots_short", 8, false),
        ("roots_long", 128, false),
        ("chain", 128, true),
    ] {
        let reports = Reports {
            entries: (0..512_u64)
                .map(|id| ArchiveEntryReport {
                    id,
                    parent_id: (chain && id % 128 != 0).then(|| id - 1),
                    created_execution: id,
                    input: Input {
                        actions: vec![7; length + if chain { id as usize % 128 } else { 0 }],
                    },
                    key: Key([id; 4]),
                    milestones: vec![1; 16],
                    selector: None,
                })
                .collect(),
        };
        c.bench_function(&format!("archive_report_{name}_512"), |b| {
            b.iter(|| black_box(serde_json::to_vec(black_box(&reports)).unwrap()));
        });
    }
}

fn pending_suffix_admission(c: &mut Criterion) {
    for length in [1, 128, 4096] {
        let mut archive = populated::<4>();
        let pending_suffix = vec![99; length];
        c.bench_function(&format!("pending_suffix_rejected_{length}"), |b| {
            b.iter(|| {
                black_box(
                    archive
                        .insert_after(
                            None,
                            None,
                            1,
                            ArchiveCandidate {
                                suffix: black_box(pending_suffix.as_slice()),
                                key: Key([0; 4]),
                                milestones: (),
                            },
                            (),
                        )
                        .unwrap(),
                );
            });
        });
    }
}

#[derive(Clone, Copy, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
struct LineageKey<const N: usize>(u64);

impl<const N: usize> ArchiveKey for LineageKey<N> {
    type Place = ();
    type Progress = ();
    type Identity = ();
    type Lineage = Vec<[u8; 3]>;

    fn place(self) {}
    fn progress(self) {}
    fn identity(self) {}
    fn capacity() -> usize {
        1
    }
    fn preference_cmp(self, _preference: usize, other: Self) -> Ordering {
        self.0.cmp(&other.0)
    }
    fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
        self
    }
    fn record(lineage: &mut Self::Lineage, _key: Self) {
        if lineage.is_empty() {
            lineage.extend((0..N).map(|index| [0, (index / 256) as u8, index as u8]));
        }
    }
}

fn rejected_lineage<const N: usize>(c: &mut Criterion) {
    let mut archive = Archive::<u64, LineageKey<N>, (), ()>::new(|_| 1);
    let parent = archive
        .insert(
            None,
            0,
            ArchiveCandidate {
                suffix: vec![1],
                key: LineageKey(10),
                milestones: (),
            },
            (),
        )
        .unwrap()
        .unwrap();
    c.bench_function(&format!("rejected_lineage_{N}"), |b| {
        b.iter(|| {
            black_box(
                archive
                    .insert(
                        Some(parent),
                        1,
                        ArchiveCandidate {
                            suffix: [2].as_slice(),
                            key: LineageKey(0),
                            milestones: (),
                        },
                        (),
                    )
                    .unwrap(),
            )
        });
    });
}

fn lineage_admission(c: &mut Criterion) {
    rejected_lineage::<0>(c);
    rejected_lineage::<16>(c);
    rejected_lineage::<128>(c);
    rejected_lineage::<4096>(c);
}

criterion_group!(
    benches,
    admission,
    reporting,
    pending_suffix_admission,
    lineage_admission
);
criterion_main!(benches);
