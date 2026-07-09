// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reconstructing the recorded campaign: its **selection stream** (which branch
//! descended from which) and its **control fold** (that the harness reproduces
//! what the campaign recorded).
//!
//! Axis (c) asks whether *every ancestor of every bug-finding run still claims a
//! cell when it arrives* under a candidate. That needs the ancestor chains, and
//! the campaign logs record only each find's `path_len` / `novel_on_path` — not
//! the parent branch indices. They are nonetheless **exactly recoverable**: the
//! campaign is a pure function of its seed, and its only per-branch decisions
//! are drawn from one `Prng` stream whose draw pattern is fixed by the
//! configuration.
//!
//! Per branch (`conductor::benchcampaign::run_bench_campaign`), with
//! `step = branch + 1`:
//!
//! - **explore** (`Baseline`, or an empty frontier, or `step % explore_period ==
//!   0`) draws one word, the fresh campaign seed;
//! - **exploit** draws two: the frontier index, then — for bug 3's fault-less
//!   `RareEntropy` parent — the bit to twiddle in the parent's seed.
//!
//! A branch joins the frontier iff it claimed a fresh cell, which the recorded
//! `CampaignLog` states outright. So the whole stream replays.
//!
//! **This is not trusted — it is checked.** Every reconstructed branch seed is
//! compared against the seed the branch's recorded environment actually carries
//! ([`Error::ChainDiverged`]), and every reconstructed chain against the
//! `FindRecord` the campaign wrote ([`Error::ChainContradiction`]). All 10 240
//! bug-3 branches must agree, or the harness refuses to report an axis (c) it
//! cannot justify.
//!
//! ## Scope
//!
//! The exploit kernel is bug-shaped: a fault-bearing parent has its fault
//! jittered (a variable number of draws), a fault-less one has its seed
//! twiddled (exactly one draw). Only bug 3 (`RareEntropy`, which mints no
//! fault) is in the trace corpus, and only its fault-less kernel is
//! reconstructed here. Any other bug is refused rather than guessed at.

use std::collections::BTreeSet;

use explorer::Prng;

use benchmark::{BugId, Configuration};

use crate::error::{Error, Result};
use crate::observe::CampaignObs;

/// The bug whose fault-less exploit kernel [`reconstruct`] models.
const RECONSTRUCTIBLE_BUG: BugId = BugId(3);

/// The reconstructed campaign: the ancestry the search actually walked, and the
/// admissions (under the **recorded** v1 fold) that produced it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Chains {
    /// Each branch's parent branch, or `None` when it explored from genesis.
    pub parent: Vec<Option<u64>>,
    /// Whether each branch claimed a fresh cell **as the campaign recorded it**
    /// — i.e. whether it was admitted to the frontier.
    pub admitted: Vec<bool>,
    /// The finding runs' chains, root-first, including the finding branch. Empty
    /// when the campaign found nothing.
    pub find_chains: Vec<Vec<u64>>,
}

impl Chains {
    /// The **proper** ancestors of every finding run: the branches whose
    /// admission the find depended on. A find on an explore branch has none —
    /// it was minted fresh from genesis, so no cell function could have lost it.
    pub fn find_ancestors(&self) -> Vec<Vec<u64>> {
        self.find_chains
            .iter()
            .map(|chain| chain[..chain.len().saturating_sub(1)].to_vec())
            .collect()
    }
}

