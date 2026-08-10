// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deterministic fixture construction: an explicit tree builder (used both by
//! the committed hand fixtures and the random sweeps) and a seeded random tree
//! generator. Determinism discipline: the only entropy is a caller-provided
//! seed through splitmix64; no wall clock, no host randomness.

use crate::data::{
    CfgId, Cut, EntryCommitRec, EntryId, FinalizeRec, Fixture, LineageRec, Moment, ObsCutRec,
    OrderScope, Payload, Pos, PropId, PropertyDecl, QueryId, ReduceOp, RegId, RegisterDecl, Replay,
    Revision, RolloutId, ScrapeLineRec, SdkEventRec, SealId, SealRec, SeqQueryRec, SourceDecl,
    SourceId, WorkingRec,
};

/// splitmix64 — tiny, seeded, deterministic.
#[derive(Clone, Debug)]
pub struct SplitMix64(pub u64);

impl SplitMix64 {
    /// Next raw value.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` (n > 0).
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }
}

/// Explicit fixture builder. Maintains, per rollout, the genesis-complete
/// replay vector (the referee's authority) alongside the persisted suffix
/// records (the ledger the dataflow sees).
pub struct Builder {
    config: CfgId,
    fixture: Fixture,
    /// Full replay vector per (config, rollout).
    replay: Vec<((CfgId, RolloutId), Vec<SdkEventRec>)>,
    /// Last moment per rollout (for the nondecreasing-moment contract).
    last_moment: Vec<(RolloutId, Moment)>,
    /// Start position per rollout (its branch-point count; 0 for genesis).
    starts: Vec<(RolloutId, Pos)>,
    /// Birth (fork-cut) Moment per rollout; 0 for genesis.
    births: Vec<(RolloutId, Moment)>,
    next_rollout: RolloutId,
}

impl Builder {
    /// New builder for one campaign configuration.
    pub fn new(name: &str, config: CfgId) -> Builder {
        Builder {
            config,
            fixture: Fixture {
                name: name.to_owned(),
                ..Fixture::default()
            },
            replay: Vec::new(),
            last_moment: Vec::new(),
            starts: Vec::new(),
            births: Vec::new(),
            next_rollout: 0,
        }
    }

    /// Declare a register.
    pub fn reg(&mut self, rev: Revision, reg: RegId, op: ReduceOp) -> &mut Self {
        self.fixture.registers.push(RegisterDecl {
            rev,
            config: self.config,
            reg,
            op,
        });
        self
    }

    /// Declare a source.
    pub fn source(&mut self, rev: Revision, source: SourceId, scope: OrderScope) -> &mut Self {
        self.fixture.sources.push(SourceDecl {
            rev,
            config: self.config,
            source,
            scope,
        });
        self
    }

    /// Declare a property under one source schema.
    pub fn property(
        &mut self,
        rev: Revision,
        source: SourceId,
        property: PropId,
        must_hit: bool,
    ) -> &mut Self {
        self.fixture.properties.push(PropertyDecl {
            rev,
            config: self.config,
            source,
            property,
            must_hit,
        });
        self
    }

    /// Start a genesis rollout (empty vector).
    pub fn genesis(&mut self) -> RolloutId {
        let id = self.next_rollout;
        self.next_rollout += 1;
        self.replay.push(((self.config, id), Vec::new()));
        self.last_moment.push((id, 0));
        self.starts.push((id, 0));
        self.births.push((id, 0));
        id
    }

    /// Branch `child` from `parent` at `cut`, recording lineage and seeding
    /// the child's replay vector with the restored prefix. The prefix is
    /// inherited — never re-persisted under the child identity.
    pub fn fork(&mut self, rev: Revision, parent: RolloutId, cut: Cut) -> RolloutId {
        let pvec = self.vector(parent);
        assert!(
            cut.count as usize <= pvec.len(),
            "fork cut beyond parent evidence: {} > {}",
            cut.count,
            pvec.len()
        );
        // A cut is a physical seal coordinate: the parent machine exists only
        // from its own branch point onward, so no cut on it can precede its
        // start. Lineage composition relies on this (cuts are nondecreasing
        // along every lineage path).
        assert!(
            cut.count >= self.start_of(parent),
            "cut before the parent's own branch point: {} < {}",
            cut.count,
            self.start_of(parent)
        );
        let prefix: Vec<SdkEventRec> = pvec[..cut.count as usize].to_vec();
        let id = self.next_rollout;
        self.next_rollout += 1;
        self.replay.push(((self.config, id), prefix));
        self.last_moment.push((id, cut.moment));
        self.starts.push((id, cut.count));
        self.births.push((id, cut.moment));
        self.fixture.lineage.push(LineageRec {
            rev,
            config: self.config,
            child: id,
            parent,
            cut,
        });
        id
    }

