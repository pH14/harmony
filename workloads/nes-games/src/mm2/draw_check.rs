// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;

use machine::nes::ButtonChord;

use super::{
    action::{Mm2Action, switch_choice},
    game::WEAPON_SITE,
};
use crate::{
    chord::{CHORD_SITES, HOLD_FRAMES_SITE},
    search::{
        assertion::RecordedAction,
        choice::{Choice, ChoiceRecord},
        rand::RomuDuoJrRand,
    },
    workload::move_answer,
};

const MINIMUM_EXPECTED: f64 = 50.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Drawn {
    Chord(ButtonChord),
    Switch(u16),
    Idle,
}

#[derive(Default)]
struct Side {
    counts: BTreeMap<String, u64>,
    trials: BTreeMap<String, u64>,
}

impl Side {
    fn tally(&mut self, family: &str, category: String, hit: bool) {
        *self.trials.entry(family.to_owned()).or_default() += 1;
        if hit {
            *self.counts.entry(category).or_default() += 1;
        }
    }

    fn count(&mut self, family: &str, category: String) {
        *self.trials.entry(family.to_owned()).or_default() += 1;
        *self.counts.entry(category).or_default() += 1;
    }
}

struct Ask {
    targets: Vec<u8>,
    weapon_record: bool,
}

fn ask_of(action: &RecordedAction<ButtonChord>) -> Ask {
    let record = action
        .choices
        .iter()
        .find(|record| record.site == CHORD_SITES + WEAPON_SITE);
    Ask {
        targets: record.map_or_else(Vec::new, |record| {
            record
                .offered
                .iter()
                .filter(|label| **label != record.default)
                .filter_map(|label| u8::try_from(label - 1).ok())
                .collect()
        }),
        weapon_record: record.is_some(),
    }
}

#[derive(Default)]
struct Cuts {
    chords: u64,
    first: u64,
}

fn drawn_chord(
    action: &RecordedAction<ButtonChord>,
    first: bool,
    last: bool,
    cuts: &mut Cuts,
) -> Result<ButtonChord, String> {
    if move_answer(&action.action).is_some() {
        return Ok(action.action);
    }
    let Some(record) = action
        .choices
        .iter()
        .rev()
        .find(|record| record.site == HOLD_FRAMES_SITE)
    else {
        return Ok(action.action);
    };
    let drawn = u8::try_from(record.resolved).map_err(|error| error.to_string())?;
    let executed = action.action.hold_frames;
    if executed == drawn {
        return Ok(action.action);
    }
    if executed > drawn {
        return Err(format!(
            "job {} recorded a hold of {executed} frames, longer than its drawn hold of {drawn}",
            action.sequence
        ));
    }
    if !last {
        return Err(format!(
            "job {} recorded a hold cut to {executed} of {drawn} frames before its last action",
            action.sequence
        ));
    }
    cuts.chords += 1;
    cuts.first += u64::from(first);
    Ok(ButtonChord::new(action.action.buttons, drawn))
}

fn played(chord: ButtonChord) -> Drawn {
    match move_answer(&chord) {
        Some((site, label)) if site == WEAPON_SITE => Drawn::Switch(label),
        Some(_) => Drawn::Idle,
        None => Drawn::Chord(chord),
    }
}

fn hand_draw(rand: &mut RomuDuoJrRand, previous: Option<&Mm2Action>) -> Mm2Action {
    Mm2Action::draw(rand, previous).expect("hand draw")
}

fn hand_drawn(action: Mm2Action, targets: &[u8]) -> Drawn {
    match action {
        Mm2Action::Chord(chord) => Drawn::Chord(chord),
        Mm2Action::Switch(word) => switch_choice(targets, word)
            .map_or(Drawn::Idle, |weapon| Drawn::Switch(u16::from(weapon) + 1)),
    }
}

fn kind(drawn: Drawn) -> &'static str {
    match drawn {
        Drawn::Chord(_) => "chord",
        Drawn::Switch(_) => "switch",
        Drawn::Idle => "idle",
    }
}

