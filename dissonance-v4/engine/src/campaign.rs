// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    archive::{Archive, Key, Order, StateRec, compare},
    rng::Rng,
    snap::Snap,
    target::{Action, JobId, Kind, Position, Probe, Vocabulary, Want, Worker},
};

#[derive(Clone, Debug)]
pub struct Config {
    pub seed: u64,
    pub batch: usize,
    pub budget: u64,
    pub rekey_every: u64,
    pub root_only: bool,
}

impl Config {
    pub fn new(seed: u64, budget: u64) -> Self {
        Self {
            seed,
            batch: 32,
            budget,
            rekey_every: 16,
            root_only: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stop {
    Milestone,
    Budget,
    Memory,
    Error(String),
}

#[derive(Clone, Debug, Serialize)]
pub struct Outcome {
    pub stop: Stop,
    pub work: u64,
    pub jobs: u64,
    pub job_stream_sha256: String,
    pub milestone_state: Option<u32>,
}

struct Job {
    parent: u32,
    cell: u32,
    actions: Vec<Action>,
    start: Arc<Snap>,
    route: u64,
}

struct Cand {
    key: Key,
    scalars: Vec<u64>,
    route: u64,
    at: Position,
    work: u64,
    snapshot: Option<Vec<u8>>,
    stored: Option<Snap>,
}

struct JobOut {
    work: u64,
    cands: Vec<Cand>,
    spans: Vec<u64>,
    steps: Vec<[u64; 65]>,
    milestone: Option<(u64, Position, Vec<u64>)>,
}

struct Walk {
    limit: u64,
    prev: Option<u64>,
    at: i128,
    lo: i128,
    hi: i128,
    best: u64,
    steps: [u64; 65],
}

impl Walk {
    fn new(limit: u64) -> Self {
        Walk {
            limit,
            prev: None,
            at: 0,
            lo: 0,
            hi: 0,
            best: 0,
            steps: [0; 65],
        }
    }

    fn step(&mut self, v: u64) {
        if let Some(p) = self.prev {
            let d = v.abs_diff(p);
            if d > 0 {
                self.steps[log2_bucket(d)] += 1;
            }
            if d <= self.limit {
                self.at += i128::from(v) - i128::from(p);
                self.lo = self.lo.min(self.at);
                self.hi = self.hi.max(self.at);
            } else {
                self.best = self.span();
                self.at = 0;
                self.lo = 0;
                self.hi = 0;
            }
        }
        self.prev = Some(v);
    }

    fn span(&self) -> u64 {
        u64::try_from(self.hi - self.lo)
            .unwrap_or(u64::MAX)
            .max(self.best)
    }
}

struct JobProbe<'a> {
    archive: &'a Archive,
    route: u64,
    cands: Vec<Cand>,
    walks: Vec<Walk>,
    milestone: Option<(u64, Position, Vec<u64>)>,
}

impl Probe for JobProbe<'_> {
    fn observe(
        &mut self,
        at: Position,
        work: u64,
        scalars: &[u64],
        _array: &[u8],
        milestone: bool,
    ) -> Want {
        for (w, v) in self.walks.iter_mut().zip(scalars) {
            w.step(*v);
        }
        if milestone {
            self.milestone = Some((work, at, scalars.to_vec()));
            return Want::Stop;
        }
        let route = self.route + work;
        let key = self.archive.key(scalars);
        if !self.archive.would_admit(&key, scalars, route) {
            return Want::Nothing;
        }
        let kinds = &self.archive.kinds;
        if self
            .cands
            .iter()
            .any(|c| c.key == key && compare(kinds, scalars, &c.scalars) == Order::Worse)
        {
            return Want::Nothing;
        }
        self.cands
            .retain(|c| c.key != key || compare(kinds, &c.scalars, scalars) != Order::Worse);
        let equal: Vec<usize> = (0..self.cands.len())
            .filter(|&i| {
                self.cands[i].key == key
                    && compare(kinds, &self.cands[i].scalars, scalars) == Order::Equal
            })
            .collect();
        if equal.len() >= 2
            && let Some(&last) = equal.iter().max_by_key(|&&i| self.cands[i].route)
        {
            self.cands.remove(last);
        }
        self.cands.push(Cand {
            key,
            scalars: scalars.to_vec(),
            route,
            at,
            work,
            snapshot: None,
            stored: None,
        });
        Want::Capture
    }

    fn keep(&mut self, state: Vec<u8>) {
        if let Some(c) = self.cands.last_mut() {
            c.snapshot = Some(state);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stretch {
    pub job: u64,
    pub steps: u64,
    pub jobs_since_previous: u64,
}

pub fn prefix(actions: &[Action], at: Position) -> Vec<Action> {
    let mut out: Vec<Action> = actions[..at.action].to_vec();
    if let Some(a) = actions.get(at.action) {
        out.push(Action {
            hold: at.step + 1,
            ..*a
        });
    }
    out
}

pub fn draw(rng: &mut Rng, vocab: &Vocabulary, steps: u64) -> Vec<Action> {
    let mut out = Vec::new();
    let mut total = 0u64;
    while total < steps {
        let input = rng.below(u64::from(vocab.inputs)) as u32;
        let arg = rng.next_u64();
        let hold = if vocab.holds {
            let e = rng.below(6);
            (1u64 << e) + rng.below(1u64 << e)
        } else {
            1
        };
        let hold = hold.min(steps - total);
        out.push(Action {
            input,
            arg,
            hold: hold as u32,
        });
        total += hold;
    }
    out
}

fn luby_exponent(index: u64) -> u32 {
    let mut i = u128::from(index.max(1));
    loop {
        let k = 128 - i.leading_zeros();
        if i == (1u128 << k) - 1 {
            return k - 1;
        }
        i = i - (1u128 << (k - 1)) + 1;
    }
}

fn median_bucket(h: &[u64; 65]) -> Option<usize> {
    let n: u64 = h.iter().sum();
    let mut acc = 0;
    for (b, count) in h.iter().enumerate() {
        acc += count;
        if n > 0 && acc * 2 >= n {
            return Some(b);
        }
    }
    None
}

fn step_limit(h: &[u64; 65]) -> u64 {
    match median_bucket(h) {
        Some(b) if b < 63 => (1u64 << (b + 1)) - 1,
        _ => u64::MAX,
    }
}

fn log2_bucket(span: u64) -> usize {
    (64 - span.leading_zeros()) as usize
}

pub struct Campaign {
    pub archive: Archive,
    pub vocab: Vocabulary,
    pub config: Config,
    pub work: u64,
    pub jobs: u64,
    pub batches: u64,
    hasher: Sha256,
    root: Option<Arc<Snap>>,
    span_hist: Vec<[u64; 65]>,
    step_hist: Vec<[u64; 65]>,
    step_limits: Vec<u64>,
}

impl Campaign {
    pub fn new(
        kinds: Vec<Kind>,
        vocab: Vocabulary,
        config: Config,
        root: Vec<u8>,
        root_scalars: Vec<u64>,
    ) -> Self {
        let mut archive = Archive::new(kinds.clone());
        let root = Arc::new(Snap::full(&root));
        let key = archive.key(&root_scalars);
        archive.admit(
            StateRec {
                parent: None,
                actions: Vec::new(),
                route: 0,
                found: 0,
                scalars: root_scalars,
                snapshot: Some(root.clone()),
                cell: 0,
                job: 0,
            },
            key,
        );
        let mut hasher = Sha256::new();
        hasher.update(b"dissonance-v4 job stream 1");
        hasher.update(config.seed.to_le_bytes());
        hasher.update((config.batch as u64).to_le_bytes());
        hasher.update([u8::from(config.root_only)]);
        for k in &kinds {
            hasher.update([match k {
                Kind::Identity => 0u8,
                Kind::Higher => 1,
                Kind::Lower => 2,
            }]);
        }
        Self {
            archive,
            vocab,
            config,
            work: 0,
            jobs: 0,
            batches: 0,
            hasher,
            root: Some(root),
            span_hist: vec![[0; 65]; kinds.len()],
            step_hist: vec![[0; 65]; kinds.len()],
            step_limits: vec![u64::MAX; kinds.len()],
        }
    }

    fn plan(&mut self, rng: &mut Rng) -> Job {
        let (c, parent, steps) = if self.config.root_only {
            (self.archive.states[0].cell, 0, self.vocab.base_steps)
        } else {
            let width = self.archive.strata.width() as u64;
            let scalar = if width == 0 { 0 } else { rng.below(width) };
            let g = self.archive.pick_group(scalar, rng.unit(), rng.unit());
            let c = self.archive.pick_cell(g, rng.unit());
            let cell = &self.archive.cells[c as usize];
            let parent = cell.front[rng.below(cell.front.len() as u64) as usize];
            let grow = luby_exponent(cell.empty_streak + 1);
            (
                c,
                parent,
                self.vocab.base_steps.saturating_mul(1u64 << grow.min(40)),
            )
        };
        self.archive.note_chosen(c);
        let actions = draw(rng, &self.vocab, steps);
        let state = &self.archive.states[parent as usize];
        Job {
            parent,
            cell: c,
            actions,
            start: state
                .snapshot
                .clone()
                .or_else(|| self.root.clone())
                .expect("front states hold snapshots"),
            route: state.route,
        }
    }

    pub fn span_medians(&self) -> Vec<u32> {
        self.span_hist
            .iter()
            .map(|h| median_bucket(h).map_or(0, |b| b as u32))
            .collect()
    }

    fn shifts(&self) -> Vec<u32> {
        self.archive
            .kinds
            .iter()
            .zip(&self.span_hist)
            .map(|(k, h)| {
                if !k.is_identity() {
                    return 0;
                }
                median_bucket(h).map_or(0, |b| b.saturating_sub(1) as u32)
            })
            .collect()
    }

    pub fn run(
        &mut self,
        workers: &mut [Box<dyn Worker>],
        memory_ok: &mut dyn FnMut(&Archive) -> bool,
        progress: &mut dyn FnMut(&Campaign),
    ) -> Outcome {
        loop {
            if self.work >= self.config.budget {
                return self.finish(Stop::Budget, None);
            }
            if !memory_ok(&self.archive) {
                return self.finish(Stop::Memory, None);
            }
            let mut rng = Rng::new(self.config.seed, self.batches);
            let jobs: Vec<Job> = (0..self.config.batch)
                .map(|_| self.plan(&mut rng))
                .collect();
            let results = self.execute(workers, &jobs);
            for (job, out) in jobs.iter().zip(results) {
                let out = match out {
                    Ok(o) => o,
                    Err(e) => return self.finish(Stop::Error(e), None),
                };
                if let Some(stop) = self.admit(job, out) {
                    return stop;
                }
                if self.work >= self.config.budget {
                    return self.finish(Stop::Budget, None);
                }
            }
            self.batches += 1;
            if self.batches.is_multiple_of(self.config.rekey_every) {
                let shifts = self.shifts();
                let limits: Vec<u64> = self.step_hist.iter().map(step_limit).collect();
                if limits != self.step_limits {
                    self.step_limits = limits;
                    self.span_hist = vec![[0; 65]; self.span_hist.len()];
                }
                if shifts != self.archive.shifts {
                    self.hasher.update(b"rekey");
                    for s in &shifts {
                        self.hasher.update(s.to_le_bytes());
                    }
                    self.archive.rekey(shifts);
                }
                self.archive.refresh_ranks();
            }
            progress(self);
            if self.batches.is_multiple_of(self.config.rekey_every) {
                self.archive.maybe_prune();
            }
        }
    }

    fn execute(
        &self,
        workers: &mut [Box<dyn Worker>],
        jobs: &[Job],
    ) -> Vec<Result<JobOut, String>> {
        let next = AtomicUsize::new(0);
        let slots: Vec<Mutex<Option<Result<JobOut, String>>>> =
            jobs.iter().map(|_| Mutex::new(None)).collect();
        let archive = &self.archive;
        let n = archive.kinds.len();
        let limits = &self.step_limits;
        let batch = self.batches;
        thread::scope(|scope| {
            for worker in workers.iter_mut() {
                let next = &next;
                let slots = &slots;
                scope.spawn(move || {
                    loop {
                        let j = next.fetch_add(1, Ordering::Relaxed);
                        if j >= jobs.len() {
                            break;
                        }
                        let job = &jobs[j];
                        let start = match job.start.raw() {
                            Ok(s) => s,
                            Err(e) => {
                                *slots[j].lock().expect("slot lock") = Some(Err(e));
                                continue;
                            }
                        };
                        let mut probe = JobProbe {
                            archive,
                            route: job.route,
                            cands: Vec::new(),
                            walks: (0..n).map(|i| Walk::new(limits[i])).collect(),
                            milestone: None,
                        };
                        let result = worker
                            .run(
                                JobId {
                                    batch,
                                    index: j as u64,
                                },
                                &start,
                                &job.actions,
                                &mut probe,
                            )
                            .map(|work| JobOut {
                                work,
                                spans: probe.walks.iter().map(Walk::span).collect(),
                                steps: probe.walks.iter().map(|w| w.steps).collect(),
                                cands: probe
                                    .cands
                                    .into_iter()
                                    .map(|mut c| {
                                        c.stored = c
                                            .snapshot
                                            .take()
                                            .map(|b| Snap::encode(&b, &job.start, &start));
                                        c
                                    })
                                    .collect(),
                                milestone: probe.milestone,
                            });
                        *slots[j].lock().expect("slot lock") = Some(result);
                    }
                });
            }
        });
        slots
            .into_iter()
            .map(|s| {
                s.into_inner()
                    .expect("slot lock")
                    .unwrap_or_else(|| Err("job did not run".into()))
            })
            .collect()
    }

    fn admit(&mut self, job: &Job, out: JobOut) -> Option<Outcome> {
        let before = self.work;
        self.jobs += 1;
        self.hasher.update(job.parent.to_le_bytes());
        for a in &job.actions {
            self.hasher.update(a.input.to_le_bytes());
            self.hasher.update(a.arg.to_le_bytes());
            self.hasher.update(a.hold.to_le_bytes());
        }
        for (h, s) in self.span_hist.iter_mut().zip(&out.spans) {
            h[log2_bucket(*s)] += 1;
        }
        for (h, s) in self.step_hist.iter_mut().zip(&out.steps) {
            for (a, b) in h.iter_mut().zip(s) {
                *a += b;
            }
        }
        let mut admitted = 0u64;
        for cand in out.cands {
            let rec = StateRec {
                parent: Some(job.parent),
                actions: prefix(&job.actions, cand.at),
                route: cand.route,
                found: before + cand.work,
                scalars: cand.scalars,
                snapshot: cand.stored.map(Arc::new),
                cell: 0,
                job: self.jobs,
            };
            if rec.snapshot.is_none() {
                continue;
            }
            if let Some((id, _)) = self.archive.admit(rec, cand.key) {
                admitted += 1;
                self.hasher.update(id.to_le_bytes());
            }
        }
        let cell = &mut self.archive.cells[job.cell as usize];
        cell.empty_streak = if admitted > 0 {
            0
        } else {
            cell.empty_streak + 1
        };
        if let Some((work, at, scalars)) = out.milestone {
            self.work = before + work;
            self.hasher.update(self.work.to_le_bytes());
            let key = self.archive.key(&scalars);
            let rec = StateRec {
                parent: Some(job.parent),
                actions: prefix(&job.actions, at),
                route: job.route + work,
                found: self.work,
                scalars,
                snapshot: None,
                cell: u32::MAX,
                job: self.jobs,
            };
            let id = self.archive.states.len() as u32;
            let parent_cell = self.archive.states[job.parent as usize].cell;
            let cell = self.archive.index.get(&key).copied().unwrap_or(parent_cell);
            self.archive.states.push(StateRec { cell, ..rec });
            return Some(self.finish(Stop::Milestone, Some(id)));
        }
        self.work = before + out.work;
        self.hasher.update(out.work.to_le_bytes());
        None
    }

    fn finish(&mut self, stop: Stop, milestone_state: Option<u32>) -> Outcome {
        let digest = self.hasher.clone().finalize();
        Outcome {
            stop,
            work: self.work,
            jobs: self.jobs,
            job_stream_sha256: digest.iter().map(|b| format!("{b:02x}")).collect(),
            milestone_state,
        }
    }

    pub fn stretches(&self, state: u32) -> Vec<Stretch> {
        let mut chain = Vec::new();
        let mut s = Some(state);
        while let Some(id) = s {
            let rec = &self.archive.states[id as usize];
            if rec.parent.is_some() {
                chain.push(id);
            }
            s = rec.parent;
        }
        let mut prev = 0;
        chain
            .iter()
            .rev()
            .map(|&id| {
                let rec = &self.archive.states[id as usize];
                let out = Stretch {
                    job: rec.job,
                    steps: rec.actions.iter().map(|a| u64::from(a.hold)).sum(),
                    jobs_since_previous: rec.job - prev,
                };
                prev = rec.job;
                out
            })
            .collect()
    }

    pub fn route(&self, state: u32) -> Vec<Action> {
        let mut chain = Vec::new();
        let mut s = Some(state);
        while let Some(id) = s {
            let rec = &self.archive.states[id as usize];
            chain.push(id);
            s = rec.parent;
        }
        chain
            .iter()
            .rev()
            .flat_map(|&id| self.archive.states[id as usize].actions.iter().copied())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{Walk, luby_exponent, step_limit};

    #[test]
    fn luby_sequence() {
        let got: Vec<u32> = (1..=15).map(luby_exponent).collect();
        assert_eq!(got, [0, 0, 1, 0, 0, 1, 2, 0, 0, 1, 0, 0, 1, 2, 3]);
        assert_eq!(luby_exponent(u64::MAX), 63);
    }

    #[test]
    fn walk_span_skips_jumps_above_the_step_limit() {
        let mut w = Walk::new(7);
        for v in [100, 103, 106, 109, 5000, 112, 115, 0, 3, 6, 9, 12, 15] {
            w.step(v);
        }
        assert_eq!(w.span(), 15);
        let mut drift = Walk::new(7);
        for v in [0, 4, 8, 12, 0, 4, 8, 12, 0, 4] {
            drift.step(v);
        }
        assert_eq!(drift.span(), 12);
        let mut h = [0u64; 65];
        h[2] = 10;
        h[13] = 2;
        assert_eq!(step_limit(&h), 7);
        assert_eq!(step_limit(&[0; 65]), u64::MAX);
    }
}
