// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    ops::Range,
};

use machine::{
    Answer, Machine, MachineError, Moment, Reproducer, SnapId, StopConditions, StopReason,
    nes::{WRAM_SIZE, actions_of},
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

thread_local! {
    static RECORDING: RefCell<Option<Trail>> = const { RefCell::new(None) };
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

pub struct Traced<M> {
    inner: M,
    probe: Probe,
    elapsed: u64,
    held: BTreeMap<u64, (u64, usize)>,
    staged: VecDeque<u16>,
}

impl<M: Machine + NesMemory> Traced<M> {
    pub fn new(inner: M, probe: Probe) -> Self {
        Self {
            inner,
            probe,
            elapsed: 0,
            held: BTreeMap::new(),
            staged: VecDeque::new(),
        }
    }

    pub fn inner(&self) -> &M {
        &self.inner
    }

    pub fn inner_mut(&mut self) -> &mut M {
        &mut self.inner
    }

    fn rewind(&mut self, elapsed: u64, trail: usize) {
        self.elapsed = elapsed;
        self.staged.clear();
        with_trail(|state| state.samples.truncate(trail));
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
        self.held.insert(snap.0, (self.elapsed, trail_len()));
        Ok(snap)
    }

    fn drop_snapshot(&mut self, snap: SnapId) -> Result<(), MachineError> {
        self.held.remove(&snap.0);
        self.inner.drop_snapshot(snap)
    }

    fn branch(&mut self, snap: SnapId, env: &Reproducer) -> Result<(), MachineError> {
        let actions = actions_of(env)?;
        self.inner.branch(snap, env)?;
        let (elapsed, trail) = self.held.get(&snap.0).copied().unwrap_or((0, 0));
        self.rewind(elapsed, trail);
        self.staged = actions
            .iter()
            .map(|action| u16::from(action.bounded_hold_frames()))
            .filter(|&frames| frames > 0)
            .collect();
        if let Some(wram) = self.current_wram() {
            self.begin(&wram);
        }
        Ok(())
    }

    fn replay(&mut self, snap: SnapId) -> Result<(), MachineError> {
        self.inner.replay(snap)?;
        let (elapsed, trail) = self.held.get(&snap.0).copied().unwrap_or((0, 0));
        self.rewind(elapsed, trail);
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
        self.held.insert(snap.0, (0, 0));
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
        self.held.insert(snap.0, (0, 0));
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
        self.rewind(0, 0);
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

    fn poke_wram(&mut self, addr: usize, byte: u8) {
        self.inner.poke_wram(addr, byte);
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