fn tally_job(
    side: &mut Side,
    context: Option<ButtonChord>,
    strategy: &[(Drawn, bool)],
    job: &[(Drawn, bool)],
) {
    for (index, &(drawn, targeted)) in job.iter().enumerate() {
        let previous = match index.checked_sub(1) {
            Some(before) => strategy.get(before).copied(),
            None => context.map(|chord| (Drawn::Chord(chord), false)),
        };
        if targeted {
            let family = if index == 0 {
                "first_switch_taken"
            } else {
                "switch_taken"
            };
            side.tally(family, family.to_owned(), matches!(drawn, Drawn::Switch(_)));
            if let Drawn::Switch(label) = drawn {
                side.count("switch_label", format!("switch_label:{label}"));
            }
            if let Some((before, true)) = previous {
                side.count(
                    "kind_pair",
                    format!("kind_pair:{}>{}", kind(before), kind(drawn)),
                );
            }
        } else {
            side.tally(
                "untargeted",
                "untargeted:switch_or_idle".to_owned(),
                !matches!(drawn, Drawn::Chord(_)),
            );
        }
        let Drawn::Chord(chord) = drawn else {
            continue;
        };
        match previous {
            None => side.count("context", "context:fresh_first_in_job".to_owned()),
            Some((Drawn::Switch(_) | Drawn::Idle, _)) => {
                side.count("context", "context:after_switch".to_owned());
            }
            Some((Drawn::Chord(before), _)) => {
                side.count("chord", format!("chord:{:02x}", chord.buttons));
                side.count("hold", format!("hold:{}", chord.hold_frames));
                side.count(
                    "chord_pair",
                    format!("chord_pair:{:02x}>{:02x}", before.buttons, chord.buttons),
                );
            }
        }
    }
}

fn family_of(category: &str) -> &str {
    category.split(':').next().unwrap_or(category)
}

fn normal_cdf(z: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.231_641_9 * z.abs());
    let poly = t
        * (0.319_381_530
            + t * (-0.356_563_782
                + t * (1.781_477_937 + t * (-1.821_255_978 + t * 1.330_274_429))));
    let tail = (-z * z / 2.0).exp() / (2.0 * std::f64::consts::PI).sqrt() * poly;
    if z >= 0.0 { 1.0 - tail } else { tail }
}

fn upper_quantile(p: f64) -> f64 {
    let (mut low, mut high) = (0.0, 10.0);
    for _ in 0..200 {
        let mid = f64::midpoint(low, high);
        if 1.0 - normal_cdf(mid) > p {
            low = mid;
        } else {
            high = mid;
        }
    }
    high
}