/// Reconstruct `obs`'s selection stream, checking every step against the record.
pub fn reconstruct(obs: &CampaignObs) -> Result<Chains> {
    let name = obs.name();
    if obs.bug != RECONSTRUCTIBLE_BUG {
        return Err(Error::Corpus {
            campaign: name,
            why: format!(
                "chain reconstruction models only bug {}'s fault-less exploit kernel (a seed \
                 twiddle, one draw); bug {}'s kernel jitters a staged fault with a \
                 fault-kind-dependent number of draws",
                RECONSTRUCTIBLE_BUG.0, obs.bug.0
            ),
        });
    }
    let period = obs.explore_period.max(1);

    let mut prng = Prng::new(obs.seed);
    let mut seen: BTreeSet<u64> = BTreeSet::new();
    // The novelty frontier, as `(branch, env seed)` in admission order.
    let mut frontier: Vec<(u64, u64)> = Vec::new();
    let mut parent = Vec::with_capacity(obs.branches.len());
    let mut admitted = Vec::with_capacity(obs.branches.len());

    for (i, event) in obs.log.events.iter().enumerate() {
        let branch = i as u64;
        let step = branch + 1;
        let exploit = obs.config == Configuration::Signal
            && !frontier.is_empty()
            && !step.is_multiple_of(period);

        let (env_seed, from) = if exploit {
            // `pick = prng.next_u64() % frontier.len()`, then the seed twiddle.
            let pick = (prng.next_u64() % frontier.len() as u64) as usize;
            let (parent_branch, parent_seed) = frontier[pick];
            let bit = prng.next_u64() % 64;
            (parent_seed ^ (1u64 << bit), Some(parent_branch))
        } else {
            (prng.next_u64(), None)
        };

        let recorded = *obs.env_seeds.get(i).ok_or_else(|| Error::Corpus {
            campaign: name.clone(),
            why: format!("branch {branch} has no recorded environment"),
        })?;
        if env_seed != recorded {
            return Err(Error::ChainDiverged {
                campaign: name,
                branch,
                recorded,
                replayed: env_seed,
            });
        }

        // Admission, first-wins, exactly as the campaign folded it.
        let mut novel = false;
        for &cell in &event.touched {
            if seen.insert(cell) {
                novel = true;
            }
        }
        parent.push(from);
        admitted.push(novel);
        if novel {
            frontier.push((branch, env_seed));
        }
    }

    let mut find_chains = Vec::new();
    for find in &obs.log.finds {
        let chain = walk(&parent, find.branch, &name)?;
        let path_len = chain.len() as u64;
        let novel_on_path = chain.iter().filter(|&&b| admitted[b as usize]).count() as u64;
        if path_len != find.path_len || novel_on_path != find.novel_on_path {
            return Err(Error::ChainContradiction {
                campaign: name,
                branch: find.branch,
                rec_path: find.path_len,
                rec_novel: find.novel_on_path,
                got_path: path_len,
                got_novel: novel_on_path,
            });
        }
        find_chains.push(chain);
    }

    Ok(Chains {
        parent,
        admitted,
        find_chains,
    })
}

/// The chain from the genesis-rooted explore branch down to `branch`, root
/// first. Bounded by the branch count: parents strictly decrease.
fn walk(parent: &[Option<u64>], branch: u64, campaign: &str) -> Result<Vec<u64>> {
    let mut chain = Vec::new();
    let mut cur = Some(branch);
    while let Some(b) = cur {
        let idx = usize::try_from(b).ok().filter(|&i| i < parent.len());
        let idx = idx.ok_or_else(|| Error::Corpus {
            campaign: campaign.to_string(),
            why: format!("chain walk left the campaign at branch {b}"),
        })?;
        chain.push(b);
        cur = parent[idx];
    }
    chain.reverse();
    Ok(chain)
}

/// **Harness-correctness gate** (spec gate 2). Re-key `obs` under the v1
/// control and require that every branch's cell ids equal the ones the campaign
/// recorded, in order and with multiplicity.
///
/// If this fails, the replay is wrong and no candidate score means anything:
/// never tune candidates against a broken replay.
pub fn check_control(obs: &CampaignObs, control: &crate::candidate::Candidate) -> Result<()> {
    for branch_obs in &obs.branches {
        let replayed: Vec<u64> = control
            .key_stream(branch_obs)
            .iter()
            .map(|k| crate::candidate::cell_id_of(k))
            .collect();
        let recorded = &obs.log.events[branch_obs.branch as usize].touched;
        if &replayed != recorded {
            return Err(Error::ControlDiverged {
                campaign: obs.name(),
                branch: branch_obs.branch,
                recorded: recorded.clone(),
                replayed,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The chain walk climbs to the genesis-rooted explore branch and orders
    /// root-first.
    #[test]
    fn walk_returns_the_chain_root_first() {
        let parent = vec![None, Some(0), Some(1), None];
        assert_eq!(walk(&parent, 2, "c").expect("walk"), vec![0, 1, 2]);
        assert_eq!(walk(&parent, 3, "c").expect("walk"), vec![3]);
        assert_eq!(walk(&parent, 0, "c").expect("walk"), vec![0]);
        assert!(walk(&parent, 9, "c").is_err(), "off the end is loud");
    }

    /// A find on an explore branch has **no** proper ancestors: it was minted
    /// fresh from genesis, so no cell function could have lost it.
    #[test]
    fn explore_finds_have_no_proper_ancestors() {
        let chains = Chains {
            parent: vec![None, Some(0)],
            admitted: vec![true, false],
            find_chains: vec![vec![1], vec![0, 1]],
        };
        let ancestors = chains.find_ancestors();
        assert_eq!(ancestors[0], Vec::<u64>::new(), "a singleton chain");
        assert_eq!(ancestors[1], vec![0], "the exploit's parent");
    }
}
