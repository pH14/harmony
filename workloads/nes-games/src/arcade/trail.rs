// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    ops::Range,
};

use machine::{
    Answer, Machine, MachineError, Moment, Reproducer, SnapId, StopConditions, StopReason,
    nes::{ButtonChord, WRAM_SIZE, actions_of},
};

use crate::nes_backend::{NesBackend, NesMemory};

pub const TRAIL_STRIDE: u16 = 4;

#[derive(Clone, Copy)]
pub struct Probe {
    pub motion: fn(&[u8; WRAM_SIZE]) -> [u16; 4],
    pub cleared: fn(&[u8]) -> u8,
    pub memory_bytes: usize,
}

#[derive(Default)]
pub struct Trail {
    skip: u64,
    pub samples: Vec<u16>,
    pub start_cleared: u8,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ControllerPath {
    pub actions: Vec<ButtonChord>,
    pub from_power_on: bool,
}

thread_local! {
    static RECORDING: RefCell<Option<Trail>> = const { RefCell::new(None) };
    static PATH: RefCell<Option<Option<ControllerPath>>> = const { RefCell::new(None) };
}

pub fn record_path<T>(run: impl FnOnce() -> T) -> (T, Option<ControllerPath>) {
    PATH.set(Some(None));
    let value = run();
    (value, PATH.take().flatten())
}

pub fn record<T>(skip: u64, run: impl FnOnce() -> T) -> (T, Trail) {
    RECORDING.set(Some(Trail {
        skip,
        ..Trail::default()
    }));
    let value = run();
    (value, RECORDING.take().unwrap_or_default())
}

fn recording() -> bool {
    RECORDING.with_borrow(Option::is_some)
}

fn with_trail(update: impl FnOnce(&mut Trail)) {
    RECORDING.with_borrow_mut(|trail| {
        if let Some(trail) = trail {
            update(trail);
        }
    });
}

fn trail_len() -> usize {
    RECORDING.with_borrow(|trail| trail.as_ref().map_or(0, |trail| trail.samples.len()))
}

#[derive(Clone, Copy)]
struct Held {
    elapsed: u64,
    trail: usize,
    path: usize,
    from_power_on: bool,
}

const IMPORTED: Held = Held {
    elapsed: 0,
    trail: 0,
    path: 0,
    from_power_on: false,
};

pub struct Traced<M> {
    inner: M,
    probe: Probe,
    elapsed: u64,
    held: BTreeMap<u64, Held>,
    staged: VecDeque<u16>,
    pending: Vec<ButtonChord>,
    path: Option<ControllerPath>,
}

impl<M> Drop for Traced<M> {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            PATH.with_borrow_mut(|recorded| {
                if let Some(recorded) = recorded {
                    *recorded = Some(path);
                }
            });
        }
    }
}

impl<M: Machine + NesMemory> Traced<M> {
    pub fn new(inner: M, probe: Probe) -> Self {
        Self {
            inner,
            probe,
            elapsed: 0,
            held: BTreeMap::new(),
            staged: VecDeque::new(),
            pending: Vec::new(),
            path: PATH.with_borrow(Option::is_some).then(|| ControllerPath {
                actions: Vec::new(),
                from_power_on: true,
            }),
        }
    }

    pub fn inner(&self) -> &M {
        &self.inner
    }

    pub fn inner_mut(&mut self) -> &mut M {
        &mut self.inner
    }

    fn rewind(&mut self, held: Held) {
        self.elapsed = held.elapsed;
        self.staged.clear();
        self.pending.clear();
        with_trail(|state| state.samples.truncate(held.trail));
        if let Some(path) = self.path.as_mut() {
            path.actions.truncate(held.path);
            path.from_power_on = held.from_power_on;
        }
    }

    fn held(&self, snap: SnapId) -> Held {
        self.held.get(&snap.0).copied().unwrap_or(IMPORTED)
    }

    fn begin(&self, wram: &[u8; WRAM_SIZE]) {
        let (elapsed, probe) = (self.elapsed, self.probe);
        let needed = RECORDING.with_borrow(|trail| {
            trail
                .as_ref()
                .is_some_and(|trail| elapsed == trail.skip && trail.samples.is_empty())
        });
        if !needed {
            return;
        }
        let mut memory = wram.to_vec();
        if probe.memory_bytes > WRAM_SIZE {
            memory.extend(self.inner.read_save_ram().unwrap_or_default());
            memory.resize(probe.memory_bytes, 0);
        }
        with_trail(|trail| {
            trail.samples.push(0);
            trail.samples.extend((probe.motion)(wram));
            trail.start_cleared = (probe.cleared)(&memory);
        });
    }

