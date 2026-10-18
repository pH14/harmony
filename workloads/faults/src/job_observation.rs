// SPDX-License-Identifier: AGPL-3.0-or-later

use std::cell::{Cell, RefCell};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RestoreSource {
    #[default]
    Unavailable,
    LocalExact,
    LocalAncestor,
    SharedImportExact,
    SharedImportAncestor,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RestoreOutcome {
    #[default]
    NotAttempted,
    Failed,
    Cutoff,
    Succeeded,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RestoreStage {
    #[default]
    Prevalidation,
    Session,
    Lookup,
    Rebuild,
    Replay,
    Certificate,
    Observation,
    Complete,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LookupStage {
    Origin,
    Parent,
    Child,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LookupObservation {
    pub attempts: u64,
    pub import_attempts: u64,
    pub import_completed: u64,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SourceCounts {
    pub local_exact: u64,
    pub local_ancestor: u64,
    pub shared_exact: u64,
    pub shared_ancestor: u64,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RestoreObservation {
    pub source: RestoreSource,
    pub stage: RestoreStage,
    pub outcome: RestoreOutcome,
    pub requested_depth: u64,
    pub selected_depth: u64,
    pub generation: Option<u64>,
    pub rebuild_attempted: u64,
    pub rebuild_completed: u64,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FaultJobObservation {
    pub origin: RestoreObservation,
    pub origin_lookups: LookupObservation,
    pub parent_lookups: LookupObservation,
    pub child_lookups: LookupObservation,
    pub parent_sources: SourceCounts,
    pub child_sources: SourceCounts,
    pub parent_tail_attempted: u64,
    pub parent_tail_completed: u64,
    pub end_generation: Option<u64>,
    pub overflow: bool,
}
thread_local! {
    static ACTIVE: RefCell<Option<FaultJobObservation>> = const { RefCell::new(None) };
    static GENERATION: Cell<Option<u64>> = const { Cell::new(Some(0)) };
}
pub(crate) fn update(f: impl FnOnce(&mut FaultJobObservation)) {
    ACTIVE.with(|s| {
        if let Some(value) = s.borrow_mut().as_mut() {
            f(value);
        }
    });
}
pub(crate) fn increment(value: &mut u64, overflow: &mut bool) {
    match value.checked_add(1) {
        Some(n) => *value = n,
        None => *overflow = true,
    }
}
pub(crate) fn begin() {
    ACTIVE.with(|s| *s.borrow_mut() = Some(FaultJobObservation::default()));
}
pub(crate) fn take() -> Option<FaultJobObservation> {
    ACTIVE.with(|s| s.borrow_mut().take())
}
pub(crate) fn new_generation() {
    GENERATION.with(|g| g.set(g.get().and_then(|n| n.checked_add(1))));
}
pub(crate) fn generation() -> Option<u64> {
    GENERATION.with(|g| g.get().filter(|n| *n != 0))
}
pub(crate) fn origin_start(depth: usize) {
    update(|v| {
        v.origin = RestoreObservation {
            outcome: RestoreOutcome::Failed,
            requested_depth: depth as u64,
            ..Default::default()
        };
    });
}
pub(crate) fn selected(imported: bool, depth: usize) {
    update(|v| {
        v.origin.selected_depth = depth as u64;
        v.origin.generation = generation();
        v.origin.source = match (imported, depth as u64 == v.origin.requested_depth) {
            (false, true) => RestoreSource::LocalExact,
            (false, false) => RestoreSource::LocalAncestor,
            (true, true) => RestoreSource::SharedImportExact,
            (true, false) => RestoreSource::SharedImportAncestor,
        };
    });
}
pub(crate) fn origin_finish(outcome: RestoreOutcome) {
    update(|v| {
        v.origin.outcome = outcome;
        if outcome == RestoreOutcome::Succeeded {
            v.origin.stage = RestoreStage::Complete;
        }
    });
}
pub(crate) fn tail(completed: bool) {
    update(|v| {
        if completed {
            increment(&mut v.parent_tail_completed, &mut v.overflow)
        } else {
            increment(&mut v.parent_tail_attempted, &mut v.overflow)
        }
    });
}

pub(crate) fn stage(value: RestoreStage) {
    update(|v| v.origin.stage = value);
}
#[derive(Clone, Copy)]
pub(crate) enum LookupEvent {
    Attempt,
    ImportAttempt,
    ImportCompleted,
}

pub(crate) fn lookup(stage: LookupStage, event: LookupEvent) {
    update(|v| {
        let selected = match stage {
            LookupStage::Origin => &mut v.origin_lookups,
            LookupStage::Parent => &mut v.parent_lookups,
            LookupStage::Child => &mut v.child_lookups,
        };
        match event {
            LookupEvent::Attempt => increment(&mut selected.attempts, &mut v.overflow),
            LookupEvent::ImportAttempt => increment(&mut selected.import_attempts, &mut v.overflow),
            LookupEvent::ImportCompleted => {
                increment(&mut selected.import_completed, &mut v.overflow)
            }
        }
    });
}
pub(crate) fn source(stage: LookupStage, imported: bool, selected: usize, requested: usize) {
    update(|v| {
        let counts = match stage {
            LookupStage::Origin => return,
            LookupStage::Parent => &mut v.parent_sources,
            LookupStage::Child => &mut v.child_sources,
        };
        let counter = match (imported, selected == requested) {
            (false, true) => &mut counts.local_exact,
            (false, false) => &mut counts.local_ancestor,
            (true, true) => &mut counts.shared_exact,
            (true, false) => &mut counts.shared_ancestor,
        };
        increment(counter, &mut v.overflow);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn job_scope_and_session_generation_are_values_without_retained_owners() {
        assert!(std::mem::size_of::<FaultJobObservation>() <= 256);
        begin();
        new_generation();
        origin_start(8);
        selected(true, 1);
        lookup(LookupStage::Origin, LookupEvent::ImportAttempt);
        tail(false);
        tail(true);
        origin_finish(RestoreOutcome::Succeeded);
        let first = take().unwrap();
        assert_eq!(first.origin.source, RestoreSource::SharedImportAncestor);
        assert_eq!(first.origin.outcome, RestoreOutcome::Succeeded);
        assert_eq!(first.parent_tail_completed, 1);
        assert!(take().is_none());
        begin();
        new_generation();
        origin_start(8);
        selected(false, 8);
        origin_finish(RestoreOutcome::Cutoff);
        let second = take().unwrap();
        assert_eq!(second.origin.source, RestoreSource::LocalExact);
        assert_eq!(second.origin.outcome, RestoreOutcome::Cutoff);
        assert_ne!(first.origin.generation, second.origin.generation);
        assert_eq!(second.origin_lookups.import_attempts, 0);
        assert_eq!(second.parent_tail_completed, 0);
    }
    #[test]
    fn unavailable_and_overflowed_generation_cannot_alias_a_prior_session() {
        let previous = GENERATION.with(|v| v.replace(Some(u64::MAX)));
        new_generation();
        assert_eq!(generation(), None);
        new_generation();
        assert_eq!(generation(), None);
        GENERATION.with(|v| v.set(previous));
    }
}
