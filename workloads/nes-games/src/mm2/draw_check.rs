// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;

use machine::nes::ButtonChord;

use super::{
    action::{Mm2Action, switch_choice},
    game::WEAPON_SITE,
};
use crate::{
    chord::CHORD_SITES,
    search::{
        assertion::{RecordedAction, Recording},
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

fn strategy_drawn(action: &RecordedAction<ButtonChord>) -> Drawn {
    match move_answer(&action.action) {
        Some((site, label)) if site == WEAPON_SITE => Drawn::Switch(label),
        Some(_) => Drawn::Idle,
        None => Drawn::Chord(action.action),
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

fn tally_job(side: &mut Side, job: &[(Drawn, bool)]) {
    let mut previous: Option<(Drawn, bool)> = None;
    for (index, &(drawn, targeted)) in job.iter().enumerate() {
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
            previous = Some((drawn, targeted));
            continue;
        };
        match previous {
            None if index == 0 => side.count("context", "context:first_in_job".to_owned()),
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
            None => {}
        }
        previous = Some((drawn, targeted));
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

#[test]
#[ignore = "needs HARMONY_MM2_RECORDS from a strategy run with records, and HARMONY_MM2_HAND_SEED"]
fn strategy_draws_match_the_hand_draw_on_recorded_jobs() {
    let path = std::env::var_os("HARMONY_MM2_RECORDS").expect("HARMONY_MM2_RECORDS");
    let seed: u64 = std::env::var("HARMONY_MM2_HAND_SEED")
        .expect("HARMONY_MM2_HAND_SEED")
        .parse()
        .expect("a seed");
    let recording = Recording::<ButtonChord>::from_bytes(&std::fs::read(path).expect("records"))
        .expect("decode");
    let mut rand = RomuDuoJrRand::with_seed(seed);
    let (mut strategy, mut hand) = (Side::default(), Side::default());
    let (mut asks, mut untargeted, mut actions) = (0_u64, 0_u64, 0_u64);
    let mut jobs: BTreeMap<u64, Vec<&RecordedAction<ButtonChord>>> = BTreeMap::new();
    for action in &recording.actions {
        jobs.entry(action.sequence).or_default().push(action);
    }
    for job in jobs.values() {
        let mut strategy_job = Vec::with_capacity(job.len());
        let mut hand_job = Vec::with_capacity(job.len());
        let mut previous = None;
        for action in job {
            let ask = ask_of(action);
            actions += 1;
            asks += u64::from(ask.weapon_record);
            untargeted += u64::from(ask.targets.is_empty());
            let targeted = !ask.targets.is_empty();
            strategy_job.push((strategy_drawn(action), targeted));
            let drawn = hand_draw(&mut rand, previous.as_ref());
            previous = Some(drawn);
            hand_job.push((hand_drawn(drawn, &ask.targets), targeted));
        }
        tally_job(&mut strategy, &strategy_job);
        tally_job(&mut hand, &hand_job);
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
        "{{\"jobs\":{},\"actions\":{actions},\"weapon_asks\":{asks},\"untargeted_actions\":{untargeted},\"judged\":{k},\"z_limit\":{limit:.3},\"failures\":{failures}}}",
        jobs.len()
    );
    assert_eq!(failures, 0);
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
        let targets: Vec<u8> = MegaMan2.decode(None, &wram).switch_targets().collect();
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