fn compare<I>(recorded: I, seed: u64) -> Result<usize, String>
where
    I: IntoIterator<Item = Vec<RecordedAction<ButtonChord>>>,
{
    let mut rand = RomuDuoJrRand::with_seed(seed);
    let (mut strategy, mut hand) = (Side::default(), Side::default());
    let (mut asks, mut untargeted, mut actions, mut jobs) = (0_u64, 0_u64, 0_u64, 0_usize);
    let (mut replayed_jobs, mut replayed_actions, mut mixed_jobs) = (0_usize, 0_u64, 0_usize);
    let mut cuts = Cuts::default();
    for job in recorded {
        let drawn = job
            .iter()
            .filter(|action| !action.choices.is_empty())
            .count();
        if drawn < job.len() {
            if drawn == 0 {
                replayed_jobs += 1;
            } else {
                mixed_jobs += 1;
            }
            replayed_actions += job.len() as u64;
            continue;
        }
        jobs += 1;
        let context = job.first().and_then(|action| action.context);
        let mut strategy_job = Vec::with_capacity(job.len());
        let mut hand_job = Vec::with_capacity(job.len());
        let mut previous = context.map(Mm2Action::Chord);
        for (index, action) in job.iter().enumerate() {
            let ask = ask_of(action);
            actions += 1;
            asks += u64::from(ask.weapon_record);
            untargeted += u64::from(ask.targets.is_empty());
            let targeted = !ask.targets.is_empty();
            let chord = drawn_chord(action, index == 0, index + 1 == job.len(), &mut cuts)?;
            let strategy_drawn = played(chord);
            strategy_job.push((strategy_drawn, targeted));
            let drawn = hand_draw(&mut rand, previous.as_ref());
            hand_job.push((hand_drawn(drawn, &ask.targets), targeted));
            previous = Some(match strategy_drawn {
                Drawn::Chord(chord) => Mm2Action::Chord(chord),
                Drawn::Switch(_) | Drawn::Idle => Mm2Action::Switch(0),
            });
        }
        tally_job(&mut strategy, context, &strategy_job, &strategy_job);
        tally_job(&mut hand, context, &strategy_job, &hand_job);
    }
    let categories: Vec<String> = strategy
        .counts
        .keys()
        .chain(hand.counts.keys())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let judged = |family: &str| !matches!(family, "context" | "untargeted");
    let mut tested = Vec::new();
    for category in &categories {
        let family = family_of(category);
        let n1 = hand.trials.get(family).copied().unwrap_or(0) as f64;
        let n2 = strategy.trials.get(family).copied().unwrap_or(0) as f64;
        let c1 = hand.counts.get(category).copied().unwrap_or(0) as f64;
        let c2 = strategy.counts.get(category).copied().unwrap_or(0) as f64;
        let pooled = (c1 + c2) / (n1 + n2);
        let z = if pooled > 0.0 && pooled < 1.0 {
            (c2 / n2 - c1 / n1) / (pooled * (1.0 - pooled) * (1.0 / n1 + 1.0 / n2)).sqrt()
        } else {
            0.0
        };
        let enough = pooled * n1.min(n2) >= MINIMUM_EXPECTED;
        tested.push((
            category.clone(),
            c1,
            n1,
            c2,
            n2,
            z,
            judged(family) && enough,
        ));
    }
    let k = tested.iter().filter(|row| row.6).count();
    let limit = upper_quantile(0.025 / k.max(1) as f64);
    let mut failures = 0;
    for (category, c1, n1, c2, n2, z, judged) in &tested {
        let pass = !judged || z.abs() <= limit;
        failures += usize::from(!pass);
        println!(
            "{{\"category\":\"{category}\",\"hand\":{c1},\"hand_trials\":{n1},\"strategy\":{c2},\"strategy_trials\":{n2},\"z\":{z:.3},\"judged\":{judged},\"pass\":{pass}}}"
        );
    }
    println!(
        "{{\"jobs\":{jobs},\"actions\":{actions},\"weapon_asks\":{asks},\"untargeted_actions\":{untargeted},\"judged\":{k},\"z_limit\":{limit:.3},\"failures\":{failures},\"replayed_jobs\":{replayed_jobs},\"replayed_actions\":{replayed_actions},\"mixed_jobs\":{mixed_jobs},\"cut_chords\":{},\"cut_first_chords\":{}}}",
        cuts.chords, cuts.first
    );
    Ok(failures)
}

