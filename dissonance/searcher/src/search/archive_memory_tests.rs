// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    cmp::Ordering,
    collections::VecDeque,
    mem::size_of,
    num::NonZeroU64,
    sync::Arc,
};

use serde::{Deserialize, Serialize};

use super::{ActiveIds, Archive, ArchiveCandidate, ArchiveKey, InputIndex, SharedSnapshotMemory};
use crate::search::{draw::SUFFIX_DOUBLING_LIMIT, rand::RomuDuoJrRand};

struct ThreadHeap;

thread_local! {
    static LIVE_BYTES: Cell<isize> = const { Cell::new(0) };
}

fn malloc_chunk(size: usize) -> isize {
    isize::try_from((size + 8).next_multiple_of(16).max(32)).unwrap_or(isize::MAX)
}

fn track(delta: isize) {
    let _ = LIVE_BYTES.try_with(|live| live.set(live.get() + delta));
}

// SAFETY: every call is forwarded unchanged to System; the per-thread counter is
// const-initialized, has no destructor, allocates nothing and never touches the memory.
unsafe impl GlobalAlloc for ThreadHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        track(malloc_chunk(layout.size()));
        // SAFETY: GlobalAlloc's caller supplies a valid nonzero layout, passed on unchanged.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        track(malloc_chunk(layout.size()));
        // SAFETY: GlobalAlloc's caller supplies a valid nonzero layout, passed on unchanged.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        track(-malloc_chunk(layout.size()));
        // SAFETY: ptr came from System through this allocator with the same layout.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        track(malloc_chunk(new_size) - malloc_chunk(layout.size()));
        // SAFETY: ptr came from System through this allocator with layout, and the
        // caller guarantees new_size is valid for that layout's alignment.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static THREAD_HEAP: ThreadHeap = ThreadHeap;