    fn current_wram(&self) -> Option<[u8; WRAM_SIZE]> {
        self.inner.read_wram().ok()
    }

    fn sample(&mut self, start: Option<[u8; WRAM_SIZE]>) {
        let frames = self.inner.frames().len();
        if !recording() {
            self.elapsed += frames as u64;
            for _ in 0..frames {
                if let Some(head) = self.staged.front_mut() {
                    *head = head.saturating_sub(1);
                    if *head == 0 {
                        self.staged.pop_front();
                    }
                }
            }
            return;
        }
        if let Some(start) = start {
            self.begin(&start);
        }
        let mut within = 0_u16;
        for index in 0..frames {
            let wram = self.inner.frames()[index];
            if within == 0 && index > 0 {
                self.begin(&self.inner.frames()[index - 1]);
            }
            self.elapsed += 1;
            within += 1;
            let ended = match self.staged.front_mut() {
                Some(head) => {
                    *head = head.saturating_sub(1);
                    *head == 0
                }
                None => index + 1 == frames,
            };
            if ended {
                self.staged.pop_front();
            }
            if within.is_multiple_of(TRAIL_STRIDE) || ended || index + 1 == frames {
                let (elapsed, motion) = (self.elapsed, self.probe.motion);
                with_trail(|trail| {
                    if elapsed > trail.skip && !trail.samples.is_empty() {
                        trail
                            .samples
                            .push(u16::try_from(elapsed - trail.skip).unwrap_or(u16::MAX));
                        trail.samples.extend(motion(&wram));
                    }
                });
            }
            if ended {
                within = 0;
            }
        }
    }
}

impl<M: Machine + NesMemory> Machine for Traced<M> {
    type Portable = M::Portable;

    fn snapshot(&mut self) -> Result<SnapId, MachineError> {
        let snap = self.inner.snapshot()?;
        let held = Held {
            elapsed: self.elapsed,
            trail: trail_len(),
            path: self.path.as_ref().map_or(0, |path| path.actions.len()),
            from_power_on: self.path.as_ref().is_some_and(|path| path.from_power_on),
        };
        self.held.insert(snap.0, held);
        Ok(snap)
    }

    fn drop_snapshot(&mut self, snap: SnapId) -> Result<(), MachineError> {
        self.held.remove(&snap.0);
        self.inner.drop_snapshot(snap)
    }

    fn branch(&mut self, snap: SnapId, env: &Reproducer) -> Result<(), MachineError> {
        let actions = actions_of(env)?;
        self.inner.branch(snap, env)?;
        self.rewind(self.held(snap));
        self.staged = actions
            .iter()
            .map(|action| u16::from(action.bounded_hold_frames()))
            .filter(|&frames| frames > 0)
            .collect();
        if self.path.is_some() {
            self.pending = actions
                .iter()
                .map(|action| ButtonChord::new(action.buttons, action.bounded_hold_frames()))
                .collect();
        }
        if let Some(wram) = self.current_wram() {
            self.begin(&wram);
        }
        Ok(())
    }

    fn replay(&mut self, snap: SnapId) -> Result<(), MachineError> {
        self.inner.replay(snap)?;
        self.rewind(self.held(snap));
        if let Some(wram) = self.current_wram() {
            self.begin(&wram);
        }
        Ok(())
    }

    fn run(
        &mut self,
        until: StopConditions,
        resolve: Option<&Answer>,
    ) -> Result<StopReason, MachineError> {
        let start = recording().then(|| self.current_wram()).flatten();
        let stopped = self.inner.run(until, resolve)?;
        if let Some(path) = self.path.as_mut() {
            path.actions.append(&mut self.pending);
        }
        self.sample(start);
        Ok(stopped)
    }

    fn read(&self, addr: u64, len: u32) -> Result<Vec<u8>, MachineError> {
        self.inner.read(addr, len)
    }

    fn export(
        &mut self,
        snap: SnapId,
        base: Option<&Self::Portable>,
    ) -> Result<Self::Portable, MachineError> {
        self.inner.export(snap, base)
    }

    fn import(&mut self, portable: &Self::Portable) -> Result<SnapId, MachineError> {
        let snap = self.inner.import(portable)?;
        self.held.insert(snap.0, IMPORTED);
        Ok(snap)
    }

    fn portable_memory_charge(portable: &Self::Portable) -> usize {
        M::portable_memory_charge(portable)
    }

