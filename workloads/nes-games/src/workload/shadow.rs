// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::BTreeMap,
    error::Error,
    fmt::Debug,
    hash::{DefaultHasher, Hash, Hasher},
    sync::{Arc, Mutex},
};

use machine::nes::WRAM_SIZE;

use super::{Game, Lowered};
use crate::search::{
    archive::ArchiveKey,
    assertion::{ShadowCursor, ShadowReport, ShadowTable},
};

trait Lane<H>: Send + Sync {
    fn fork(&self) -> Box<dyn Lane<H>>;
    fn start(&self, ram: &[u8]);
    fn advance(&self, frames: &[[u8; WRAM_SIZE]]);
    fn saved(&self, tag: u64);
    fn restored(&self, tag: u64, ram: &[u8]);
    fn observe(&self, hand: Option<H>) -> Option<H>;
}

pub struct ShadowHook<H>(Box<dyn Lane<H>>);

impl<H> Clone for ShadowHook<H> {
    fn clone(&self) -> Self {
        Self(self.0.fork())
    }
}

impl<H> Debug for ShadowHook<H> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ShadowHook")
    }
}

impl<H> ShadowHook<H> {
    pub fn start(&self, ram: &[u8]) {
        self.0.start(ram);
    }

    pub fn advance(&self, frames: &[[u8; WRAM_SIZE]]) {
        self.0.advance(frames);
    }

    pub fn saved(&self, tag: u64) {
        self.0.saved(tag);
    }

    pub fn restored(&self, tag: u64, ram: &[u8]) {
        self.0.restored(tag, ram);
    }

    pub fn observe(&self, hand: Option<H>) -> Option<H> {
        self.0.observe(hand)
    }
}

#[must_use]
pub fn snapshot_tag(emulator_state: &[u8], frame_count: u64) -> u64 {
    let mut hasher = DefaultHasher::new();
    emulator_state.hash(&mut hasher);
    frame_count.hash(&mut hasher);
    hasher.finish()
}

struct Position<S, H: ArchiveKey> {
    state: S,
    cursor: ShadowCursor<H>,
}

impl<S: Clone, H: ArchiveKey> Clone for Position<S, H> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            cursor: self.cursor.clone(),
        }
    }
}

struct Current<S, H: ArchiveKey> {
    position: Option<Position<S, H>>,
    pending: Option<u64>,
}

struct Threaded<G: Game, H: ArchiveKey> {
    shadow: Arc<Shadow<G, H>>,
    current: Mutex<Current<G::State, H>>,
}

impl<G: Game, H: ArchiveKey> Threaded<G, H> {
    fn new(shadow: Arc<Shadow<G, H>>) -> Self {
        Self {
            shadow,
            current: Mutex::new(Current {
                position: None,
                pending: None,
            }),
        }
    }
}

impl<G: Game, H: ArchiveKey + Debug + Send + Sync + 'static> Threaded<G, H>
where
    H::Lineage: Send + Sync,
{
    fn arrive(
        &self,
        previous: Option<&G::State>,
        ram: &[u8],
        cursor: &mut ShadowCursor<H>,
    ) -> G::State {
        let lowered = &self.shadow.lowered;
        let state = lowered.game.decode(previous, ram);
        cursor.observe(&lowered.compiled, &lowered.record(&state).0);
        state
    }

    fn begin(&self, ram: &[u8]) -> Position<G::State, H> {
        let mut cursor = ShadowCursor::default();
        let state = self.arrive(None, ram, &mut cursor);
        Position { state, cursor }
    }
}

impl<G: Game, H: ArchiveKey + Debug + Send + Sync + 'static> Lane<H> for Threaded<G, H>
where
    H::Lineage: Send + Sync,
{
    fn fork(&self) -> Box<dyn Lane<H>> {
        Box::new(Self::new(Arc::clone(&self.shadow)))
    }

    fn start(&self, ram: &[u8]) {
        let position = self.begin(ram);
        if let Ok(mut current) = self.current.lock() {
            current.position = Some(position);
            current.pending = None;
        }
    }

    fn advance(&self, frames: &[[u8; WRAM_SIZE]]) {
        let Ok(mut current) = self.current.lock() else {
            return;
        };
        current.pending = None;
        if let Some(position) = current.position.as_mut() {
            for wram in frames {
                position.state = self.arrive(Some(&position.state), wram, &mut position.cursor);
            }
        }
    }

    fn saved(&self, tag: u64) {
        let Ok(mut current) = self.current.lock() else {
            return;
        };
        if let (Some(position), Ok(mut saved)) =
            (current.position.as_ref(), self.shadow.saved.lock())
        {
            saved.insert(tag, position.clone());
        }
        current.pending = Some(tag);
    }

    fn restored(&self, tag: u64, ram: &[u8]) {
        let found = self
            .shadow
            .saved
            .lock()
            .ok()
            .and_then(|saved| saved.get(&tag).cloned());
        let position = found.unwrap_or_else(|| {
            self.shadow.table.unknown_restore();
            self.begin(ram)
        });
        if let Ok(mut current) = self.current.lock() {
            current.position = Some(position);
            current.pending = None;
        }
    }

    fn observe(&self, hand: Option<H>) -> Option<H> {
        let Ok(mut current) = self.current.lock() else {
            return None;
        };
        let pending = current.pending.take();
        let Some(position) = current.position.as_mut() else {
            self.shadow.table.unknown_restore();
            return None;
        };
        let completed = position.cursor.pair(&self.shadow.table, hand);
        if let (Some(tag), Ok(mut saved)) = (pending, self.shadow.saved.lock()) {
            saved.insert(tag, position.clone());
        }
        completed
    }
}

