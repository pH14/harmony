// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::BTreeMap,
    error::Error,
    fmt::{self, Debug},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicU64, Ordering},
    },
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
    fn forced(&self, steps: &[[u8; WRAM_SIZE]]);
    fn saved(&self) -> ShadowToken;
    fn restored(&self, token: Option<&ShadowToken>, ram: &[u8]);
    fn observe(&self, hand: Option<H>) -> Option<H>;
    #[cfg(test)]
    fn key(&self) -> Option<crate::search::assertion::AssertionKey>;
}

static SHADOWS: AtomicU64 = AtomicU64::new(0);

trait Forget: Send + Sync {
    fn forget(&self, index: u64);
}

struct Issued {
    shadow: u64,
    index: u64,
    saved: Weak<dyn Forget>,
}

impl Drop for Issued {
    fn drop(&mut self) {
        if let Some(saved) = self.saved.upgrade() {
            saved.forget(self.index);
        }
    }
}

#[derive(Clone)]
pub struct ShadowToken(Arc<Issued>);

impl ShadowToken {
    fn index_in(&self, shadow: u64) -> Option<u64> {
        (self.0.shadow == shadow).then_some(self.0.index)
    }
}

impl PartialEq for ShadowToken {
    fn eq(&self, other: &Self) -> bool {
        (self.0.shadow, self.0.index) == (other.0.shadow, other.0.index)
    }
}

impl Eq for ShadowToken {}

impl Debug for ShadowToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ShadowToken")
            .field("shadow", &self.0.shadow)
            .field("index", &self.0.index)
            .finish()
    }
}

pub struct ShadowHook<H>(Box<dyn Lane<H>>);

impl<H> Clone for ShadowHook<H> {
    fn clone(&self) -> Self {
        Self(self.0.fork())
    }
}

impl<H> Debug for ShadowHook<H> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
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

    pub fn forced(&self, steps: &[[u8; WRAM_SIZE]]) {
        self.0.forced(steps);
    }

    #[must_use]
    pub fn saved(&self) -> ShadowToken {
        self.0.saved()
    }

    pub fn restored(&self, token: Option<&ShadowToken>, ram: &[u8]) {
        self.0.restored(token, ram);
    }

    pub fn observe(&self, hand: Option<H>) -> Option<H> {
        self.0.observe(hand)
    }

    #[cfg(test)]
    fn key(&self) -> Option<crate::search::assertion::AssertionKey> {
        self.0.key()
    }
}

pub struct StateHook<G: Game, H: ArchiveKey>(Threaded<G, H>);

impl<G: Game, H: ArchiveKey> Clone for StateHook<G, H> {
    fn clone(&self) -> Self {
        Self(Threaded::new(Arc::clone(&self.0.shadow)))
    }
}

impl<G: Game, H: ArchiveKey> Debug for StateHook<G, H> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StateHook")
    }
}

impl<G: Game, H: ArchiveKey + Debug + Send + Sync + 'static> StateHook<G, H>
where
    H::Lineage: Send + Sync,
{
    pub fn start_state(&self, state: &G::State) {
        self.0.enter(self.0.begin_state(state.clone()), None);
    }

    pub fn advance_states(&self, states: &[G::State]) {
        self.0.moved(|threaded, position| {
            for state in states {
                position.state = threaded.arrive_state(state.clone(), &mut position.cursor);
            }
        });
    }

    #[must_use]
    pub fn saved(&self) -> ShadowToken {
        self.0.saved()
    }

    pub fn restored_state(&self, token: Option<&ShadowToken>, state: &G::State) {
        self.0.resume(token, || self.0.begin_state(state.clone()));
    }

    pub fn observe(&self, hand: Option<H>) -> Option<H> {
        self.0.observe(hand)
    }
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
    awaiting: bool,
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
                awaiting: false,
            }),
        }
    }
}