fn synthetic(
    fresh: impl Fn(&mut RomuDuoJrRand, Option<&Mm2Action>) -> ButtonChord,
    executed: impl Fn(usize, ButtonChord) -> ButtonChord,
) -> Vec<Vec<RecordedAction<ButtonChord>>> {
    let mut rand = RomuDuoJrRand::with_seed(0x5eed);
    let skewed = ButtonChord::new(0x83, 9);
    (0..40_000_u64)
        .map(|sequence| {
            let drawn = sequence % 2 == 0;
            let mut previous: Option<Mm2Action> = None;
            (0..3)
                .map(|position| {
                    let context = match previous {
                        Some(Mm2Action::Chord(chord)) => Some(chord),
                        _ => None,
                    };
                    let (action, choices) = if drawn {
                        let chord = fresh(&mut rand, previous.as_ref());
                        previous = Some(Mm2Action::Chord(chord));
                        let hold = u16::from(chord.hold_frames);
                        (
                            executed(position, chord),
                            vec![
                                ChoiceRecord::new(0, vec![0, 1], 0, Choice::Default),
                                ChoiceRecord::new(
                                    HOLD_FRAMES_SITE,
                                    vec![hold],
                                    hold,
                                    Choice::Default,
                                ),
                            ],
                        )
                    } else {
                        (skewed, Vec::new())
                    };
                    RecordedAction {
                        sequence,
                        action,
                        failed: false,
                        cost: u64::from(action.hold_frames),
                        frames: Vec::new(),
                        choices,
                        context,
                    }
                })
                .collect()
        })
        .collect()
}

fn hand_chord(rand: &mut RomuDuoJrRand, previous: Option<&Mm2Action>) -> ButtonChord {
    loop {
        if let Mm2Action::Chord(chord) = hand_draw(rand, previous) {
            return chord;
        }
    }
}

fn as_drawn(_: usize, chord: ButtonChord) -> ButtonChord {
    chord
}

fn cut_last(position: usize, chord: ButtonChord) -> ButtonChord {
    if position == 2 {
        ButtonChord::new(chord.buttons, 1 + chord.hold_frames / 3)
    } else {
        chord
    }
}

fn read_as_executed(
    jobs: Vec<Vec<RecordedAction<ButtonChord>>>,
) -> Vec<Vec<RecordedAction<ButtonChord>>> {
    jobs.into_iter()
        .map(|job| {
            job.into_iter()
                .map(|mut action| {
                    for record in &mut action.choices {
                        if record.site == HOLD_FRAMES_SITE {
                            record.resolved = u16::from(action.action.hold_frames);
                        }
                    }
                    action
                })
                .collect()
        })
        .collect()
}

#[test]
fn replayed_tails_are_left_out_and_fresh_draws_like_the_hand_pass() {
    assert_eq!(compare(synthetic(hand_chord, as_drawn), 0x6d32), Ok(0));
}

#[test]
fn last_actions_cut_short_pass_on_their_drawn_holds_and_fail_on_their_executed_holds() {
    let jobs = synthetic(hand_chord, cut_last);
    assert_eq!(compare(jobs.clone(), 0x6d32), Ok(0));
    assert!(compare(read_as_executed(jobs), 0x6d32).expect("no cut chords") > 0);
}

#[test]
fn a_recorded_hold_longer_than_its_draw_fails_the_check() {
    let longer = |position: usize, chord: ButtonChord| {
        if position == 2 {
            ButtonChord::new(chord.buttons, chord.hold_frames + 1)
        } else {
            chord
        }
    };
    assert!(compare(synthetic(hand_chord, longer), 0x6d32).is_err());
}

#[test]
fn a_hold_cut_before_the_last_action_fails_the_check() {
    let cut_first = |position: usize, chord: ButtonChord| {
        if position == 0 {
            ButtonChord::new(chord.buttons, 1)
        } else {
            chord
        }
    };
    assert!(compare(synthetic(hand_chord, cut_first), 0x6d32).is_err());
}

#[test]
fn fresh_draws_skewed_from_the_hand_draw_fail() {
    let skewed = |rand: &mut RomuDuoJrRand, previous: Option<&Mm2Action>| {
        if rand.next_u64().is_multiple_of(4) {
            ButtonChord::new(0x83, 9)
        } else {
            hand_chord(rand, previous)
        }
    };
    assert!(compare(synthetic(skewed, as_drawn), 0x6d32).expect("no cut chords") > 0);
}

