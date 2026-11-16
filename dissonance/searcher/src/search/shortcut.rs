// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};

use super::campaign::Workload;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Schedule {
    pub(crate) size: usize,
    pub(crate) position: usize,
    pub(crate) variant: usize,
    accepted_in_pass: bool,
    pub(crate) exhausted: bool,
}

pub(crate) enum Fill<A> {
    Delete,
    Replace(Vec<A>),
}

pub(crate) fn proposal<A: Copy>(actions: &[A], indices: &[usize], fill: Fill<A>) -> Vec<A> {
    let start = indices[0];
    let stop = indices[indices.len() - 1] + 1;
    let mut replaced = match fill {
        Fill::Delete => None,
        Fill::Replace(replaced) => Some(replaced.into_iter()),
    };
    let mut proposal = Vec::with_capacity(actions.len() - start);
    let mut window = indices.iter().peekable();
    for (index, action) in actions.iter().enumerate().take(stop).skip(start) {
        if window.next_if(|&&next| next == index).is_none() {
            proposal.push(*action);
        } else if let Some(replacement) = replaced.as_mut().and_then(Iterator::next) {
            proposal.push(replacement);
        }
    }
    proposal.extend_from_slice(&actions[stop..]);
    proposal
}

impl Schedule {
    fn fills<G: Workload + ?Sized>(
        workload: &G,
        run: &G::Run,
        window: &[G::Action],
        primary: bool,
    ) -> Vec<Fill<G::Action>> {
        let mut fills = Vec::new();
        if let [action] = window
            && !primary
        {
            for simpler in workload.simpler_actions(run, action) {
                fills.push(Fill::Replace(vec![simpler]));
            }
        } else {
            let mut changed = false;
            let replaced = window
                .iter()
                .map(
                    |action| match workload.simpler_actions(run, action).first() {
                        Some(simpler) => {
                            changed = true;
                            *simpler
                        }
                        None => *action,
                    },
                )
                .collect::<Vec<_>>();
            if changed {
                fills.push(Fill::Replace(replaced));
            }
        }
        if !(primary && !fills.is_empty()) {
            fills.push(Fill::Delete);
        }
        fills
    }

    pub(crate) fn next<G: Workload + ?Sized>(
        &mut self,
        workload: &G,
        run: &G::Run,
        actions: &[G::Action],
        eligible: &[usize],
        primary: bool,
    ) -> Option<(usize, Vec<G::Action>)> {
        let length = eligible.len();
        loop {
            if self.exhausted || length == 0 {
                self.exhausted = true;
                return None;
            }
            if self.size == 0 {
                self.size = length.div_ceil(2).max(1);
            }
            self.size = self.size.min(length);
            if self.position >= length {
                if self.size == 1 {
                    if self.accepted_in_pass {
                        self.accepted_in_pass = false;
                        self.position = 0;
                        self.variant = 0;
                        continue;
                    }
                    self.exhausted = true;
                    return None;
                }
                self.size = self.size.div_ceil(2);
                self.position = 0;
                self.variant = 0;
                continue;
            }
            let end = (self.position + self.size).min(length);
            let indices = &eligible[self.position..end];
            let window = indices
                .iter()
                .map(|&index| actions[index])
                .collect::<Vec<_>>();
            let fills = Self::fills(workload, run, &window, primary);
            let Some(fill) = fills.into_iter().nth(self.variant) else {
                self.variant = 0;
                self.position = end;
                continue;
            };
            self.variant += 1;
            let start = indices[0];
            let proposal = proposal(actions, indices, fill);
            return Some((start, proposal));
        }
    }

    pub(crate) fn restart_at(&mut self, position: usize) {
        self.position = position;
        self.variant = 0;
        self.accepted_in_pass = true;
        self.exhausted = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_contiguous_window_is_replaced_or_deleted_with_the_tail_kept() {
        let actions = [1, 2, 3, 4, 5];
        assert_eq!(
            proposal(&actions, &[1, 2], Fill::Replace(vec![20, 30])),
            vec![20, 30, 4, 5]
        );
        assert_eq!(proposal(&actions, &[1, 2], Fill::Delete), vec![4, 5]);
    }

    #[test]
    fn a_window_of_scattered_actions_keeps_the_actions_between_them() {
        let actions = [1, 2, 3, 4, 5];
        assert_eq!(
            proposal(&actions, &[1, 3], Fill::Replace(vec![20, 40])),
            vec![20, 3, 40, 5]
        );
        assert_eq!(proposal(&actions, &[1, 3], Fill::Delete), vec![3, 5]);
    }
}