    /// Append one event to `rollout`'s own suffix; `pos` is assigned as the
    /// current full-vector length (the cumulative persisted vector position).
    pub fn push(
        &mut self,
        rev: Revision,
        rollout: RolloutId,
        source: SourceId,
        moment: Moment,
        payload: Payload,
    ) -> Pos {
        let last = self
            .last_moment
            .iter_mut()
            .find(|(r, _)| *r == rollout)
            .expect("push to unknown rollout");
        assert!(
            moment >= last.1,
            "moments must be nondecreasing within a rollout"
        );
        last.1 = moment;
        let vec = self
            .replay
            .iter_mut()
            .find(|(k, _)| *k == (self.config, rollout))
            .map(|(_, v)| v)
            .expect("push to unknown rollout");
        let pos = vec.len() as Pos;
        let rec = SdkEventRec {
            rev,
            config: self.config,
            rollout,
            source,
            pos,
            moment,
            payload,
        };
        vec.push(rec.clone());
        self.fixture.events.push(rec);
        pos
    }

    /// Append a scrape line (source-local ordinal assigned per rollout).
    pub fn scrape(&mut self, rev: Revision, rollout: RolloutId, tag: u32) -> &mut Self {
        let local_ord = self
            .fixture
            .scrape
            .iter()
            .filter(|s| s.rollout == rollout)
            .count() as u64;
        self.fixture.scrape.push(ScrapeLineRec {
            rev,
            config: self.config,
            rollout,
            local_ord,
            tag,
        });
        self
    }

    /// Record a configured unsealed evidence cut.
    pub fn obs_cut(&mut self, rev: Revision, rollout: RolloutId, cut: Cut) -> &mut Self {
        assert!(
            cut.count >= self.start_of(rollout),
            "cut before the rollout's branch point"
        );
        assert!(
            cut.count as usize <= self.vector(rollout).len(),
            "cut beyond rollout evidence"
        );
        self.fixture.obs_cuts.push(ObsCutRec {
            rev,
            config: self.config,
            rollout,
            cut,
        });
        self
    }

    /// Record a candidate seal.
    pub fn seal(&mut self, rev: Revision, rollout: RolloutId, seal: SealId, cut: Cut) -> &mut Self {
        assert!(
            cut.count >= self.start_of(rollout),
            "seal before the rollout's branch point"
        );
        assert!(
            cut.count as usize <= self.vector(rollout).len(),
            "seal cut beyond rollout evidence"
        );
        self.fixture.seals.push(SealRec {
            rev,
            config: self.config,
            rollout,
            seal,
            cut,
        });
        self
    }

    /// Record a committed Entry assignment.
    pub fn commit_entry(
        &mut self,
        rev: Revision,
        entry: EntryId,
        rollout: RolloutId,
        seal: SealId,
        quality: i64,
    ) -> &mut Self {
        self.fixture.entry_commits.push(EntryCommitRec {
            rev,
            config: self.config,
            entry,
            rollout,
            seal,
            quality,
        });
        self
    }

    /// Record a working-set membership update.
    pub fn working(
        &mut self,
        rev: Revision,
        rollout: RolloutId,
        pos: Pos,
        delta: i64,
    ) -> &mut Self {
        self.fixture.working.push(WorkingRec {
            rev,
            config: self.config,
            rollout,
            pos,
            delta,
        });
        self
    }

    /// Record the campaign finalization (closure) for this configuration.
    pub fn finalize(&mut self, rev: Revision) -> &mut Self {
        self.fixture.finalizations.push(FinalizeRec {
            rev,
            config: self.config,
        });
        self
    }

    /// Record a cross-source sequence query.
    pub fn seq_query(
        &mut self,
        rev: Revision,
        query: QueryId,
        src_a: SourceId,
        src_b: SourceId,
    ) -> &mut Self {
        self.fixture.seq_queries.push(SeqQueryRec {
            rev,
            config: self.config,
            query,
            src_a,
            src_b,
        });
        self
    }