#[test]
fn the_harness_draws_what_the_hand_search_and_target_draw() {
    use crate::{
        mm2::{
            campaign::{Mm2CampaignRun, Mm2Game},
            game::MegaMan2,
            target::switch_choices,
        },
        search::campaign::InputPolicy,
        workload::Game,
    };
    let hand = Mm2Game::<machine::quicknes::QuickNesMachine>::whole_game(
        &[],
        "core",
        "sha",
        Vec::new(),
        Box::new(|| Err(machine::MachineError::Backend("no machine".into()))),
    );
    let mut harness = RomuDuoJrRand::with_seed(0x6d32);
    let mut search = RomuDuoJrRand::with_seed(0x6d32);
    let mut states = RomuDuoJrRand::with_seed(7);
    let mut wram = [0_u8; machine::nes::WRAM_SIZE];
    let (mut previous, mut searched) = (None, None);
    let mut switches = 0;
    for _ in 0..50_000 {
        wram[0x9a] = u8::try_from(states.next_u64() >> 56).unwrap();
        wram[0x9b] = u8::try_from(states.next_u64() >> 61).unwrap();
        wram[0xa9] = u8::try_from(states.next_u64() % 12).unwrap();
        for energy in &mut wram[0x9c..0xa8] {
            *energy = u8::try_from(states.next_u64() >> 62).unwrap();
        }
        let targets: Vec<u8> = MegaMan2
            .decode(None, None, &wram)
            .switch_targets()
            .collect();
        let drawn = hand_draw(&mut harness, previous.as_ref());
        let action = hand
            .sample_alphabet(&Mm2CampaignRun, searched.as_ref(), &mut search)
            .expect("search draw");
        assert_eq!(drawn, action);
        let played = match action {
            Mm2Action::Chord(chord) => Drawn::Chord(chord),
            Mm2Action::Switch(word) => switch_choice(&switch_choices(&wram).unwrap(), word)
                .map_or(Drawn::Idle, |weapon| Drawn::Switch(u16::from(weapon) + 1)),
        };
        switches += usize::from(matches!(played, Drawn::Switch(_)));
        assert_eq!(hand_drawn(drawn, &targets), played);
        previous = Some(drawn);
        searched = Some(action);
    }
    assert!(switches > 1_000);
}

fn draw_categories(drawn: Drawn) -> Vec<String> {
    match drawn {
        Drawn::Chord(chord) => vec![
            "kind:chord".to_owned(),
            format!("chord:{:02x}", chord.buttons),
            format!("hold:{}", chord.hold_frames),
        ],
        Drawn::Switch(label) => vec!["kind:switch".to_owned(), format!("switch:{label}")],
        Drawn::Idle => vec!["kind:idle".to_owned()],
    }
}

fn expected_draws(
    switch_rate: f64,
    targets: &[u8],
    idles: bool,
    previous: Option<ButtonChord>,
) -> BTreeMap<String, f64> {
    use crate::chord::{LONG_HOLD_FRAMES, NES_PRESSABLE_BUTTON_MASKS, SHORT_HOLD_FRAMES};
    let held = NES_PRESSABLE_BUTTON_MASKS;
    let mut expected = BTreeMap::new();
    let mut add = |category: String, share: f64| *expected.entry(category).or_insert(0.0) += share;
    let switch_share = if targets.is_empty() && !idles {
        0.0
    } else {
        switch_rate
    };
    if targets.is_empty() {
        add("kind:idle".to_owned(), switch_share);
    } else {
        add("kind:switch".to_owned(), switch_share);
        for target in targets {
            add(
                format!("switch:{}", u16::from(*target) + 1),
                switch_share / targets.len() as f64,
            );
        }
    }
    let chord_share = 1.0 - switch_share;
    add("kind:chord".to_owned(), chord_share);
    let mut buttons: BTreeMap<u8, f64> = BTreeMap::new();
    let base = previous
        .map(|chord| chord.buttons)
        .filter(|base| held.contains(base));
    match base {
        None => {
            for mask in held {
                *buttons.entry(mask).or_default() += 1.0 / held.len() as f64;
            }
        }
        Some(base) => {
            let directions: std::collections::BTreeSet<u8> =
                held.iter().map(|mask| mask & 0xf0).collect();
            let edits: Vec<u8> = directions
                .iter()
                .filter(|direction| **direction != base & 0xf0)
                .map(|direction| (base & 0x0f) | direction)
                .filter(|mask| held.contains(mask))
                .collect();
            let toggled = |bit: u8| {
                Some(base ^ bit)
                    .filter(|mask| held.contains(mask))
                    .unwrap_or(base)
            };
            *buttons.entry(base).or_default() += 0.25;
            *buttons.entry(toggled(0x01)).or_default() += 0.25;
            *buttons.entry(toggled(0x02)).or_default() += 0.25;
            if edits.is_empty() {
                *buttons.entry(base).or_default() += 0.25;
            }
            for mask in &edits {
                *buttons.entry(*mask).or_default() += 0.25 / edits.len() as f64;
            }
        }
    }
    for (mask, share) in buttons {
        add(format!("chord:{mask:02x}"), chord_share * share);
    }
    for (low, high) in [SHORT_HOLD_FRAMES, LONG_HOLD_FRAMES] {
        for frames in low..=high {
            add(
                format!("hold:{frames}"),
                chord_share * 0.5 / f64::from(high - low + 1),
            );
        }
    }
    expected
}