fn live_bytes() -> isize {
    LIVE_BYTES.with(Cell::get)
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
struct Chord {
    buttons: u8,
    hold: u8,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
struct LevelKey {
    band: i32,
    cell: i32,
    slot: i32,
    frames: u32,
}

impl ArchiveKey for LevelKey {
    type Place = (u8, i32, i32, u8, bool, bool, bool, u8);
    type Progress = (u8, i32);
    type Identity = (u8, u8, i32);
    type Lineage = ();

    fn place(self) -> Self::Place {
        (0, self.cell, 0, 0, false, false, false, 0)
    }

    fn progress(self) -> Self::Progress {
        (0, self.band)
    }

    fn identity(self) -> Self::Identity {
        (0, 0, self.slot)
    }

    fn capacity() -> usize {
        1
    }

    fn preferences() -> usize {
        1
    }

    fn preference_cmp(self, _preference: usize, other: Self) -> Ordering {
        other.frames.cmp(&self.frames)
    }

    fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
        self
    }

    fn record(_lineage: &mut Self::Lineage, _key: Self) {}
}

struct HistoryNode {
    parent: Option<Arc<HistoryNode>>,
    depth: u8,
}

#[derive(Clone)]
struct Snapshot {
    state: Vec<u8>,
    history: Option<Arc<HistoryNode>>,
}

type LevelArchive = Archive<Chord, LevelKey, (), Snapshot>;

const SNAPSHOT_BYTES: usize = 96;

const HISTORY_DEPTH: u8 = 64;

fn snapshot_charge(snapshot: &Snapshot) -> usize {
    size_of::<Snapshot>() + usize::try_from(malloc_chunk(snapshot.state.len())).unwrap()
}

fn shared_history(snapshot: &Snapshot, visit: &mut dyn FnMut(usize, usize) -> bool) {
    let mut node = snapshot.history.as_ref();
    while let Some(current) = node {
        if !visit(
            Arc::as_ptr(current) as usize,
            size_of::<(usize, usize, HistoryNode)>(),
        ) {
            break;
        }
        node = current.parent.as_ref();
    }
}

fn extend_history(archive: &LevelArchive, parent: Option<usize>) -> Arc<HistoryNode> {
    let parent = parent
        .and_then(|parent| archive.entries[parent].snapshot.as_ref())
        .and_then(|snapshot| snapshot.history.clone())
        .filter(|node| node.depth < HISTORY_DEPTH);
    Arc::new(HistoryNode {
        depth: parent.as_ref().map_or(0, |node| node.depth + 1),
        parent,
    })
}

fn below(rand: &mut RomuDuoJrRand, bound: u64) -> u64 {
    rand.below_u64(NonZeroU64::new(bound).unwrap())
}

struct Admissions {
    rand: RomuDuoJrRand,
    slots_per_cell: i32,
    shared: bool,
    next: i32,
    execution: u64,
}

impl Admissions {
    fn admit(&mut self, archive: &mut LevelArchive, entries: usize) {
        let rand = &mut self.rand;
        let mut admitted = 0_usize;
        while admitted < entries {
            let parent = if archive.entries.is_empty() {
                None
            } else {
                archive.prepare_selection();
                let (id, draw) = archive.select_parent(rand).unwrap();
                archive.record_selection(id, &draw);
                Some(id)
            };
            let length = 1 + usize::try_from(below(rand, 12)).unwrap();
            let suffix: Vec<Chord> = (0..length)
                .map(|_| Chord {
                    buttons: u8::try_from(below(rand, 4)).unwrap(),
                    hold: 1 + u8::try_from(below(rand, 3)).unwrap(),
                })
                .collect();
            let position = if self.next > 0 && below(rand, 8) == 0 {
                i32::try_from(below(rand, u64::try_from(self.next).unwrap())).unwrap()
            } else {
                self.next += 1;
                self.next - 1
            };
            let key = LevelKey {
                band: position / 64,
                cell: position / self.slots_per_cell,
                slot: position % self.slots_per_cell,
                frames: u32::try_from(below(rand, 1 << 20)).unwrap(),
            };
            self.execution += 1;
            let history = self.shared.then(|| extend_history(archive, parent));
            let inserted = archive
                .insert(
                    parent,
                    self.execution,
                    ArchiveCandidate {
                        suffix,
                        key,
                        milestones: (),
                    },
                    Snapshot {
                        state: vec![0_u8; SNAPSHOT_BYTES],
                        history,
                    },
                )
                .unwrap();
            if let Some(parent) = parent {
                archive.record_selection_outcome(parent, inserted.is_some());
            }
            if inserted.is_some_and(|id| id + 1 == archive.entries.len()) {
                admitted += 1;
            }
            archive.maintain_memory_budget().unwrap();
        }
        archive.prepare_selection();
    }
}

#[derive(Debug)]
struct Usage {
    entries: usize,
    inputs: usize,
    cells: usize,
    continuations: usize,
    snapshots: usize,
    total: usize,
}

fn charged(archive: &LevelArchive) -> Usage {
    Usage {
        entries: archive.entry_metadata_memory_bytes(),
        inputs: archive.input_index_memory_bytes(),
        cells: archive.cell_memory_bytes(),
        continuations: archive
            .continuations
            .as_ref()
            .map_or(0, |bank| bank.memory_bytes()),
        snapshots: archive.resident_snapshot_bytes() + archive.snapshot_header_memory_bytes(),
        total: archive.resident_memory_bytes(),
    }
}

fn measured(mut archive: LevelArchive, baseline: isize) -> Usage {
    let total = usize::try_from(live_bytes() - baseline).unwrap();
    let mut freed = |release: &mut dyn FnMut(&mut LevelArchive)| {
        let before = live_bytes();
        release(&mut archive);
        usize::try_from(before - live_bytes()).unwrap()
    };
    let snapshots = freed(&mut |archive| {
        for entry in &mut archive.entries {
            entry.snapshot = None;
        }
        archive.shared_snapshot_memory = SharedSnapshotMemory::default();
    });
    let inputs = freed(&mut |archive| {
        for entry in &mut archive.entries {
            entry.input_suffix = Vec::new();
        }
        archive.input_index = InputIndex::default();
    });
    let cells = freed(&mut |archive| {
        archive.slots.clear();
        archive.cells.clear();
        archive.seen_slots.clear();
        archive.tiers.clear();
        archive.active_ids = ActiveIds::default();
    });
    let continuations = freed(&mut |archive| archive.continuations = None);
    let entries = freed(&mut |archive| {
        archive.entries = Vec::new();
        archive.id_to_index.clear();
        archive.active = Vec::new();
        archive.selected = Vec::new();
        archive.productive = Vec::new();
        archive.opened_cell = Vec::new();
        archive.opened_slot = Vec::new();
        archive.opened_novel_slot = Vec::new();
        archive.cost_in_group = Vec::new();
        archive.replacement_preferences = Vec::new();
        archive.in_place_lengths = Vec::new();
        archive.lineages = Vec::new();
        archive.deepest_leaf = Vec::new();
        archive.parent_index = Vec::new();
        archive.snapshot_selectable = Vec::new();
        archive.keyframe = Vec::new();
        archive.keyframe_id = Vec::new();
        archive.keyframe_dependents = Vec::new();
        archive.referenced = Vec::new();
        archive.resident_snapshot_order = VecDeque::new();
        archive.landed.clear();
        archive.metadata_pins.clear();
        archive.inflight_snapshot_pins.clear();
        archive.inflight_snapshot_charges.clear();
    });
    drop(archive);
    assert_eq!(
        live_bytes(),
        baseline,
        "the archive released all of its heap"
    );
    Usage {
        entries,
        inputs,
        cells,
        continuations,
        snapshots,
        total,
    }
}

fn assert_charges_cover_heap(slots_per_cell: i32, bounded: bool, shared: bool) {
    let baseline = live_bytes();
    let mut admissions = Admissions {
        rand: RomuDuoJrRand::with_seed(0x729),
        slots_per_cell,
        shared,
        next: 0,
        execution: 0,
    };
    let mut archive = LevelArchive::new(|chord| u64::from(chord.hold));
    archive.set_memory_budget(usize::MAX, snapshot_charge);
    if shared {
        archive.set_snapshot_shared_allocations(shared_history);
    }
    archive.enable_continuations(usize::from(SUFFIX_DOUBLING_LIMIT));
    admissions.admit(&mut archive, 10_000);
    if bounded {
        archive.memory_limit = Some(archive.resident_memory_bytes() / 2);
        admissions.admit(&mut archive, 4_000);
        assert!(archive.entry_drops() > 0 && archive.history_compactions() > 0);
    }
    let charged = charged(&archive);
    let measured = measured(archive, baseline);
    assert!(
        measured.entries <= charged.entries
            && measured.inputs <= charged.inputs
            && measured.cells <= charged.cells
            && measured.continuations <= charged.continuations
            && measured.snapshots <= charged.snapshots
            && measured.total <= charged.total,
        "heap {measured:?} exceeds charges {charged:?}"
    );
}

#[test]
fn charges_cover_a_growing_archive_with_a_cell_per_entry() {
    assert_charges_cover_heap(1, false, false);
}

#[test]
fn charges_cover_a_growing_archive_with_shared_cells() {
    assert_charges_cover_heap(8, false, false);
}

#[test]
fn charges_cover_a_bounded_archive_with_a_cell_per_entry() {
    assert_charges_cover_heap(1, true, false);
}

#[test]
fn charges_cover_a_bounded_archive_with_shared_cells() {
    assert_charges_cover_heap(8, true, false);
}

#[test]
fn charges_cover_a_growing_archive_with_shared_histories() {
    assert_charges_cover_heap(8, false, true);
}

#[test]
fn charges_cover_a_bounded_archive_with_shared_histories() {
    assert_charges_cover_heap(8, true, true);
}