    /// Current full vector of a rollout.
    pub fn vector(&self, rollout: RolloutId) -> &[SdkEventRec] {
        self.replay
            .iter()
            .find(|(k, _)| *k == (self.config, rollout))
            .map(|(_, v)| v.as_slice())
            .expect("unknown rollout")
    }

    /// Start position of a rollout (its branch-point count; 0 for genesis).
    pub fn start_of(&self, rollout: RolloutId) -> Pos {
        self.starts
            .iter()
            .find(|(r, _)| *r == rollout)
            .map(|(_, p)| *p)
            .expect("unknown rollout")
    }

    /// Birth (fork-cut) Moment of a rollout; 0 for genesis.
    pub fn birth_of(&self, rollout: RolloutId) -> Moment {
        self.births
            .iter()
            .find(|(r, _)| *r == rollout)
            .map(|(_, m)| *m)
            .expect("unknown rollout")
    }

    /// Current moment of a rollout's last event (or its fork moment).
    pub fn moment(&self, rollout: RolloutId) -> Moment {
        self.last_moment
            .iter()
            .find(|(r, _)| *r == rollout)
            .map(|(_, m)| *m)
            .expect("unknown rollout")
    }

    /// Finish: the persisted fixture and the genesis-complete replay.
    pub fn finish(self) -> (Fixture, Replay) {
        (self.fixture, Replay { full: self.replay })
    }
}

/// Parameters for the seeded random tree generator.
#[derive(Clone, Copy, Debug)]
pub struct TreeParams {
    /// Number of rollouts (tree nodes) to generate.
    pub rollouts: u32,
    /// Maximum events appended per rollout suffix (min is 1).
    pub max_events: u64,
    /// Number of declared registers (cycled through all four ops).
    pub registers: u32,
    /// Number of note tags.
    pub tags: u32,
    /// Configured unsealed cuts per rollout (capped by suffix length).
    pub cuts_per_rollout: u32,
    /// Candidate seals per rollout (capped by suffix length).
    pub seals_per_rollout: u32,
}

/// The two rollout-global sources the generator emits on, and the scrape.
pub const SRC_MAIN: SourceId = 0;
/// Secondary rollout-global source (cross-source sequence partner).
pub const SRC_AUX: SourceId = 1;
/// The source-local serial scrape.
pub const SRC_SCRAPE: SourceId = 2;