fn departures(drawn: &[Drawn], expected: &BTreeMap<String, f64>) -> Vec<(String, f64)> {
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    for categories in drawn.iter().copied().map(draw_categories) {
        for category in categories {
            *counts.entry(category).or_default() += 1;
        }
    }
    let n = drawn.len() as f64;
    let mut judged = Vec::new();
    for category in counts.keys().chain(expected.keys()) {
        let share = expected.get(category).copied().unwrap_or(0.0);
        let count = counts.get(category).copied().unwrap_or(0) as f64;
        if share == 0.0 || share == 1.0 {
            assert_eq!(count, share * n, "{category}");
            continue;
        }
        if n * share.min(1.0 - share) >= MINIMUM_EXPECTED {
            let z = (count - n * share) / (n * share * (1.0 - share)).sqrt();
            judged.push((category.clone(), z));
        }
    }
    judged.sort_by(|a, b| a.0.cmp(&b.0));
    judged.dedup_by(|a, b| a.0 == b.0);
    judged
}

struct Declared {
    sites: Vec<crate::search::choice::ChoiceSite>,
    weapon_rate: u32,
}

fn declared() -> Declared {
    use crate::{
        mm2::game::MegaMan2,
        search::choice::{CERTAIN, ChoiceSite},
        workload::Game,
    };
    let mut sites = MegaMan2::CHORDS.sites();
    sites.extend(MegaMan2.move_sites());
    let weapon_rate = ChoiceSite::declared_contrary(&sites)
        .into_iter()
        .find(|(site, _)| *site == CHORD_SITES + WEAPON_SITE)
        .map(|(_, rate)| rate)
        .expect("a declared weapon rate");
    assert_eq!(
        ChoiceSite::declared_contrary(&sites).len(),
        1,
        "only the weapon site carries a declared rate"
    );
    assert_eq!(
        MegaMan2::CHORDS.reproducing_profile().contrary,
        CERTAIN * 3 / 4
    );
    assert_eq!(MegaMan2::CHORDS.reproducing_profile().hold, 0);
    Declared { sites, weapon_rate }
}

fn strategy_ask(
    state: &super::game::Mm2State,
    chooser: &mut crate::search::choice::Chooser,
    previous: Option<ButtonChord>,
) -> Drawn {
    use crate::{chord::ask, mm2::game::MegaMan2, workload::Game};
    let mut records = Vec::new();
    let asked = ask(previous.as_ref());
    let resolved = match MegaMan2.moves(state).offer {
        Some(offer) => offer.resolve(
            &MegaMan2::CHORDS,
            chooser,
            &asked,
            previous.as_ref(),
            &mut records,
        ),
        None => MegaMan2::CHORDS.resolve(chooser, &asked, previous.as_ref(), &mut records),
    }
    .expect("resolve");
    played(resolved)
}