    fn now(&self) -> Moment {
        self.inner.now()
    }

    fn frames(&self) -> &[[u8; WRAM_SIZE]] {
        self.inner.frames()
    }
}

impl<M: NesBackend<Vec<u8>> + NesMemory> NesBackend<Vec<u8>> for Traced<M> {
    fn export_nes(
        &mut self,
        snapshot: SnapId,
        base: Option<&Vec<u8>>,
    ) -> Result<Vec<u8>, MachineError> {
        self.inner.export_nes(snapshot, base)
    }

    fn import_nes(&mut self, portable: &Vec<u8>) -> Result<SnapId, MachineError> {
        let snap = self.inner.import_nes(portable)?;
        self.held.insert(snap.0, IMPORTED);
        Ok(snap)
    }

    fn release_exported(&mut self, snapshot: SnapId) -> Result<(), MachineError> {
        self.held.remove(&snapshot.0);
        self.inner.release_exported(snapshot)
    }

    fn keep_run_capture(&mut self, frames: usize) {
        self.inner.keep_run_capture(frames);
    }

    fn capture_nes(&mut self, base: Option<&Vec<u8>>) -> Result<Vec<u8>, MachineError> {
        self.inner.capture_nes(base)
    }

    fn restore_nes(&mut self, state: &Vec<u8>) -> Result<(), MachineError> {
        self.inner.restore_nes(state)?;
        self.rewind(IMPORTED);
        if let Some(wram) = self.current_wram() {
            self.begin(&wram);
        }
        Ok(())
    }
}

impl<M: NesMemory> NesMemory for Traced<M> {
    fn read_wram(&self) -> Result<[u8; WRAM_SIZE], MachineError> {
        self.inner.read_wram()
    }

    fn poke_wram(&mut self, addr: usize, byte: u8) -> Result<(), MachineError> {
        self.inner.poke_wram(addr, byte)
    }

    fn capture_save_ram_per_frame(&mut self, range: Range<usize>) {
        self.inner.capture_save_ram_per_frame(range);
    }

    fn save_ram_frames(&self) -> &[u8] {
        self.inner.save_ram_frames()
    }

    fn read_save_ram(&self) -> Result<Vec<u8>, MachineError> {
        self.inner.read_save_ram()
    }

    fn write_save_ram(&mut self, offset: usize, bytes: &[u8]) -> Result<(), MachineError> {
        self.inner.write_save_ram(offset, bytes)
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, ops::Range};

    use machine::{
        Answer, Machine, MachineError, Moment, Reproducer, SnapId, StopConditions, StopReason,
        nes::{ButtonChord, WRAM_SIZE, actions_of, reproducer},
    };

    use super::{ControllerPath, Probe, Traced, record, record_path};
    use crate::nes_backend::NesMemory;

    #[derive(Default)]
    struct Walker {
        x: u8,
        held: BTreeMap<u64, u8>,
        next: u64,
        staged: Vec<ButtonChord>,
        frames: Vec<[u8; WRAM_SIZE]>,
    }

    impl Walker {
        fn wram(&self) -> [u8; WRAM_SIZE] {
            let mut wram = [0; WRAM_SIZE];
            wram[0] = self.x;
            wram
        }
    }

    impl Machine for Walker {
        type Portable = u8;

        fn snapshot(&mut self) -> Result<SnapId, MachineError> {
            self.next += 1;
            self.held.insert(self.next, self.x);
            Ok(SnapId(self.next))
        }

        fn drop_snapshot(&mut self, snap: SnapId) -> Result<(), MachineError> {
            self.held.remove(&snap.0);
            Ok(())
        }

        fn branch(&mut self, snap: SnapId, env: &Reproducer) -> Result<(), MachineError> {
            self.replay(snap)?;
            self.staged = actions_of(env)?;
            Ok(())
        }

        fn replay(&mut self, snap: SnapId) -> Result<(), MachineError> {
            self.x = *self
                .held
                .get(&snap.0)
                .ok_or(MachineError::UnknownSnapshot)?;
            Ok(())
        }

        fn run(
            &mut self,
            _until: StopConditions,
            _resolve: Option<&Answer>,
        ) -> Result<StopReason, MachineError> {
            self.frames.clear();
            for chord in std::mem::take(&mut self.staged) {
                for _ in 0..chord.bounded_hold_frames() {
                    self.x = self.x.wrapping_add(chord.buttons & 1);
                    self.frames.push(self.wram());
                }
            }
            Ok(StopReason::Quiescent { vtime: Moment(0) })
        }