/// Generate a random branch tree driven by `seed`. Revisions: declarations at
/// 1; each rollout's evidence, cuts, and lineage at `2 + index`; every seal one
/// revision after the last evidence revision; entry commits one after that
/// (the two-revision materialization barrier holds by construction).
pub fn random_tree(name: &str, seed: u64, p: TreeParams) -> (Fixture, Replay) {
    assert!(
        p.rollouts > 0 && p.max_events > 0 && p.registers > 0 && p.tags > 0,
        "TreeParams counts must be positive (zero would divide the RNG range)"
    );
    let mut rng = SplitMix64(seed);
    let mut b = Builder::new(name, 0);
    for i in 0..p.registers {
        let op = match i % 4 {
            0 => ReduceOp::Set,
            1 => ReduceOp::Max,
            2 => ReduceOp::Min,
            _ => ReduceOp::Accumulate,
        };
        b.reg(1, 100 + i, op);
    }
    b.source(1, SRC_MAIN, OrderScope::RolloutGlobal);
    b.source(1, SRC_AUX, OrderScope::RolloutGlobal);
    b.source(1, SRC_SCRAPE, OrderScope::SourceLocal);
    b.property(1, SRC_MAIN, 500, true);
    b.property(1, SRC_MAIN, 501, true);

    let mut rollouts: Vec<RolloutId> = Vec::new();
    let mut next_seal: SealId = 0;
    let mut next_entry: EntryId = 0;
    let mut seal_targets: Vec<(RolloutId, SealId)> = Vec::new();

    for i in 0..p.rollouts {
        let rev = 2 + Revision::from(i);
        let r = if rollouts.is_empty() {
            b.genesis()
        } else {
            // Fork from a random existing rollout at a random cut of its
            // current vector (same-cut siblings arise naturally from reuse).
            let parent = rollouts[rng.below(rollouts.len() as u64) as usize];
            let plen = b.vector(parent).len() as u64;
            let pstart = b.start_of(parent);
            let count = pstart + rng.below(plen - pstart + 1);
            // The cut moment: the moment of the last included event (or the
            // parent's fork moment when nothing is included).
            // Clamp to the parent's own birth: a fork cut is a physical
            // seal coordinate on the parent, which exists only from its own
            // branch Moment onward (J1 coherence).
            let moment = cut_moment(b.vector(parent), count).max(b.birth_of(parent));
            b.fork(rev, parent, Cut { moment, count })
        };
        rollouts.push(r);

        let events = 1 + rng.below(p.max_events);
        let mut moment = b.moment(r);
        for _ in 0..events {
            // 0-steps produce same-moment clusters on purpose.
            moment += rng.below(3);
            let source = if rng.below(4) == 0 { SRC_AUX } else { SRC_MAIN };
            let payload = match rng.below(10) {
                0..=5 => Payload::Register {
                    reg: 100 + rng.below(u64::from(p.registers)) as u32,
                    value: rng.below(100) as i64 - 50,
                },
                6..=7 => Payload::Note {
                    tag: rng.below(u64::from(p.tags)) as u32,
                },
                _ => Payload::Assertion {
                    site: 900 + rng.below(3) as u32,
                    property: 500,
                    passed: rng.below(3) > 0,
                },
            };
            b.push(rev, r, source, moment, payload);
        }
        if rng.below(3) == 0 {
            b.scrape(rev, r, 40 + rng.below(4) as u32);
        }

        let start = b
            .fixture
            .lineage
            .iter()
            .find(|l| l.child == r)
            .map(|l| l.cut.count)
            .unwrap_or(0);
        let len = b.vector(r).len() as u64;
        // Obs-cut counts are a record identity per rollout (validated
        // unique): dedup the random draws.
        let mut cut_counts = std::collections::BTreeSet::new();
        for _ in 0..p.cuts_per_rollout {
            cut_counts.insert(start + rng.below(len - start + 1));
        }
        for count in cut_counts {
            let moment = cut_moment(b.vector(r), count).max(b.birth_of(r));
            b.obs_cut(rev, r, Cut { moment, count });
        }
        for _ in 0..p.seals_per_rollout {
            let count = start + rng.below(len - start + 1);
            let moment = cut_moment(b.vector(r), count).max(b.birth_of(r));
            let seal = next_seal;
            next_seal += 1;
            seal_targets.push((r, seal));
            // Seal revision assigned below (after all evidence revisions).
            b.seal(
                2 + Revision::from(p.rollouts),
                r,
                seal,
                Cut { moment, count },
            );
        }
    }

    // Entry commits: a random subset of seals, one revision after seals.
    let commit_rev = 3 + Revision::from(p.rollouts);
    for (r, s) in &seal_targets {
        if rng.below(2) == 0 {
            let entry = next_entry;
            next_entry += 1;
            b.commit_entry(commit_rev, entry, *r, *s, rng.below(10) as i64);
        }
    }

    // Working membership: admit a few coordinates, then expire a subset one
    // revision later.
    let admit_rev = commit_rev;
    let expire_rev = commit_rev + 1;
    let mut admitted: Vec<(RolloutId, Pos)> = Vec::new();
    for r in &rollouts {
        let own: Vec<Pos> = b
            .fixture
            .events
            .iter()
            .filter(|e| e.rollout == *r)
            .map(|e| e.pos)
            .collect();
        for pos in own {
            if rng.below(3) == 0 {
                b.working(admit_rev, *r, pos, 1);
                admitted.push((*r, pos));
            }
        }
    }
    for (r, pos) in admitted {
        if rng.below(2) == 0 {
            b.working(expire_rev, r, pos, -1);
        }
    }

    b.seq_query(1, 0, SRC_MAIN, SRC_AUX);
    b.seq_query(1, 1, SRC_MAIN, SRC_SCRAPE);
    // Campaign closure: after all evidence (finalized absence facts become
    // derivable); working-set retention continues at expire_rev.
    b.finalize(commit_rev);

    b.finish()
}

/// The moment of the last event included by a cut at `count` (or the previous
/// event's moment when the cut includes nothing beyond the vector start).
pub fn cut_moment(vector: &[SdkEventRec], count: Pos) -> Moment {
    if count == 0 {
        0
    } else {
        vector[count as usize - 1].moment
    }
}