fn offered_targets(state: &super::game::Mm2State) -> (Vec<u16>, Vec<u8>) {
    use crate::{mm2::game::MegaMan2, workload::Game};
    let labels = MegaMan2
        .moves(state)
        .offer
        .map_or_else(Vec::new, |offer| offer.labels());
    let targets = labels
        .iter()
        .filter(|label| **label != 0)
        .map(|label| u8::try_from(label - 1).expect("a weapon label"))
        .collect();
    (labels, targets)
}

fn prefix_states() -> Vec<(&'static str, super::game::Mm2State, Vec<u16>)> {
    use super::{
        game::Mm2State,
        target::{GAMEPLAY_BANK, MENU_CLOSED, PLAYER_STATE_STANDING},
    };
    let holding = |weapon| Mm2State {
        bank: GAMEPLAY_BANK,
        health: 28,
        player_state: PLAYER_STATE_STANDING,
        menu: MENU_CLOSED,
        weapons_obtained: 0x80,
        weapon_energies: [28; 11],
        weapon,
        ..Mm2State::default()
    };
    vec![
        ("root, no move offered", Mm2State::default(), Vec::new()),
        ("buster held, weapon 8 owned", holding(0), vec![0, 9]),
        ("weapon 8 held and masked", holding(8), vec![0, 1]),
    ]
}

const PREVIOUS: [Option<ButtonChord>; 3] = [
    None,
    Some(ButtonChord {
        buttons: 0x81,
        hold_frames: 30,
    }),
    Some(ButtonChord {
        buttons: 0x03,
        hold_frames: 9,
    }),
];

fn judge(name: &str, sides: &[(&str, Vec<(String, f64)>)]) -> usize {
    let k: usize = sides.iter().map(|(_, rows)| rows.len()).sum();
    let limit = upper_quantile(0.025 / k.max(1) as f64);
    let mut failures = 0;
    for (side, rows) in sides {
        let worst = rows
            .iter()
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .cloned();
        let over: Vec<_> = rows.iter().filter(|row| row.1.abs() > limit).collect();
        failures += over.len();
        println!(
            "{{\"setting\":\"{name}\",\"side\":\"{side}\",\"judged\":{},\"z_limit\":{limit:.3},\"worst\":\"{:?}\",\"over\":\"{over:?}\"}}",
            rows.len(),
            worst
        );
    }
    failures
}

#[test]
fn the_declared_moves_draw_what_the_hand_draws_at_the_root_and_at_two_prefix_states() {
    use crate::search::choice::{CERTAIN, ChoicePlan, Chooser};
    const DRAWS: usize = 200_000;
    let declared = declared();
    assert_eq!(declared.sites.len(), usize::from(CHORD_SITES) + 1);
    let strategy_rate = f64::from(declared.weapon_rate) / f64::from(CERTAIN);
    let hand_rate = 1.0 / super::action::SWITCH_ODDS as f64;
    assert!((strategy_rate - hand_rate).abs() < 1.0 / f64::from(CERTAIN));
    let mut failures = 0;
    for (index, (name, state, offered)) in prefix_states().into_iter().enumerate() {
        let (labels, targets) = offered_targets(&state);
        assert_eq!(labels, offered, "{name}");
        let reached: Vec<u8> = state.switch_targets().collect();
        assert_eq!(
            reached
                .iter()
                .map(|weapon| u16::from(*weapon) + 1)
                .collect::<Vec<_>>(),
            offered
                .iter()
                .copied()
                .filter(|label| *label != 0)
                .collect::<Vec<_>>(),
            "{name}"
        );
        for (context, previous) in PREVIOUS.into_iter().enumerate() {
            let seed = 0x6d32_a000 + (index * 16 + context) as u64;
            let mut chooser = Chooser::new(&ChoicePlan {
                profile: MegaMan2Chords::profile(),
                redrawn: false,
                seed,
                untried: None,
                held: Vec::new(),
                declared: crate::search::choice::ChoiceSite::declared_contrary(&declared.sites),
            });
            let strategy: Vec<Drawn> = (0..DRAWS)
                .map(|_| strategy_ask(&state, &mut chooser, previous))
                .collect();
            let mut rand = RomuDuoJrRand::with_seed(seed);
            let hand: Vec<Drawn> = (0..DRAWS)
                .map(|_| {
                    let before = previous.map(Mm2Action::Chord);
                    hand_drawn(hand_draw(&mut rand, before.as_ref()), &targets)
                })
                .collect();
            let setting = format!("A {name}, previous {previous:?}");
            failures += judge(
                &setting,
                &[
                    (
                        "strategy",
                        departures(
                            &strategy,
                            &expected_draws(strategy_rate, &targets, false, previous),
                        ),
                    ),
                    (
                        "hand",
                        departures(&hand, &expected_draws(hand_rate, &targets, true, previous)),
                    ),
                ],
            );
        }
    }
    assert_eq!(failures, 0);
}