impl<G: Game, H: ArchiveKey + Debug + Send + Sync + 'static> Threaded<G, H>
where
    H::Lineage: Send + Sync,
{
    fn arrive_state(&self, state: G::State, cursor: &mut ShadowCursor<H>) -> G::State {
        let lowered = &self.shadow.lowered;
        cursor.observe(&lowered.compiled, &lowered.record(&state).0);
        state
    }

    fn arrive(
        &self,
        previous: Option<&G::State>,
        ram: &[u8],
        cursor: &mut ShadowCursor<H>,
    ) -> G::State {
        self.arrive_state(self.shadow.lowered.game.decode(previous, ram), cursor)
    }

    fn begin_state(&self, state: G::State) -> Position<G::State, H> {
        let mut cursor = ShadowCursor::default();
        let state = self.arrive_state(state, &mut cursor);
        Position { state, cursor }
    }

    fn begin(&self, ram: &[u8]) -> Position<G::State, H> {
        self.begin_state(self.shadow.lowered.game.decode(None, ram))
    }

    fn enter(&self, position: Position<G::State, H>, pending: Option<u64>) {
        if let Ok(mut current) = self.current.lock() {
            current.position = Some(position);
            current.pending = pending;
            current.awaiting = false;
        }
    }

    fn moved(&self, step: impl FnOnce(&Self, &mut Position<G::State, H>)) {
        let Ok(mut current) = self.current.lock() else {
            return;
        };
        current.pending = None;
        if current.awaiting {
            self.shadow.table.unobserved();
        }
        current.awaiting = true;
        if let Some(position) = current.position.as_mut() {
            step(self, position);
        }
    }

    fn resume(&self, token: Option<&ShadowToken>, fresh: impl FnOnce() -> Position<G::State, H>) {
        let found = token
            .and_then(|token| token.index_in(self.shadow.id))
            .and_then(|index| {
                let saved = self.shadow.saved.lock().ok()?;
                Some((index, saved.get(&index)?.clone()))
            });
        let (pending, position) = match found {
            Some((index, position)) => (Some(index), position),
            None => {
                self.shadow.table.unknown_restore();
                (None, fresh())
            }
        };
        self.enter(position, pending);
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
        self.enter(self.begin(ram), None);
    }

    fn advance(&self, frames: &[[u8; WRAM_SIZE]]) {
        self.moved(|threaded, position| {
            for wram in frames {
                position.state = threaded.arrive(Some(&position.state), wram, &mut position.cursor);
            }
        });
    }

    fn forced(&self, steps: &[[u8; WRAM_SIZE]]) {
        let Ok(mut current) = self.current.lock() else {
            return;
        };
        current.pending = None;
        current.awaiting = true;
        let (Some(position), Some((arrival, interior))) =
            (current.position.as_mut(), steps.split_last())
        else {
            return;
        };
        let game = &self.shadow.lowered.game;
        for wram in interior {
            position.state = game.decode(Some(&position.state), wram);
        }
        position.state = self.arrive(Some(&position.state), arrival, &mut position.cursor);
    }

    fn saved(&self) -> ShadowToken {
        let index = self.shadow.issued.fetch_add(1, Ordering::Relaxed);
        let saved: Weak<Shadow<G, H>> = Arc::downgrade(&self.shadow);
        let token = ShadowToken(Arc::new(Issued {
            shadow: self.shadow.id,
            index,
            saved,
        }));
        let Ok(mut current) = self.current.lock() else {
            return token;
        };
        if let (Some(position), Ok(mut saved)) =
            (current.position.as_ref(), self.shadow.saved.lock())
        {
            saved.insert(index, position.clone());
        }
        current.pending = Some(index);
        token
    }

    fn restored(&self, token: Option<&ShadowToken>, ram: &[u8]) {
        self.resume(token, || self.begin(ram));
    }

    fn observe(&self, hand: Option<H>) -> Option<H> {
        let Ok(mut current) = self.current.lock() else {
            return None;
        };
        let pending = current.pending.take();
        current.awaiting = false;
        let Some(position) = current.position.as_mut() else {
            self.shadow.table.unknown_restore();
            return None;
        };
        let completed = position.cursor.pair(&self.shadow.table, hand);
        if let Some(index) = pending
            && let Ok(mut saved) = self.shadow.saved.lock()
            && let Some(kept) = saved.get_mut(&index)
        {
            *kept = position.clone();
        }
        completed
    }

    #[cfg(test)]
    fn key(&self) -> Option<crate::search::assertion::AssertionKey> {
        self.current.lock().ok()?.position.as_ref()?.cursor.key()
    }
}