pub struct Shadow<G: Game, H: ArchiveKey> {
    lowered: Lowered<G>,
    table: ShadowTable<H>,
    saved: Mutex<BTreeMap<u64, Position<G::State, H>>>,
}

impl<G: Game, H: ArchiveKey + Debug + Send + Sync + 'static> Shadow<G, H>
where
    H::Lineage: Send + Sync,
{
    pub fn new(game: G) -> Result<Arc<Self>, Box<dyn Error>> {
        Ok(Arc::new(Self {
            lowered: Lowered::new(game, G::STRATEGY)?,
            table: ShadowTable::default(),
            saved: Mutex::default(),
        }))
    }

    #[must_use]
    pub fn hook(self: &Arc<Self>) -> ShadowHook<H> {
        ShadowHook(Box::new(Threaded::new(Arc::clone(self))))
    }

    pub fn report(&self) -> Result<ShadowReport, Box<dyn Error>> {
        self.table.report(&self.lowered.compiled)
    }
}

#[cfg(test)]
mod tests {
    use machine::{
        Machine, SnapId, StopConditions,
        nes::{ButtonChord, reproducer},
    };

    use super::*;
    use crate::workload::{
        fixture::{LEFT, Ledge, LedgeKey, RIGHT, Stepper, WARP},
        target::read_wram,
    };

    struct Hand {
        machine: Stepper,
        at: SnapId,
        frames: u64,
        hook: ShadowHook<LedgeKey>,
    }

    impl Hand {
        fn new(hook: ShadowHook<LedgeKey>) -> Self {
            let mut machine = Stepper::default();
            let at = machine.snapshot().unwrap();
            hook.start(&read_wram(&machine).unwrap());
            let mut hand = Self {
                machine,
                at,
                frames: 0,
                hook,
            };
            hand.key();
            hand
        }

        fn key(&mut self) -> Option<LedgeKey> {
            let spot = Ledge.decode(None, &read_wram(&self.machine).unwrap());
            self.hook.observe(Some(LedgeKey::raw(spot)))
        }

        fn apply(&mut self, buttons: u8, hold: u8) {
            let chord = ButtonChord::new(buttons, hold);
            self.machine
                .branch(self.at, &reproducer(std::slice::from_ref(&chord)))
                .unwrap();
            self.machine.run(StopConditions::default(), None).unwrap();
            self.hook.advance(self.machine.frames());
            self.frames += u64::from(hold);
            self.at = self.machine.snapshot().unwrap();
        }

        fn walk(&mut self, chords: &[(u8, u8)]) -> Vec<Option<LedgeKey>> {
            chords
                .iter()
                .map(|&(buttons, hold)| {
                    self.apply(buttons, hold);
                    self.key()
                })
                .collect()
        }

        fn save(&mut self) -> (Vec<u8>, u64) {
            let state = self.machine.export(self.at, None).unwrap();
            self.hook.saved(snapshot_tag(&state, self.frames));
            (state, self.frames)
        }

        fn restore(&mut self, (state, frames): &(Vec<u8>, u64)) {
            self.at = self.machine.import(state).unwrap();
            self.machine.replay(self.at).unwrap();
            self.frames = *frames;
            self.hook.restored(
                snapshot_tag(state, *frames),
                &read_wram(&self.machine).unwrap(),
            );
        }
    }

    #[test]
    fn the_shadow_threads_the_accumulator_and_the_hand_lineage_across_a_save_and_restore() {
        let shadow = Shadow::<Ledge, LedgeKey>::new(Ledge).unwrap();
        let mut hand = Hand::new(shadow.hook());
        hand.apply(RIGHT, 3);
        hand.key();
        hand.apply(RIGHT, 2);
        let saved = hand.save();
        assert_eq!(
            hand.key(),
            Some(LedgeKey {
                stage: 1,
                room: 0,
                band: 5
            })
        );
        let on = [(LEFT, 1), (RIGHT, 1), (WARP, 1), (RIGHT, 2)];
        let first = hand.walk(&on);
        hand.restore(&saved);
        assert_eq!(hand.walk(&on), first);
        assert_eq!(
            first[2],
            Some(LedgeKey {
                stage: 1,
                room: 1,
                band: 0
            })
        );
        let report = shadow.report().unwrap();
        assert_eq!(report.disagreements.total(), 0, "{:?}", report.examples);
        assert_eq!(report.unknown_restores, 0);
        assert!(report.lineage_changes > 0);
        assert_eq!(report.keyed_states, 11);
    }
}