struct MegaMan2Chords;

impl MegaMan2Chords {
    fn profile() -> crate::search::choice::ChoiceProfile {
        use crate::{mm2::game::MegaMan2, workload::Game};
        MegaMan2::CHORDS.reproducing_profile()
    }
}

#[test]
fn a_job_first_ask_under_the_shipped_policy_draws_what_the_hand_draws() {
    use crate::{
        mm2::game::MegaMan2,
        search::choice::{CERTAIN, ChoicePlan, Chooser},
        workload::reproducing_choices,
    };
    const JOBS: usize = 200_000;
    let policy = reproducing_choices::<MegaMan2>();
    assert!(!policy.untried_first);
    let declared = declared();
    let strategy_rate = f64::from(declared.weapon_rate) / f64::from(CERTAIN);
    let hand_rate = 1.0 / super::action::SWITCH_ODDS as f64;
    let mut failures = 0;
    for (index, (name, state, _)) in prefix_states().into_iter().enumerate() {
        let (_, targets) = offered_targets(&state);
        for (context, previous) in PREVIOUS.into_iter().enumerate() {
            let campaign_seed = 0x6d32_b000 + (index * 16 + context) as u64;
            let root = policy.root_profile(campaign_seed);
            let mut mutation_seeds = RomuDuoJrRand::with_seed(campaign_seed);
            let seeds: Vec<u64> = (0..JOBS).map(|_| mutation_seeds.next_u64()).collect();
            let strategy: Vec<Drawn> = seeds
                .iter()
                .map(|&mutation_seed| {
                    let mut chooser = Chooser::new(&ChoicePlan::new(
                        &policy,
                        root,
                        mutation_seed,
                        None,
                        Vec::new(),
                        crate::search::choice::ChoiceSite::declared_contrary(&declared.sites),
                    ));
                    strategy_ask(&state, &mut chooser, previous)
                })
                .collect();
            let hand: Vec<Drawn> = seeds
                .iter()
                .map(|&mutation_seed| {
                    let mut rand = RomuDuoJrRand::with_seed(mutation_seed);
                    let before = previous.map(Mm2Action::Chord);
                    hand_drawn(hand_draw(&mut rand, before.as_ref()), &targets)
                })
                .collect();
            let setting = format!("B {name}, previous {previous:?}");
            failures += judge(
                &setting,
                &[
                    (
                        "strategy",
                        departures(
                            &strategy,
                            &expected_draws(strategy_rate, &targets, false, previous),
                        ),
                    ),
                    (
                        "hand",
                        departures(&hand, &expected_draws(hand_rate, &targets, true, previous)),
                    ),
                ],
            );
        }
    }
    assert_eq!(failures, 0);
}
