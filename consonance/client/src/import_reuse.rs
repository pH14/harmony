// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::cache::{ANCHOR_DEPTH, CommittedExtent, segments::BackingWitness};
use std::collections::VecDeque;

pub(crate) const REFERENCE_LIMIT: usize = 2 * ANCHOR_DEPTH as usize;
const RECENT_LIMIT: usize = 2 * REFERENCE_LIMIT;

#[derive(Debug, Default)]
pub(crate) struct ImportReuse {
    recent: VecDeque<BackingWitness>,
    accepted_imports: u64,
    measured_imports: u64,
    extent_references: u64,
    backing_occurrences: u64,
    duplicate_references: u64,
    recent_backing_hits: u64,
    recent_backing_misses: u64,
    recent_window_evictions: u64,
    unmeasured_overlimit: u64,
}

impl ImportReuse {
    pub(crate) fn accepted(&mut self, chain: &[CommittedExtent]) {
        self.accepted_imports = self.accepted_imports.saturating_add(1);
        if chain.len() > REFERENCE_LIMIT {
            self.unmeasured_overlimit = self.unmeasured_overlimit.saturating_add(1);
            return;
        }
        let mut distinct: Vec<BackingWitness> = Vec::with_capacity(chain.len());
        for extent in chain {
            let witness = extent.backing_witness();
            if !distinct.iter().any(|prior| prior.same_backing(&witness)) {
                distinct.push(witness);
            }
        }
        let hits = distinct
            .iter()
            .filter(|witness| self.recent.iter().any(|prior| prior.same_backing(witness)))
            .count();
        self.measured_imports = self.measured_imports.saturating_add(1);
        self.extent_references = self.extent_references.saturating_add(chain.len() as u64);
        self.backing_occurrences = self
            .backing_occurrences
            .saturating_add(distinct.len() as u64);
        self.duplicate_references = self
            .duplicate_references
            .saturating_add((chain.len() - distinct.len()) as u64);
        self.recent_backing_hits = self.recent_backing_hits.saturating_add(hits as u64);
        self.recent_backing_misses = self
            .recent_backing_misses
            .saturating_add((distinct.len() - hits) as u64);
        self.recent
            .retain(|prior| !distinct.iter().any(|witness| prior.same_backing(witness)));
        for witness in distinct {
            if self.recent.len() == RECENT_LIMIT {
                self.recent.pop_front();
                self.recent_window_evictions = self.recent_window_evictions.saturating_add(1);
            }
            self.recent.push_back(witness);
        }
    }