pub struct Shadow<G: Game, H: ArchiveKey> {
    lowered: Lowered<G>,
    table: ShadowTable<H>,
    saved: Mutex<BTreeMap<u64, Position<G::State, H>>>,
    id: u64,
    issued: AtomicU64,
}

impl<G: Game, H: ArchiveKey + Debug + Send + Sync + 'static> Shadow<G, H>
where
    H::Lineage: Send + Sync,
{
    pub fn new(game: G) -> Result<Arc<Self>, Box<dyn Error>> {
        Self::with_strategy(game, G::STRATEGY)
    }

    pub fn with_strategy(game: G, strategy: &str) -> Result<Arc<Self>, Box<dyn Error>> {
        Ok(Arc::new(Self {
            lowered: Lowered::new(game, strategy)?,
            table: ShadowTable::default(),
            saved: Mutex::default(),
            id: SHADOWS.fetch_add(1, Ordering::Relaxed),
            issued: AtomicU64::new(0),
        }))
    }

    #[must_use]
    pub fn hook(self: &Arc<Self>) -> ShadowHook<H> {
        ShadowHook(Box::new(Threaded::new(Arc::clone(self))))
    }

    #[must_use]
    pub fn state_hook(self: &Arc<Self>) -> StateHook<G, H> {
        StateHook(Threaded::new(Arc::clone(self)))
    }

    pub fn report(&self) -> Result<ShadowReport, Box<dyn Error>> {
        self.table.report(&self.lowered.compiled)
    }

    #[cfg(test)]
    fn saved_positions(&self) -> usize {
        self.saved.lock().map_or(0, |saved| saved.len())
    }
}

impl<G: Game, H: ArchiveKey + Debug + Send + Sync + 'static> Forget for Shadow<G, H>
where
    H::Lineage: Send + Sync,
{
    fn forget(&self, index: u64) {
        let forgotten = self
            .saved
            .lock()
            .ok()
            .and_then(|mut saved| saved.remove(&index));
        drop(forgotten);
    }
}

#[cfg(test)]
mod tests {
    use machine::{
        Machine, SnapId, StopConditions,
        nes::{ButtonChord, reproducer},
    };

    use super::*;
    use crate::{
        search::assertion::{Accumulator, AssertionKey},
        target::Target,
        workload::{
            fixture::{LEFT, Ledge, LedgeKey, RIGHT, Spot, Stairs, Stepper, WARP},
            target::{NesTarget, read_wram},
        },
    };

    #[derive(Clone)]
    struct Saved {
        state: Vec<u8>,
        frames: u64,
        token: Option<ShadowToken>,
    }

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