        fn read(&self, _addr: u64, _len: u32) -> Result<Vec<u8>, MachineError> {
            Ok(Vec::new())
        }

        fn export(&mut self, snap: SnapId, _base: Option<&u8>) -> Result<u8, MachineError> {
            self.held
                .get(&snap.0)
                .copied()
                .ok_or(MachineError::UnknownSnapshot)
        }

        fn import(&mut self, portable: &u8) -> Result<SnapId, MachineError> {
            self.next += 1;
            self.held.insert(self.next, *portable);
            Ok(SnapId(self.next))
        }

        fn portable_memory_charge(_portable: &u8) -> usize {
            1
        }

        fn now(&self) -> Moment {
            Moment(0)
        }

        fn frames(&self) -> &[[u8; WRAM_SIZE]] {
            &self.frames
        }
    }

    impl NesMemory for Walker {
        fn read_wram(&self) -> Result<[u8; WRAM_SIZE], MachineError> {
            Ok(self.wram())
        }

        fn poke_wram(&mut self, _addr: usize, _byte: u8) -> Result<(), MachineError> {
            Ok(())
        }

        fn capture_save_ram_per_frame(&mut self, _range: Range<usize>) {}

        fn save_ram_frames(&self) -> &[u8] {
            &[]
        }

        fn read_save_ram(&self) -> Result<Vec<u8>, MachineError> {
            Ok(Vec::new())
        }

        fn write_save_ram(&mut self, _offset: usize, _bytes: &[u8]) -> Result<(), MachineError> {
            Ok(())
        }
    }

    fn probe() -> Probe {
        Probe {
            motion: |wram| [u16::from(wram[0]), 0, 0, 0],
            cleared: |memory| memory[0] / 100,
            memory_bytes: WRAM_SIZE,
        }
    }

    fn walk(machine: &mut Traced<Walker>, from: SnapId, chords: &[ButtonChord]) {
        machine.branch(from, &reproducer(chords)).unwrap();
        machine.run(StopConditions::default(), None).unwrap();
    }

    #[test]
    fn a_trail_samples_every_fourth_frame_and_each_chord_end() {
        let mut machine = Traced::new(Walker::default(), probe());
        let root = machine.snapshot().unwrap();
        let ((), trail) = record(0, || {
            walk(
                &mut machine,
                root,
                &[ButtonChord::new(1, 6), ButtonChord::new(0, 3)],
            );
        });
        assert_eq!(
            trail.samples,
            [0, 0, 0, 0, 0, 4, 4, 0, 0, 0, 6, 6, 0, 0, 0, 9, 6, 0, 0, 0]
        );
        assert_eq!(trail.start_cleared, 0);
    }

    #[test]
    fn a_trail_starts_after_the_replayed_prefix() {
        let mut machine = Traced::new(Walker::default(), probe());
        let root = machine.snapshot().unwrap();
        let ((), trail) = record(2, || {
            walk(
                &mut machine,
                root,
                &[ButtonChord::new(1, 2), ButtonChord::new(1, 4)],
            );
        });
        assert_eq!(trail.samples, [0, 2, 0, 0, 0, 4, 6, 0, 0, 0]);
    }

    #[test]
    fn the_controller_path_follows_restores_and_imports() {
        let ((), path) = record_path(|| {
            let mut machine = Traced::new(Walker::default(), probe());
            let root = machine.snapshot().unwrap();
            walk(&mut machine, root, &[ButtonChord::new(1, 2)]);
            let middle = machine.snapshot().unwrap();
            walk(&mut machine, middle, &[ButtonChord::new(0, 3)]);
            walk(&mut machine, middle, &[ButtonChord::new(1, 5)]);
        });
        assert_eq!(
            path,
            Some(ControllerPath {
                actions: vec![ButtonChord::new(1, 2), ButtonChord::new(1, 5)],
                from_power_on: true,
            })
        );
        let ((), path) = record_path(|| {
            let mut machine = Traced::new(Walker::default(), probe());
            let imported = machine.import(&9).unwrap();
            walk(&mut machine, imported, &[ButtonChord::new(1, 1)]);
        });
        assert_eq!(
            path,
            Some(ControllerPath {
                actions: vec![ButtonChord::new(1, 1)],
                from_power_on: false,
            })
        );
        let mut outside = Traced::new(Walker::default(), probe());
        let root = outside.snapshot().unwrap();
        walk(&mut outside, root, &[ButtonChord::new(1, 1)]);
        assert!(outside.path.is_none());
    }
}