    pub(crate) fn counters(&self) -> Vec<(String, u64)> {
        [
            ("accepted_imports", self.accepted_imports),
            ("measured_imports", self.measured_imports),
            ("extent_references", self.extent_references),
            ("backing_occurrences", self.backing_occurrences),
            ("duplicate_references", self.duplicate_references),
            ("recent_backing_hits", self.recent_backing_hits),
            ("recent_backing_misses", self.recent_backing_misses),
            ("recent_window_evictions", self.recent_window_evictions),
            ("unmeasured_overlimit", self.unmeasured_overlimit),
        ]
        .into_iter()
        .map(|(name, value)| (format!("cache_import.parent.{name}"), value))
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::PageSegments;

    fn separate_extent() -> CommittedExtent {
        PageSegments::new(4096, usize::MAX)
            .allocate(4096)
            .unwrap()
            .commit()
    }

    fn value(reuse: &ImportReuse, name: &str) -> u64 {
        reuse
            .counters()
            .into_iter()
            .find(|(key, _)| key == &format!("cache_import.parent.{name}"))
            .unwrap()
            .1
    }

    #[test]
    fn allocation_identity_distinguishes_equal_contents_and_allocator_ids() {
        let mut first = PageSegments::new(4 * 4096, usize::MAX);
        let a = first.allocate(4096).unwrap().commit();
        let b = first.allocate(4096).unwrap().commit();
        let c = separate_extent();
        assert_eq!(a.span().0, c.span().0);
        assert_eq!(a.bytes(), c.bytes());
        let mut reuse = ImportReuse::default();
        reuse.accepted(&[a.clone(), b.clone(), a.clone(), b.clone()]);
        assert_eq!(value(&reuse, "extent_references"), 4);
        assert_eq!(value(&reuse, "backing_occurrences"), 1);
        assert_eq!(value(&reuse, "duplicate_references"), 3);
        assert_eq!(value(&reuse, "recent_backing_hits"), 0);
        reuse.accepted(&[a, b, c.clone(), c]);
        assert_eq!(value(&reuse, "extent_references"), 8);
        assert_eq!(value(&reuse, "backing_occurrences"), 3);
        assert_eq!(value(&reuse, "duplicate_references"), 5);
        assert_eq!(value(&reuse, "recent_backing_hits"), 1);
        assert_eq!(value(&reuse, "recent_backing_misses"), 2);
    }

    #[test]
    fn recurrence_uses_prior_window_and_stays_bounded_across_eviction() {
        let extents: Vec<_> = (0..RECENT_LIMIT).map(|_| separate_extent()).collect();
        let mut reuse = ImportReuse::default();
        for extent in &extents {
            reuse.accepted(std::slice::from_ref(extent));
        }
        assert_eq!(reuse.recent.len(), RECENT_LIMIT);
        let fresh = separate_extent();
        reuse.accepted(&[fresh, extents[0].clone(), extents[0].clone()]);
        assert_eq!(value(&reuse, "recent_backing_hits"), 1);
        assert_eq!(
            value(&reuse, "recent_backing_misses"),
            RECENT_LIMIT as u64 + 1
        );
        assert_eq!(value(&reuse, "recent_window_evictions"), 1);
        reuse.accepted(std::slice::from_ref(&extents[1]));
        assert_eq!(
            value(&reuse, "recent_backing_misses"),
            RECENT_LIMIT as u64 + 2
        );
        reuse.accepted(std::slice::from_ref(&extents[0]));
        assert_eq!(value(&reuse, "recent_backing_hits"), 2);
        assert_eq!(reuse.recent.len(), RECENT_LIMIT);
        let mut other_worker = ImportReuse::default();
        other_worker.accepted(std::slice::from_ref(&extents[0]));
        assert_eq!(value(&other_worker, "recent_backing_hits"), 0);
    }

    #[test]
    fn overlimit_layout_is_visible_and_does_not_modify_recent_window() {
        let extent = separate_extent();
        let mut reuse = ImportReuse::default();
        reuse.accepted(&vec![extent.clone(); REFERENCE_LIMIT + 1]);
        assert_eq!(value(&reuse, "accepted_imports"), 1);
        assert_eq!(value(&reuse, "measured_imports"), 0);
        assert_eq!(value(&reuse, "unmeasured_overlimit"), 1);
        assert!(reuse.recent.is_empty());
        reuse.accepted(&[extent]);
        assert_eq!(value(&reuse, "recent_backing_misses"), 1);
        assert_eq!(value(&reuse, "recent_backing_hits"), 0);
    }

    #[test]
    fn weak_window_keeps_no_mapped_extent_and_has_no_reused_identity() {
        let mut segments = PageSegments::new(4096, 1);
        let extent = segments.allocate(4096).unwrap().commit();
        let witness = extent.backing_witness();
        let mut reuse = ImportReuse::default();
        reuse.accepted(std::slice::from_ref(&extent));
        segments.free_committed(extent);
        assert!(
            !segments.full(4096),
            "metric witnesses must not keep mappings alive"
        );
        assert_eq!(reuse.recent.len(), 1);
        let replacement = segments.allocate(4096).unwrap().commit();
        assert!(!witness.same_backing(&replacement.backing_witness()));
        reuse.accepted(&[replacement]);
        assert_eq!(value(&reuse, "recent_backing_hits"), 0);
        assert_eq!(value(&reuse, "recent_backing_misses"), 2);
    }

    #[test]
    fn saturated_counters_do_not_affect_identity_or_window() {
        let mut reuse = ImportReuse {
            accepted_imports: u64::MAX,
            measured_imports: u64::MAX,
            extent_references: u64::MAX,
            backing_occurrences: u64::MAX,
            duplicate_references: u64::MAX,
            recent_backing_hits: u64::MAX,
            recent_backing_misses: u64::MAX,
            recent_window_evictions: u64::MAX,
            unmeasured_overlimit: u64::MAX,
            ..Default::default()
        };
        let extent = separate_extent();
        reuse.accepted(&[extent.clone(), extent.clone()]);
        reuse.accepted(&[extent]);
        assert_eq!(reuse.recent.len(), 1);
        assert!(reuse.counters().iter().all(|(_, value)| *value == u64::MAX));
    }

    #[test]
    #[cfg(all(any(target_os = "linux", target_os = "macos"), not(miri)))]
    fn native_weak_window_releases_mapping_and_descriptor() {
        use std::os::fd::AsRawFd;
        const CHILD: &str = "HARMONY_IMPORT_REUSE_RESOURCE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "import_reuse::tests::native_weak_window_releases_mapping_and_descriptor",
                    "--color",
                    "never",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
            return;
        }
        let mut segments = PageSegments::new(4096, 1);
        let extent = segments.allocate(4096).unwrap().commit();
        let fd = extent.shared_range().fd.as_raw_fd();
        let mut reuse = ImportReuse::default();
        reuse.accepted(std::slice::from_ref(&extent));
        segments.free_committed(extent);
        // SAFETY: F_GETFD queries this isolated process's prior descriptor number without taking ownership or changing it.
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EBADF)
        );
        assert!(!segments.full(4096));
        assert_eq!(reuse.recent.len(), 1);
        let replacement = segments.allocate(4096).unwrap().commit();
        assert_eq!(replacement.shared_range().fd.as_raw_fd(), fd);
        reuse.accepted(&[replacement]);
        assert_eq!(value(&reuse, "recent_backing_hits"), 0);
        assert_eq!(value(&reuse, "recent_backing_misses"), 2);
    }
}