        fn rejected(&mut self) -> Option<LedgeKey> {
            self.hook.observe(None)
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

        fn save(&mut self) -> Saved {
            let state = self.machine.export(self.at, None).unwrap();
            let token = Some(self.hook.saved());
            Saved {
                state,
                frames: self.frames,
                token,
            }
        }

        fn restore(&mut self, saved: &Saved) {
            self.at = self.machine.import(&saved.state).unwrap();
            self.machine.replay(self.at).unwrap();
            self.frames = saved.frames;
            self.hook
                .restored(saved.token.as_ref(), &read_wram(&self.machine).unwrap());
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
        assert_eq!(report.unobserved, 0);
    }

    #[test]
    fn a_state_hook_takes_decoded_states_and_restores_the_cursor_saved_with_its_token() {
        let shadow = Shadow::<Ledge, LedgeKey>::new(Ledge).unwrap();
        let hook = shadow.state_hook();
        let spot = |x, room| Spot { x, room };
        let step = |states: &[Spot]| {
            hook.advance_states(states);
            hook.observe(states.last().copied().map(LedgeKey::raw))
        };
        hook.start_state(&spot(0, 0));
        hook.observe(Some(LedgeKey::raw(spot(0, 0))));
        step(&[spot(1, 0), spot(2, 0)]);
        step(&[spot(5, 0)]);
        let token = hook.saved();
        hook.observe(Some(LedgeKey::raw(spot(5, 0))));
        let on = [[spot(1, 0)], [spot(1, 1)], [spot(4, 1)]];
        let first: Vec<_> = on.iter().map(|states| step(states)).collect();
        hook.restored_state(Some(&token), &spot(5, 0));
        assert_eq!(
            on.iter().map(|states| step(states)).collect::<Vec<_>>(),
            first
        );
        assert_eq!(first[0].map(|key| key.band), Some(5));
        let report = shadow.report().unwrap();
        assert_eq!(report.disagreements.total(), 0, "{:?}", report.examples);
        assert_eq!((report.unknown_restores, report.unobserved), (0, 0));
        hook.restored_state(None, &spot(0, 0));
        assert_eq!(shadow.report().unwrap().unknown_restores, 1);
    }

    #[test]
    fn a_restore_brings_back_the_lineage_saved_with_its_snapshot_when_another_path_reaches_the_same_state()
     {
        let shadow = Shadow::<Ledge, LedgeKey>::new(Ledge).unwrap();
        let mut hand = Hand::new(shadow.hook());
        let genesis = hand.save();
        hand.walk(&[(RIGHT, 2), (LEFT, 2)]);
        let climbed = hand.save();
        let on = [(RIGHT, 1), (WARP, 1)];
        let first = hand.walk(&on);
        hand.restore(&genesis);
        hand.walk(&[(LEFT, 4)]);
        let walked = hand.save();
        assert_eq!(
            (&walked.state, walked.frames),
            (&climbed.state, climbed.frames)
        );
        hand.restore(&climbed);
        assert_eq!(hand.walk(&on), first);
        assert_eq!(first[0].map(|key| key.band), Some(2));
        let report = shadow.report().unwrap();
        assert_eq!(report.disagreements.total(), 0, "{:?}", report.examples);
        assert_eq!(report.unknown_restores, 0);
    }

    #[test]
    fn a_candidate_observed_after_a_probe_restores_its_snapshot_keeps_that_key_in_the_snapshot() {
        let shadow = Shadow::<Ledge, LedgeKey>::new(Ledge).unwrap();
        let mut hand = Hand::new(shadow.hook());
        hand.walk(&[(RIGHT, 2)]);
        hand.apply(RIGHT, 2);
        let candidate = hand.save();
        hand.restore(&candidate);
        hand.key();
        let on = [(LEFT, 1)];
        let first = hand.walk(&on);
        hand.restore(&candidate);
        assert_eq!(hand.walk(&on), first);
        assert_eq!(first[0].map(|key| key.band), Some(4));
        let report = shadow.report().unwrap();
        assert_eq!(report.disagreements.total(), 0, "{:?}", report.examples);
        assert_eq!(report.unknown_restores, 0);
        assert_eq!(report.unobserved, 0);
    }

    #[test]
    fn a_token_from_another_shadow_or_no_token_restores_as_unknown() {
        let first = Shadow::<Ledge, LedgeKey>::new(Ledge).unwrap();
        let mut away = Hand::new(first.hook());
        away.walk(&[(RIGHT, 2)]);
        let foreign = away.save();
        let second = Shadow::<Ledge, LedgeKey>::new(Ledge).unwrap();
        let mut hand = Hand::new(second.hook());
        hand.save();
        hand.restore(&foreign);
        hand.key();
        assert_eq!(second.report().unwrap().unknown_restores, 1);
        hand.restore(&Saved {
            token: None,
            ..foreign
        });
        hand.key();
        assert_eq!(second.report().unwrap().unknown_restores, 2);
        assert_eq!(first.report().unwrap().unknown_restores, 0);
    }

    #[test]
    fn dropping_the_last_snapshot_of_a_token_forgets_its_saved_cursor() {
        let shadow = Shadow::<Ledge, LedgeKey>::new(Ledge).unwrap();
        let mut hand = Hand::new(shadow.hook());
        hand.walk(&[(RIGHT, 2)]);
        let kept = hand.save();
        hand.key();
        assert_eq!(shadow.saved_positions(), 1);
        hand.walk(&[(RIGHT, 1)]);
        drop(hand.save());
        hand.key();
        assert_eq!(shadow.saved_positions(), 1);
        drop(kept);
        assert_eq!(shadow.saved_positions(), 0);
    }

    #[test]
    fn a_cloned_token_keeps_its_saved_cursor_until_the_last_clone_drops() {
        let shadow = Shadow::<Ledge, LedgeKey>::new(Ledge).unwrap();
        let mut hand = Hand::new(shadow.hook());
        hand.walk(&[(RIGHT, 4), (LEFT, 2)]);
        let saved = hand.save();
        hand.key();
        let copy = saved.clone();
        let on = [(RIGHT, 1), (WARP, 1)];
        let first = hand.walk(&on);
        drop(saved);
        assert_eq!(shadow.saved_positions(), 1);
        hand.restore(&copy);
        assert_eq!(hand.walk(&on), first);
        assert_eq!(first[0].map(|key| key.band), Some(4));
        assert_eq!(shadow.report().unwrap().unknown_restores, 0);
        drop(copy);
        assert_eq!(shadow.saved_positions(), 0);
    }

    #[test]
    fn a_live_token_restores_its_cursor_after_other_tokens_drop() {
        let shadow = Shadow::<Ledge, LedgeKey>::new(Ledge).unwrap();
        let mut hand = Hand::new(shadow.hook());
        let genesis = hand.save();
        hand.walk(&[(RIGHT, 4), (LEFT, 2)]);
        let middle = hand.save();
        hand.key();
        let on = [(RIGHT, 1), (LEFT, 2), (WARP, 1)];
        let first = hand.walk(&on);
        let late = hand.save();
        hand.key();
        drop(genesis);
        drop(late);
        assert_eq!(shadow.saved_positions(), 1);
        hand.restore(&middle);
        assert_eq!(hand.walk(&on), first);
        assert_eq!(first[1].map(|key| key.band), Some(4));
        let report = shadow.report().unwrap();
        assert_eq!(report.disagreements.total(), 0, "{:?}", report.examples);
        assert_eq!(report.unknown_restores, 0);
    }

    #[test]
    fn a_frame_the_strategy_rejects_agrees_with_a_hand_key_of_none() {
        let strategy = format!("reject = \"room == 2\"\n{}", Ledge::STRATEGY);
        let shadow = Shadow::<Ledge, LedgeKey>::with_strategy(Ledge, &strategy).unwrap();
        let mut hand = Hand::new(shadow.hook());
        hand.walk(&[(WARP, 1)]);
        hand.apply(WARP, 1);
        hand.rejected();
        let rejected = hand.save();
        hand.rejected();
        hand.apply(RIGHT, 1);
        hand.rejected();
        hand.walk(&[(WARP, 1)]);
        let report = shadow.report().unwrap();
        assert_eq!(report.disagreements.total(), 0, "{:?}", report.examples);
        assert_eq!((report.states, report.keyed_states), (6, 3));
        hand.apply(RIGHT, 1);
        hand.rejected();
        assert_eq!(shadow.report().unwrap().disagreements.absent, 1);
        hand.restore(&rejected);
        hand.key();
        let report = shadow.report().unwrap();
        assert_eq!(report.disagreements.absent, 2, "{:?}", report.examples);
        assert_eq!(report.unknown_restores, 0);
    }

    #[test]
    fn a_goal_state_observed_with_the_hand_key_agrees() {
        let shadow = Shadow::<Ledge, LedgeKey>::new(Ledge).unwrap();
        let goal = shadow
            .lowered
            .observe(0, Spot { x: 0, room: 9 }, &mut Accumulator::default());
        assert!(goal.goal && goal.key.is_some() && goal.rejected.is_none());
        let mut hand = Hand::new(shadow.hook());
        let keys = hand.walk(&[(WARP, 1); 9]);
        assert_eq!(keys[8].map(|key| key.room), Some(9));
        let report = shadow.report().unwrap();
        assert_eq!(report.disagreements.total(), 0, "{:?}", report.examples);
        assert_eq!((report.states, report.keyed_states), (10, 10));
        hand.apply(RIGHT, 1);
        hand.rejected();
        assert_eq!(shadow.report().unwrap().disagreements.absent, 1);
        let strategy = format!("reject = \"room >= 9\"\n{}", Ledge::STRATEGY);
        let shadow = Shadow::<Ledge, LedgeKey>::with_strategy(Ledge, &strategy).unwrap();
        let mut hand = Hand::new(shadow.hook());
        hand.walk(&[(WARP, 1); 9]);
        let report = shadow.report().unwrap();
        assert_eq!(report.disagreements.total(), 0, "{:?}", report.examples);
        assert_eq!((report.states, report.keyed_states), (10, 10));
    }

    #[test]
    fn a_state_that_ends_the_path_observed_before_a_restore_leaves_nothing_unobserved() {
        let strategy = format!("reject = \"room == 2\"\n{}", Ledge::STRATEGY);
        let shadow = Shadow::<Ledge, LedgeKey>::with_strategy(Ledge, &strategy).unwrap();
        let mut hand = Hand::new(shadow.hook());
        hand.walk(&[(WARP, 1)]);
        let root = hand.save();
        for _ in 0..2 {
            hand.apply(WARP, 1);
            hand.rejected();
            hand.restore(&root);
            hand.walk(&[(RIGHT, 1)]);
            hand.restore(&root);
        }
        let report = shadow.report().unwrap();
        assert_eq!(report.unobserved, 0);
        assert_eq!(report.disagreements.total(), 0, "{:?}", report.examples);
        assert_eq!((report.states, report.unknown_restores), (6, 0));
        let shadow = Shadow::<Ledge, LedgeKey>::new(Ledge).unwrap();
        let mut hand = Hand::new(shadow.hook());
        let root = hand.save();
        for _ in 0..2 {
            hand.walk(&[(WARP, 1); 9]);
            hand.restore(&root);
        }
        let report = shadow.report().unwrap();
        assert_eq!(report.unobserved, 0);
        assert_eq!(report.disagreements.total(), 0, "{:?}", report.examples);
        assert_eq!((report.states, report.unknown_restores), (19, 0));
    }

    #[test]
    fn a_hand_target_that_observes_only_its_root_leaves_arrivals_unobserved() {
        let shadow = Shadow::<Ledge, LedgeKey>::new(Ledge).unwrap();
        let mut hand = Hand::new(shadow.hook());
        let genesis = hand.save();
        for _ in 0..2 {
            hand.restore(&genesis);
            for _ in 0..3 {
                hand.apply(RIGHT, 1);
                hand.save();
            }
        }
        let report = shadow.report().unwrap();
        assert_eq!(report.states, 1);
        assert_eq!(report.unobserved, 4);
    }

    struct Lockstep {
        target: NesTarget<Stairs, Stepper>,
        machine: Stepper,
        at: SnapId,
        hook: ShadowHook<AssertionKey>,
    }

    impl Lockstep {
        fn new(hook: ShadowHook<AssertionKey>) -> Self {
            let lowered = Arc::new(Lowered::new(Stairs, Stairs::STRATEGY).unwrap());
            let target = NesTarget::from_machine(lowered, Stepper::default()).unwrap();
            let mut machine = Stepper::default();
            let at = machine.snapshot().unwrap();
            hook.start(&read_wram(&machine).unwrap());
            let mut lockstep = Self {
                target,
                machine,
                at,
                hook,
            };
            lockstep.key();
            lockstep
        }

        fn key(&mut self) -> Option<AssertionKey> {
            let key = self.target.observation().key;
            self.hook.observe(key);
            assert_eq!(self.hook.key(), key);
            key
        }

        fn press(&mut self, buttons: u8, hold: u8) -> [u8; WRAM_SIZE] {
            let chord = ButtonChord::new(buttons, hold);
            self.machine
                .branch(self.at, &reproducer(std::slice::from_ref(&chord)))
                .unwrap();
            self.machine.run(StopConditions::default(), None).unwrap();
            self.at = self.machine.snapshot().unwrap();
            read_wram(&self.machine).unwrap()
        }

        fn apply(&mut self, buttons: u8, hold: u8) -> Option<AssertionKey> {
            self.target.apply(&ButtonChord::new(buttons, hold));
            let departure = self.press(buttons, hold);
            self.hook.advance(self.machine.frames());
            if Stairs
                .moves(&Stairs.decode(None, &departure))
                .forced
                .is_some()
            {
                let steps = [RIGHT, RIGHT, LEFT].map(|buttons| self.press(buttons, 1));
                self.hook.forced(&steps);
            }
            self.key()
        }
    }

    #[test]
    fn a_forced_move_decodes_its_interior_steps_in_a_chain_and_observes_only_the_arrival() {
        let shadow = Shadow::<Stairs, AssertionKey>::new(Stairs).unwrap();
        let mut lockstep = Lockstep::new(shadow.hook());
        let identities: Vec<Vec<i64>> = [(RIGHT, 6), (RIGHT, 1), (LEFT, 2), (WARP, 1), (RIGHT, 6)]
            .into_iter()
            .map(|(buttons, hold)| {
                let key = lockstep.apply(buttons, hold).unwrap();
                shadow.lowered.compiled.view(key).identity
            })
            .collect();
        assert_eq!(
            identities,
            [[7, 9], [8, 10], [8, 15], [0, 0], [7, 9]].map(Vec::from)
        );
        let report = shadow.report().unwrap();
        assert_eq!(report.disagreements.total(), 0, "{:?}", report.examples);
        assert_eq!(report.keyed_states, 6);
        assert_eq!(report.unobserved, 0);
    }

    #[test]
    fn a_shadow_lowers_the_strategy_text_it_is_given() {
        const FLAT: &str =
            "capacity = 1\ngoal = \"room >= 9\"\nspread = \"room\"\nidentity = \"x\"\n";
        let shipped = Shadow::<Ledge, LedgeKey>::new(Ledge).unwrap();
        let flat = Shadow::<Ledge, LedgeKey>::with_strategy(Ledge, FLAT).unwrap();
        assert!(!shipped.lowered.compiled.is_pure());
        assert!(flat.lowered.compiled.is_pure());
        let policy = flat.report().unwrap().policy;
        assert_eq!(
            policy,
            Lowered::new(Ledge, FLAT)
                .unwrap()
                .compiled
                .policy_identifier()
        );
        assert_ne!(policy, shipped.report().unwrap().policy);
        assert!(Shadow::<Ledge, LedgeKey>::with_strategy(Ledge, "capacity = 1").is_err());
    }
}
