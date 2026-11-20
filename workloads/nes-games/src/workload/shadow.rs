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

use super::{Game, Lowered, Moves};
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

struct Position<S, H: ArchiveKey> {
    kept: S,
    state: S,
    cursor: ShadowCursor<H>,
}

impl<S: Clone, H: ArchiveKey> Clone for Position<S, H> {
    fn clone(&self) -> Self {
        Self {
            kept: self.kept.clone(),
            state: self.state.clone(),
            cursor: self.cursor.clone(),
        }
    }
}

struct Current<S, H: ArchiveKey> {
    position: Option<Position<S, H>>,
    pending: Option<u64>,
    awaiting: bool,
    departed: bool,
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
                departed: false,
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
        parent: Option<&G::State>,
        previous: Option<&G::State>,
        ram: &[u8],
        ends: impl FnOnce(&G::State, &Moves<G::Move>) -> bool,
        cursor: &mut ShadowCursor<H>,
    ) -> (G::State, bool, bool) {
        let lowered = &self.shadow.lowered;
        let state = lowered.game.decode(parent, previous, ram);
        let moves = lowered.game.moves(&state);
        let mut record = lowered.record(&state).0;
        let ends = cursor.observe_ending(&lowered.compiled, &mut record, ends(&state, &moves));
        let departs = moves.forced_after(ends).is_some();
        (state, ends, departs)
    }

    fn begin(&self, ram: &[u8]) -> Position<G::State, H> {
        let mut cursor = ShadowCursor::default();
        let (state, ..) = self.arrive(None, None, ram, |_, _| true, &mut cursor);
        Position {
            kept: state.clone(),
            state,
            cursor,
        }
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
            current.awaiting = false;
            current.departed = false;
        }
    }

    fn advance(&self, frames: &[[u8; WRAM_SIZE]]) {
        let Ok(mut current) = self.current.lock() else {
            return;
        };
        let current = &mut *current;
        current.pending = None;
        current.departed = false;
        if current.awaiting {
            self.shadow.table.unobserved();
        }
        current.awaiting = true;
        let Some(position) = current.position.as_mut() else {
            return;
        };
        let parent = position.kept.clone();
        for (index, wram) in frames.iter().enumerate() {
            let last_frame = index + 1 == frames.len();
            let (state, ends, departs) = self.arrive(
                Some(&parent),
                Some(&position.state),
                wram,
                |_, moves| moves.ends_input(last_frame),
                &mut position.cursor,
            );
            position.state = state;
            if ends || departs {
                current.departed = departs;
                break;
            }
        }
        if !current.departed {
            position.kept = position.state.clone();
        }
    }

    fn forced(&self, steps: &[[u8; WRAM_SIZE]]) {
        let Ok(mut current) = self.current.lock() else {
            return;
        };
        let current = &mut *current;
        current.pending = None;
        current.awaiting = true;
        let game = &self.shadow.lowered.game;
        let departed = std::mem::take(&mut current.departed);
        let Some(position) = current.position.as_mut().filter(|_| departed) else {
            return;
        };
        let parent = std::mem::replace(&mut position.kept, position.state.clone());
        let Some(mut mv) = game.moves(&position.state).forced else {
            return;
        };
        let departure = position.cursor.clone();
        for wram in steps {
            let Some(step) = game.play(mv, &position.state) else {
                break;
            };
            let mut cursor = departure.clone();
            let (state, ends, _) = self.arrive(
                Some(&parent),
                Some(&position.state),
                wram,
                |state, moves| {
                    let last_step = step
                        .rest
                        .is_none_or(|next| game.play(next, state).is_none());
                    moves.ends_move(last_step, false)
                },
                &mut cursor,
            );
            *position = Position {
                kept: state.clone(),
                state,
                cursor,
            };
            match step.rest {
                Some(next) if !ends => mv = next,
                _ => break,
            }
        }
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
                (None, self.begin(ram))
            }
        };
        if let Ok(mut current) = self.current.lock() {
            current.position = Some(position);
            current.pending = pending;
            current.awaiting = false;
            current.departed = false;
        }
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

    use std::panic::{AssertUnwindSafe, catch_unwind};

    use super::*;
    use crate::{
        search::{
            assertion::{
                Accumulator, AssertionKey, Disagreements, RecordedAction, RecordedJob, Recording,
                RecordsWriter, Rekeying, StreamIdentity,
            },
            campaign::CampaignAdmissionDecision,
        },
        target::Target,
        workload::{
            NesObservation,
            fixture::{
                LEFT, Landing, Ledge, LedgeKey, RIGHT, Spot, Stairs, Stepper, Switchback, WARP,
            },
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
            let spot = Ledge.decode(None, None, &read_wram(&self.machine).unwrap());
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
                .moves(&Stairs.decode(None, None, &departure))
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
    fn a_forced_move_observes_each_step_from_its_departure_and_keeps_the_arrival() {
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

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct Kept {
        state: Landing,
        key: Option<AssertionKey>,
        stops: bool,
        frames: u64,
    }

    impl Kept {
        fn of(observation: &NesObservation<Landing>) -> Self {
            Self {
                state: observation.state,
                key: observation.key,
                stops: observation.stops(),
                frames: observation.frame_count,
            }
        }
    }

    fn shown(key: Option<AssertionKey>) -> String {
        key.map_or_else(
            || "no key".to_owned(),
            |key| {
                format!(
                    "key (progress {}, place {}, identity {})",
                    key.progress,
                    key.place.get(),
                    key.identity
                )
            },
        )
    }

    fn kept(kept: &Kept) -> String {
        format!(
            "x {} dwell {} after {} frames {}",
            kept.state.x,
            kept.state.dwell,
            kept.frames,
            keyed((kept.key, kept.stops))
        )
    }

    fn keyed((key, stops): (Option<AssertionKey>, bool)) -> String {
        format!("{}{}", shown(key), if stops { " stops" } else { "" })
    }

    fn film_of<G: Game<State = Landing>>(lowered: &Arc<Lowered<G>>, tape: &[ButtonChord]) -> Kept {
        let mut film = NesTarget::from_machine(Arc::clone(lowered), Stepper::default()).unwrap();
        let tail = u32::from(tape.is_empty());
        let (metadata, filmed) = film
            .render_observed(tape, tail, 0, &mut Vec::new(), &mut Vec::new())
            .unwrap();
        Kept {
            frames: metadata.frames - u64::from(tail),
            ..Kept::of(&filmed)
        }
    }

    struct Producers<G: Game<State = Landing>> {
        game: G,
        lowered: Arc<Lowered<G>>,
        live: NesTarget<G, Stepper>,
        hand: Stepper,
        at: SnapId,
        hook: ShadowHook<AssertionKey>,
        tape: Vec<ButtonChord>,
        actions: Vec<RecordedAction<ButtonChord>>,
        jobs: Vec<RecordedJob>,
        cuts: Vec<u64>,
        live_kept: Vec<Kept>,
        shadow_keys: Vec<Option<AssertionKey>>,
        filmed: Vec<Kept>,
    }

    impl<G: Game<State = Landing>> Producers<G> {
        fn new(game: G, strategy: &str) -> (Arc<Shadow<G, AssertionKey>>, Self) {
            let shadow = Shadow::<G, AssertionKey>::with_strategy(game.clone(), strategy).unwrap();
            let lowered = Arc::new(Lowered::new(game.clone(), strategy).unwrap());
            let mut live =
                NesTarget::from_machine(Arc::clone(&lowered), Stepper::default()).unwrap();
            live.record_frames(true);
            let mut hand = Stepper::default();
            let at = hand.snapshot().unwrap();
            let hook = shadow.hook();
            hook.start(&read_wram(&hand).unwrap());
            let genesis = Kept::of(live.observation());
            hook.observe(genesis.key);
            let shadow_key = hook.key();
            let genesis_film = film_of(&lowered, &[]);
            let producers = Self {
                game,
                lowered,
                live,
                hand,
                at,
                hook,
                tape: Vec::new(),
                actions: Vec::new(),
                jobs: Vec::new(),
                cuts: vec![0],
                live_kept: vec![genesis],
                shadow_keys: vec![shadow_key],
                filmed: vec![genesis_film],
            };
            (shadow, producers)
        }

        fn press(&mut self, chord: ButtonChord) -> Vec<[u8; WRAM_SIZE]> {
            self.hand
                .branch(self.at, &reproducer(std::slice::from_ref(&chord)))
                .unwrap();
            self.hand.run(StopConditions::default(), None).unwrap();
            self.at = self.hand.snapshot().unwrap();
            self.hand.frames().to_vec()
        }

        fn resync(&mut self, wram: &[u8; WRAM_SIZE]) {
            self.at = self.hand.import(&wram[..2].to_vec()).unwrap();
        }

        fn apply(&mut self, buttons: u8, hold: u8) {
            let chord = ButtonChord::new(buttons, hold);
            let start = self.live.observation().frame_count;
            let parent = self.live.observation().state;
            self.live.apply(&chord);
            let emitted = self.live.last_action_observations().to_vec();
            let arrival = emitted.last().unwrap().observation.clone();
            let departure = emitted
                .len()
                .checked_sub(2)
                .map(|index| emitted[index].observation.clone())
                .filter(|departure| self.game.moves(&departure.state).forced.is_some());
            self.tape.push(chord);
            let whole = self.press(chord);
            self.hook.advance(&whole);
            if let Some(departure) = departure {
                let input = usize::try_from(departure.frame_count - start).unwrap();
                self.resync(&whole[input - 1]);
                let mut next = self.game.moves(&departure.state).forced;
                let mut state = departure.state;
                let mut steps = Vec::new();
                while let Some(step) = next.and_then(|mv| self.game.play(mv, &state)) {
                    let wram = *self.press(step.chord).last().unwrap();
                    state = self.game.decode(Some(&parent), Some(&state), &wram);
                    steps.push(wram);
                    next = step.rest;
                }
                self.hook.forced(&steps);
            }
            let wram = *self.live.wram();
            self.resync(&wram);
            self.hook.observe(arrival.key);
            self.shadow_keys.push(self.hook.key());
            self.live_kept.push(Kept::of(&arrival));
            self.cuts.push(arrival.frame_count);
            self.filmed.push(film_of(&self.lowered, &self.tape));
            let sequence = u64::try_from(self.tape.len()).unwrap();
            self.actions.push(RecordedAction {
                sequence,
                action: chord,
                failed: false,
                cost: emitted.iter().map(|observation| observation.cost).sum(),
                frames: emitted
                    .iter()
                    .flat_map(|observation| observation.records.iter().copied())
                    .collect(),
                choices: Vec::new(),
                context: None,
            });
            self.jobs.push(RecordedJob {
                sequence,
                parent_id: sequence - 1,
                decisions: (!arrival.stops())
                    .then_some(CampaignAdmissionDecision::Retained { id: sequence })
                    .into_iter()
                    .collect(),
            });
        }

        fn recording(&self) -> Recording<ButtonChord> {
            static FILES: AtomicU64 = AtomicU64::new(0);
            let header = self
                .lowered
                .records_header(
                    StreamIdentity {
                        campaign_seed: 1,
                        workload_identity_sha256: G::NAME.to_owned(),
                    },
                    &self.live_kept[0].state,
                    None,
                    None,
                )
                .unwrap();
            let path = std::env::temp_dir().join(format!(
                "nes-producers-records-{}-{}.bin",
                std::process::id(),
                FILES.fetch_add(1, Ordering::Relaxed)
            ));
            let mut writer = RecordsWriter::create(&path, &header).unwrap();
            for action in &self.actions {
                writer.push(action).unwrap();
            }
            writer.finish().unwrap();
            let bytes = std::fs::read(&path).unwrap();
            std::fs::remove_file(&path).unwrap();
            let recording = Recording::from_bytes(&bytes).unwrap();
            assert!(
                recording.header.genesis.action_end(),
                "the loaded genesis record has no action_end"
            );
            for action in &recording.actions {
                let marked: Vec<bool> = action
                    .frames
                    .iter()
                    .map(|frame| frame.action_end())
                    .collect();
                let last: Vec<bool> = (0..marked.len())
                    .map(|index| index + 1 == marked.len())
                    .collect();
                assert_eq!(
                    marked, last,
                    "action_end marks of the loaded frames of action {}",
                    action.sequence
                );
            }
            recording
        }

        fn rekeyed(&self, recording: &Self) -> Vec<(Option<AssertionKey>, bool)> {
            let records = recording.recording();
            let strategy = &self.lowered.compiled;
            let rekeyed = Rekeying {
                recorded: &recording.lowered.compiled,
                strategy,
                entry_limit: 256,
                rooted: None,
            }
            .run(&records.header.genesis, &recording.jobs, &records.actions)
            .unwrap_or_else(|error| panic!("rekey of {:?}: {error}", recording.tape));
            let mut accumulator = Accumulator::default();
            let genesis = strategy.observe(&records.header.genesis, &mut accumulator);
            let mut folded = vec![(genesis.key, genesis.goal || genesis.rejected.is_some())];
            for action in &records.actions {
                let mut last = None;
                for (index, frame) in action.frames.iter().enumerate() {
                    let observed = strategy.observe(frame, &mut accumulator);
                    let stops = observed.goal || observed.rejected.is_some();
                    assert!(
                        !stops || index + 1 == action.frames.len(),
                        "rekey of {:?} stops at frame {} of the {} frames of action {}",
                        recording.tape,
                        index + 1,
                        action.frames.len(),
                        action.sequence
                    );
                    last = Some((observed.key, stops));
                }
                folded.push(
                    last.unwrap_or_else(|| panic!("action {} has no frames", action.sequence)),
                );
            }
            let held: BTreeMap<u64, AssertionKey> = rekeyed
                .holders
                .iter()
                .flat_map(|held| held.ids.iter().copied().zip(held.keys.iter().copied()))
                .collect();
            let admitted: Vec<Option<AssertionKey>> = std::iter::once(held.get(&0).copied())
                .chain(rekeyed.decisions.iter().map(|made| match made.as_slice() {
                    [] => None,
                    [
                        CampaignAdmissionDecision::Retained { id }
                        | CampaignAdmissionDecision::Duplicate { id },
                    ] => Some(held[id]),
                    other => panic!("rekey of {:?} made {other:?}", recording.tape),
                }))
                .collect();
            let candidates: Vec<Option<AssertionKey>> = folded
                .iter()
                .map(|&(key, stops)| key.filter(|_| !stops))
                .collect();
            assert_eq!(
                admitted, candidates,
                "rekey of {:?}: the keys it admits against the keys of the loaded records",
                recording.tape
            );
            folded
        }
    }

    type Case = (
        &'static str,
        &'static str,
        &'static [(u8, u8)],
        &'static [(u8, bool)],
    );

    const PLAIN: &str =
        "capacity = 16\nspread = \"room\"\ngoal = \"x == 99\"\nidentity = [\"x\", \"dwell\"]\n";

    fn agree<G: Game<State = Landing>>(game: &G, (_, lines, tape, reached): &Case) {
        let strategy = format!("capacity = 16\nspread = \"room\"\n{lines}\n");
        let strategy = if strategy.contains("identity") {
            strategy
        } else {
            format!("{strategy}identity = [\"x\", \"dwell\"]\n")
        };
        let (shadow, mut producers) = Producers::new(game.clone(), &strategy);
        let (_, mut plain) = Producers::new(game.clone(), PLAIN);
        for &(buttons, hold) in *tape {
            if producers.live.observation().stops() {
                break;
            }
            producers.apply(buttons, hold);
            plain.apply(buttons, hold);
        }
        let ends: Vec<(u8, bool)> = producers.live_kept[1..]
            .iter()
            .map(|kept| (kept.state.x, kept.stops))
            .collect();
        assert_eq!(ends, *reached, "where the live target keeps each action");
        let rekeyed = producers.rekeyed(&producers);
        let across = (plain.cuts == producers.cuts).then(|| producers.rekeyed(&plain));
        for (index, live) in producers.live_kept.iter().enumerate() {
            let at = format!("kept state {index} after {:?}", &producers.tape[..index]);
            let shadow = producers.shadow_keys[index];
            assert!(
                shadow == live.key,
                "{at}: shadow {}, live {}",
                shown(shadow),
                kept(live)
            );
            let filmed = &producers.filmed[index];
            assert!(
                filmed == live,
                "{at}: film {}, live {}",
                kept(filmed),
                kept(live)
            );
            assert!(
                rekeyed[index] == (live.key, live.stops),
                "{at}: rekey {}, live {}",
                keyed(rekeyed[index]),
                kept(live)
            );
            if let Some(across) = &across {
                assert!(
                    across[index] == (live.key, live.stops),
                    "{at}: rekey of the records kept under another strategy {}, live {}",
                    keyed(across[index]),
                    kept(live)
                );
            }
        }
        let report = shadow.report().unwrap();
        assert_eq!(
            Disagreements {
                capacity: false,
                ..report.disagreements
            },
            Disagreements::default(),
            "the shadow report"
        );
    }

    fn disagreements<G: Game<State = Landing>>(game: &G, cases: &[Case]) -> Vec<String> {
        cases
            .iter()
            .filter_map(|case| {
                catch_unwind(AssertUnwindSafe(|| agree(game, case)))
                    .err()
                    .map(|panic| {
                        let message = panic
                            .downcast_ref::<String>()
                            .cloned()
                            .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
                            .unwrap_or_default();
                        format!("{} ({}): {message}", case.0, G::NAME)
                    })
            })
            .collect()
    }

    #[test]
    fn the_live_target_the_shadow_rekey_and_film_agree_on_every_kept_state() {
        let stairs: &[Case] = &[
            (
                "ordinary stop",
                "goal = \"x == 2\"",
                &[(RIGHT, 4)],
                &[(2, true)],
            ),
            (
                "stop at an action end",
                "goal = \"action_end and x == 3\"",
                &[(RIGHT, 2), (RIGHT, 1), (RIGHT, 1)],
                &[(2, false), (3, true)],
            ),
            (
                "revoked stop",
                "goal = \"x == 2 and not action_end\"\nidentity = \"highest(action_end and x == 2)\"",
                &[(RIGHT, 4), (RIGHT, 1)],
                &[(2, false), (3, false)],
            ),
            (
                "forced move after a departure",
                "goal = \"room >= 9\"\nidentity = \"highest(action_end and x == 6)\"",
                &[(RIGHT, 6), (RIGHT, 1), (LEFT, 2)],
                &[(7, false), (8, false), (7, false)],
            ),
            (
                "action_end goal at a middle step of a move",
                "goal = \"action_end and x == 8\"",
                &[(RIGHT, 6), (RIGHT, 1)],
                &[(7, false), (8, true)],
            ),
            (
                "plain goal at a middle step of a move",
                "goal = \"x == 8\"\nidentity = \"highest(action_end and x == 7)\"",
                &[(RIGHT, 6), (RIGHT, 1)],
                &[(8, true)],
            ),
            (
                "middle step of a move",
                "goal = \"x == 99\"\nidentity = \"highest(action_end and x == 8)\"",
                &[(RIGHT, 6), (RIGHT, 1)],
                &[(7, false), (8, false)],
            ),
            (
                "genesis",
                "goal = \"highest(action_end and x == 0) and x == 1\"",
                &[(RIGHT, 1)],
                &[(1, true)],
            ),
            (
                "film of a forced move",
                "goal = \"highest(action_end and x == 2) and x == 6\"",
                &[(RIGHT, 6), (RIGHT, 3)],
                &[(7, false), (10, false)],
            ),
            (
                "revoked stop at a middle step of a move",
                "goal = \"x == 8 and not action_end\"\nidentity = \"highest(action_end and x == 8)\"",
                &[(RIGHT, 6), (RIGHT, 1)],
                &[(8, false), (9, false)],
            ),
            (
                "lineage read from the kept state an input started from",
                "goal = \"departed == 1\"\nidentity = [\"x\", \"departed\"]",
                &[(RIGHT, 3), (RIGHT, 1)],
                &[(3, false), (4, false)],
            ),
            (
                "lineage of a forced move read from its input's kept state",
                "goal = \"departed == 6\"\nidentity = [\"x\", \"departed\"]",
                &[(RIGHT, 6), (RIGHT, 1)],
                &[(7, false), (8, false)],
            ),
            (
                "revoked stop at a forced move's departure",
                "goal = \"x == 6 and not action_end\"\nidentity = \"highest(action_end and x == 6)\"",
                &[(RIGHT, 6), (RIGHT, 1)],
                &[(6, false), (7, false)],
            ),
        ];
        let switchback: &[Case] = &[(
            "forced move that arrives where another forced move starts",
            "goal = \"x == 99\"\nidentity = \"highest(action_end and x == 4)\"",
            &[(RIGHT, 4), (RIGHT, 1)],
            &[(4, false), (5, false)],
        )];
        let mut failures = disagreements(&Stairs, stairs);
        failures.extend(disagreements(&Switchback, switchback));
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn a_records_header_keeps_the_genesis_action_end_for_a_rekey_under_the_same_strategy() {
        let (_, mut producers) = Producers::new(
            Stairs,
            "capacity = 16\nspread = \"room\"\ngoal = \"highest(action_end and x == 0) and x == 1\"\nidentity = [\"x\", \"dwell\"]\n",
        );
        producers.apply(RIGHT, 1);
        assert!(producers.live.observation().goal);
        let recording = producers.recording();
        assert_eq!(
            recording.header.genesis,
            producers
                .lowered
                .genesis_record(&producers.live_kept[0].state)
        );
        assert_eq!(
            producers.rekeyed(&producers),
            producers
                .live_kept
                .iter()
                .map(|kept| (kept.key, kept.stops))
                .collect::<Vec<_>>()
        );
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
