// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::heap;

const DISCOVERY_REFRESH_JOBS: u64 = 1_024;

#[derive(Default)]
pub(crate) struct CoverageCounts {
    jobs: u64,
    job: u64,
    job_counted: bool,
    job_moved: bool,
    ids: BTreeMap<u64, IdCount>,
    report: CoverageReport,
    discoveries: Option<DiscoveryLevels>,
}

struct DiscoveryLevels {
    cap_log2: u8,
    levels: BTreeMap<u32, DiscoveryLevel>,
    watched: BTreeMap<u64, Vec<u32>>,
    risen: BTreeSet<u32>,
    changed: Vec<(u32, u8)>,
}

struct DiscoveryLevel {
    ids: Vec<u64>,
    hits: u64,
    level: u8,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct DiscoveryWeightReport {
    pub cap_log2: u8,
    pub discoveries: u64,
    pub cells: u64,
    pub discoveries_by_log2_weight: Vec<u64>,
    pub log2_weight_by_discovery: BTreeMap<u32, u8>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub frontier_draws_by_discovery: BTreeMap<u32, u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CoverageRating {
    pub rarity: u8,
    pub id: u64,
    pub discovery: Option<u32>,
}

#[derive(Clone, Copy)]
struct IdCount {
    jobs: u64,
    last_job: u64,
    discovery: u32,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CoverageReport {
    pub jobs_with_ids: u64,
    pub distinct_ids: u64,
    pub actions_with_ids: u64,
    pub action_ids: u64,
    pub candidates_rated: u64,
    pub candidates_moved: u64,
    pub jobs_moved: u64,
    pub candidates_by_rarity: Vec<u64>,
    pub moved_by_rarest_id: BTreeMap<u64, u64>,
    pub discovering_actions: u64,
    pub discovering_candidates_by_log2_job: Vec<u64>,
    pub memory_bytes: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery_weight: Option<DiscoveryWeightReport>,
}

impl CoverageCounts {
    pub(crate) fn is_active(&self) -> bool {
        self.jobs > 0
    }

    pub(crate) fn weigh_discoveries(&mut self, cap_log2: u8) {
        self.discoveries = Some(DiscoveryLevels {
            cap_log2,
            levels: BTreeMap::new(),
            watched: BTreeMap::new(),
            risen: BTreeSet::new(),
            changed: Vec::new(),
        });
    }

    pub(crate) fn weighs_discoveries(&self) -> bool {
        self.discoveries.is_some()
    }

    pub(crate) fn first_hit_ids(&self, ids: &[u64]) -> Vec<u64> {
        let mut first = ids
            .iter()
            .copied()
            .filter(|id| {
                self.ids
                    .get(id)
                    .is_some_and(|count| count.jobs == 1 && count.last_job == self.job)
            })
            .collect::<Vec<_>>();
        first.sort_unstable();
        first.dedup();
        first
    }

    pub(crate) fn watch_discovery(&mut self, discovery: u32, ids: Vec<u64>) -> Option<u8> {
        let jobs = self.jobs;
        let counts = &self.ids;
        let weights = self.discoveries.as_mut()?;
        if let Some(known) = weights.levels.get(&discovery) {
            return Some(known.level);
        }
        let hits = fewest_hits(counts, &ids);
        let level = rarity(jobs, hits).min(weights.cap_log2);
        for id in &ids {
            weights.watched.entry(*id).or_default().push(discovery);
        }
        weights
            .levels
            .insert(discovery, DiscoveryLevel { ids, hits, level });
        Some(level)
    }

    pub(crate) fn take_changed_levels(&mut self) -> Vec<(u32, u8)> {
        self.discoveries
            .as_mut()
            .map(|weights| std::mem::take(&mut weights.changed))
            .unwrap_or_default()
    }

    pub(crate) fn begin_job(&mut self, job: u64) {
        self.job = job;
        self.job_counted = false;
        self.job_moved = false;
    }

    pub(crate) fn rate(&mut self, ids: &[u64]) -> Option<CoverageRating> {
        if ids.is_empty() {
            return None;
        }
        if !self.job_counted {
            self.job_counted = true;
            self.jobs = self.jobs.saturating_add(1);
        }
        let job = self.job;
        let mut rarest = (u64::MAX, 0, 0);
        for &id in ids {
            let discovered = u32::try_from(self.ids.len()).unwrap_or(u32::MAX - 1) + 1;
            let count = self.ids.entry(id).or_insert(IdCount {
                jobs: 0,
                last_job: 0,
                discovery: discovered,
            });
            if count.jobs == 0 || count.last_job != job {
                count.jobs = count.jobs.saturating_add(1);
                count.last_job = job;
                if let Some(weights) = self.discoveries.as_mut()
                    && let Some(discoveries) = weights.watched.get(&id)
                {
                    weights.risen.extend(discoveries.iter().copied());
                }
            }
            if count.jobs < rarest.0 {
                rarest = (count.jobs, id, count.discovery);
            }
        }
        self.report.actions_with_ids = self.report.actions_with_ids.saturating_add(1);
        self.report.action_ids = self
            .report
            .action_ids
            .saturating_add(u64::try_from(ids.len()).unwrap_or(u64::MAX));
        let discovery = (rarest.0 == 1).then_some(rarest.2);
        if discovery.is_some() {
            self.report.discovering_actions = self.report.discovering_actions.saturating_add(1);
        }
        Some(CoverageRating {
            rarity: rarity(self.jobs, rarest.0),
            id: rarest.1,
            discovery,
        })
    }

    pub(crate) fn record_candidate(&mut self, rating: CoverageRating, moved: bool) {
        let index = usize::from(rating.rarity);
        if self.report.candidates_by_rarity.len() <= index {
            self.report.candidates_by_rarity.resize(index + 1, 0);
        }
        self.report.candidates_by_rarity[index] += 1;
        if rating.discovery.is_some() {
            let band = usize::try_from(self.job.max(1).ilog2()).unwrap_or(usize::MAX);
            let bands = &mut self.report.discovering_candidates_by_log2_job;
            if bands.len() <= band {
                bands.resize(band + 1, 0);
            }
            bands[band] += 1;
        }
        self.report.candidates_rated = self.report.candidates_rated.saturating_add(1);
        if moved {
            self.report.candidates_moved = self.report.candidates_moved.saturating_add(1);
            *self.report.moved_by_rarest_id.entry(rating.id).or_insert(0) += 1;
            self.job_moved = true;
        }
    }

    pub(crate) fn finish_job(&mut self) {
        if self.job_moved {
            self.report.jobs_moved = self.report.jobs_moved.saturating_add(1);
            self.job_moved = false;
        }
        let jobs = self.jobs;
        let refresh = self.job_counted && jobs.is_multiple_of(DISCOVERY_REFRESH_JOBS);
        let counts = &self.ids;
        let Some(weights) = self.discoveries.as_mut() else {
            return;
        };
        let risen = std::mem::take(&mut weights.risen);
        for (discovery, level) in &mut weights.levels {
            if risen.contains(discovery) {
                level.hits = fewest_hits(counts, &level.ids);
            } else if !refresh {
                continue;
            }
            let updated = rarity(jobs, level.hits).min(weights.cap_log2);
            if updated != level.level {
                level.level = updated;
                weights.changed.push((*discovery, updated));
            }
        }
    }

    pub(crate) fn report(&self) -> Option<CoverageReport> {
        self.is_active().then(|| CoverageReport {
            jobs_with_ids: self.jobs,
            distinct_ids: u64::try_from(self.ids.len()).unwrap_or(u64::MAX),
            memory_bytes: heap::btree_bytes::<u64, IdCount>(1, self.ids.len())
                + heap::btree_bytes::<u64, u64>(1, self.report.moved_by_rarest_id.len()),
            discovery_weight: self.discoveries.as_ref().map(DiscoveryLevels::report),
            ..self.report.clone()
        })
    }
}

impl DiscoveryLevels {
    fn report(&self) -> DiscoveryWeightReport {
        let mut by_level = vec![0; usize::from(self.cap_log2) + 1];
        for level in self.levels.values() {
            by_level[usize::from(level.level)] += 1;
        }
        DiscoveryWeightReport {
            cap_log2: self.cap_log2,
            discoveries: u64::try_from(self.levels.len()).unwrap_or(u64::MAX),
            cells: 0,
            discoveries_by_log2_weight: by_level,
            log2_weight_by_discovery: self
                .levels
                .iter()
                .map(|(discovery, level)| (*discovery, level.level))
                .collect(),
            frontier_draws_by_discovery: BTreeMap::new(),
        }
    }
}

fn fewest_hits(counts: &BTreeMap<u64, IdCount>, ids: &[u64]) -> u64 {
    ids.iter()
        .map(|id| counts.get(id).map_or(0, |count| count.jobs))
        .min()
        .unwrap_or(u64::MAX)
}

#[must_use]
pub fn rarity(jobs: u64, count: u64) -> u8 {
    let total = u128::from(jobs) + 1;
    let hit = u128::from(count) + 1;
    if total <= hit {
        return 0;
    }
    let mut bits = total.ilog2() - hit.ilog2();
    if hit << bits > total {
        bits -= 1;
    }
    u8::try_from(bits).unwrap_or(u8::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rarity_is_the_floor_of_log2_of_jobs_over_hits() {
        assert_eq!(rarity(0, 0), 0);
        assert_eq!(rarity(1, 1), 0);
        assert_eq!(rarity(3, 1), 1);
        assert_eq!(rarity(6, 1), 1);
        assert_eq!(rarity(7, 1), 2);
        assert_eq!(rarity(1_000, 0), 9);
        assert_eq!(rarity(1_023, 0), 10);
        assert_eq!(rarity(1_000, 1_000), 0);
        assert_eq!(rarity(u64::MAX, 0), 64);
        for jobs in 0..200_u64 {
            for count in 0..=jobs {
                let exact = ((jobs + 1) as f64 / (count + 1) as f64).log2().floor();
                assert_eq!(f64::from(rarity(jobs, count)), exact, "{jobs} {count}");
            }
        }
    }

    #[test]
    fn an_action_is_rated_by_its_rarest_id_counting_each_job_once() {
        let mut counts = CoverageCounts::default();
        let rarity =
            |counts: &mut CoverageCounts, ids: &[u64]| counts.rate(ids).map(|rating| rating.rarity);
        counts.begin_job(1);
        assert_eq!(counts.rate(&[]), None);
        assert!(!counts.is_active());
        let first = CoverageRating {
            rarity: 0,
            id: 10,
            discovery: Some(1),
        };
        assert_eq!(counts.rate(&[10, 11]), Some(first));
        assert_eq!(counts.rate(&[10, 11]), Some(first));
        counts.finish_job();
        for job in 2..=7 {
            counts.begin_job(job);
            assert_eq!(rarity(&mut counts, &[10]), Some(0));
            counts.finish_job();
        }
        counts.begin_job(8);
        let new = counts.rate(&[10, 12]).expect("rated");
        assert_eq!(
            new,
            CoverageRating {
                rarity: 2,
                id: 12,
                discovery: Some(3),
            }
        );
        let old = counts.rate(&[10, 11]).expect("rated");
        assert_eq!(
            old,
            CoverageRating {
                rarity: 1,
                id: 11,
                discovery: None,
            }
        );
        counts.record_candidate(new, true);
        counts.record_candidate(old, false);
        counts.finish_job();
        let report = counts.report().expect("active");
        assert_eq!(report.jobs_with_ids, 8);
        assert_eq!(report.distinct_ids, 3);
        assert_eq!(report.actions_with_ids, 10);
        assert_eq!(report.action_ids, 14);
        assert_eq!(report.candidates_rated, 2);
        assert_eq!(report.candidates_moved, 1);
        assert_eq!(report.jobs_moved, 1);
        assert_eq!(report.candidates_by_rarity, vec![0, 1, 1]);
        assert_eq!(report.moved_by_rarest_id, BTreeMap::from([(12, 1)]));
        assert_eq!(report.discovering_actions, 3);
        assert_eq!(report.discovering_candidates_by_log2_job, vec![0, 0, 0, 1]);
    }

    #[test]
    fn a_discovery_level_follows_the_fewest_hits_over_its_first_hit_ids() {
        fn job(counts: &mut CoverageCounts, job: u64, ids: &[u64]) {
            counts.begin_job(job);
            counts.rate(ids);
            counts.finish_job();
        }
        let mut counts = CoverageCounts::default();
        assert_eq!(counts.watch_discovery(1, vec![1]), None);
        counts.weigh_discoveries(3);
        for number in 1..=6 {
            job(&mut counts, number, &[1]);
        }
        counts.begin_job(7);
        assert_eq!(counts.rate(&[1, 20, 21]).expect("rated").discovery, Some(2));
        let first = counts.first_hit_ids(&[21, 1, 20, 21]);
        assert_eq!(first, vec![20, 21]);
        assert_eq!(counts.watch_discovery(2, first), Some(2));
        assert_eq!(counts.watch_discovery(2, vec![1]), Some(2));
        counts.finish_job();
        for number in 8..=15 {
            job(&mut counts, number, &[1]);
        }
        assert!(counts.take_changed_levels().is_empty());
        job(&mut counts, 16, &[21]);
        assert_eq!(counts.take_changed_levels(), vec![(2, 3)]);
        job(&mut counts, 17, &[20]);
        assert_eq!(counts.take_changed_levels(), vec![(2, 2)]);
        for number in 18..DISCOVERY_REFRESH_JOBS {
            job(&mut counts, number, &[1]);
        }
        assert!(counts.take_changed_levels().is_empty());
        job(&mut counts, DISCOVERY_REFRESH_JOBS, &[1]);
        assert_eq!(counts.take_changed_levels(), vec![(2, 3)]);
        let report = counts
            .report()
            .expect("active")
            .discovery_weight
            .expect("weighed");
        assert_eq!(report.cap_log2, 3);
        assert_eq!(report.discoveries, 1);
        assert_eq!(report.discoveries_by_log2_weight, vec![0, 0, 0, 1]);
        assert_eq!(report.log2_weight_by_discovery, BTreeMap::from([(2, 3)]));
    }
}
