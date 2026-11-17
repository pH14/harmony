// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    error::Error,
};

use machine::nes::ButtonChord;
use nes_games::{
    chord::{ChordVocabulary, ask},
    donkey_kong::DonkeyKong,
    famidash::Famidash,
    ice_climbers::IceClimbers,
    joust::Joust,
    pac_man::PacMan,
    punchout::PunchOut,
    search::{
        choice::{ChoicePlan, Chooser},
        rand::RomuDuoJrRand,
    },
    twin_dragons::TwinDragons,
    workload::Game,
    zelda::Zelda,
};

const CHAINS: u64 = 100_000;
const CHAIN_LENGTH: u64 = 10;
const Z_LIMIT: f64 = 4.5;
const PAIR_MINIMUM: u64 = 50;
const NO_CHORD: u8 = 0xff;

type Pair = (u8, u8);

#[derive(Clone, Copy, Default)]
struct Chain {
    chord_pairs: [Pair; CHAIN_LENGTH as usize],
    hold_pairs: [Pair; CHAIN_LENGTH as usize],
}

#[derive(Default)]
struct Counts {
    draws: u64,
    bits: [u64; 8],
    chords: BTreeMap<u8, u64>,
    holds: BTreeMap<u8, u64>,
    chains: Vec<Chain>,
}

impl Counts {
    fn add(
        &mut self,
        chain: &mut Chain,
        step: usize,
        previous: Option<ButtonChord>,
        chord: ButtonChord,
    ) {
        self.draws += 1;
        let (previous_buttons, previous_hold) = previous.map_or((NO_CHORD, 0), |previous| {
            (previous.buttons, previous.hold_frames)
        });
        chain.chord_pairs[step] = (previous_buttons, chord.buttons);
        chain.hold_pairs[step] = (previous_hold, chord.hold_frames);
        for (bit, count) in self.bits.iter_mut().enumerate() {
            *count += u64::from(chord.buttons >> bit & 1);
        }
        *self.chords.entry(chord.buttons).or_default() += 1;
        *self.holds.entry(chord.hold_frames).or_default() += 1;
    }
}

fn starts(chords: &ChordVocabulary) -> Vec<Option<ButtonChord>> {
    let mut starts = vec![None];
    for &held in chords.held {
        starts.push(Some(ButtonChord::new(held, 30)));
        if let Some(tap) = chords.tap {
            starts.push(Some(ButtonChord::new(held | tap.buttons, 1)));
        }
    }
    starts
}

fn main_side(chords: &ChordVocabulary, seed: u64) -> Result<Counts, Box<dyn Error>> {
    let starts = starts(chords);
    let mut rand = RomuDuoJrRand::with_seed(seed);
    let mut counts = Counts::default();
    for chain in 0..CHAINS {
        let mut record = Chain::default();
        let mut previous = starts[(chain % starts.len() as u64) as usize];
        for step in 0..CHAIN_LENGTH as usize {
            let next = chords.draw(&mut rand, previous.as_ref())?;
            counts.add(&mut record, step, previous, next);
            previous = Some(next);
        }
        counts.chains.push(record);
    }
    Ok(counts)
}

fn branch_side(chords: &ChordVocabulary, seed: u64) -> Result<Counts, Box<dyn Error>> {
    let starts = starts(chords);
    let mut seeds = RomuDuoJrRand::with_seed(seed);
    let mut counts = Counts::default();
    for chain in 0..CHAINS {
        let mut chooser = Chooser::new(&ChoicePlan {
            profile: chords.reproducing_profile(),
            redrawn: false,
            seed: seeds.next_u64(),
            untried: None,
            held: Vec::new(),
        });
        let mut record = Chain::default();
        let parent = starts[(chain % starts.len() as u64) as usize];
        let mut previous = None;
        for step in 0..CHAIN_LENGTH as usize {
            let asked = ask(previous.as_ref().or(parent.as_ref()));
            let mut records = Vec::new();
            let next = chords.resolve(&mut chooser, &asked, previous.as_ref(), &mut records)?;
            counts.add(&mut record, step, previous.or(parent), next);
            previous = Some(next);
        }
        counts.chains.push(record);
    }
    Ok(counts)
}

fn compare(
    game: &str,
    measure: &str,
    category: String,
    (main, branch): (u64, u64),
    draws: (u64, u64),
    failures: &mut u64,
) {
    let (n1, n2) = (draws.0 as f64, draws.1 as f64);
    let (p1, p2) = (main as f64 / n1, branch as f64 / n2);
    let pooled = (main + branch) as f64 / (n1 + n2);
    let z = if pooled > 0.0 && pooled < 1.0 {
        (p2 - p1) / (pooled * (1.0 - pooled) * (1.0 / n1 + 1.0 / n2)).sqrt()
    } else {
        0.0
    };
    let one_sided_zero = (main == 0) != (branch == 0);
    let pass = z.abs() <= Z_LIMIT && !one_sided_zero;
    *failures += u64::from(!pass);
    println!(
        "{{\"game\":\"{game}\",\"measure\":\"{measure}\",\"category\":\"{category}\",\"main\":{main},\"branch\":{branch},\"main_share\":{p1:.6},\"branch_share\":{p2:.6},\"z\":{z:.3},\"pass\":{pass}}}"
    );
}

fn compare_pairs(
    game: &str,
    measure: &str,
    (main, branch): (&Counts, &Counts),
    pairs_of: fn(&Chain) -> &[Pair; CHAIN_LENGTH as usize],
    failures: &mut u64,
) {
    let mut tallies: BTreeMap<Pair, (u64, u64)> = BTreeMap::new();
    for (index, chain) in main.chains.iter().enumerate() {
        tallies
            .entry(pairs_of(chain)[index % CHAIN_LENGTH as usize])
            .or_default()
            .0 += 1;
    }
    for (index, chain) in branch.chains.iter().enumerate() {
        tallies
            .entry(pairs_of(chain)[index % CHAIN_LENGTH as usize])
            .or_default()
            .1 += 1;
    }
    let draws = (main.chains.len() as u64, branch.chains.len() as u64);
    let mut cells = Vec::new();
    let mut rare = (0, 0);
    let mut cell_failures = 0;
    for (pair, counts) in tallies {
        if counts.0 + counts.1 < PAIR_MINIMUM {
            rare = (rare.0 + counts.0, rare.1 + counts.1);
            continue;
        }
        compare(
            game,
            measure,
            format!("{:02x}>{:02x}", pair.0, pair.1),
            counts,
            draws,
            &mut cell_failures,
        );
        cells.push(counts);
    }
    cells.push(rare);
    cells.retain(|(main, branch)| main + branch > 0);
    let (n1, n2) = (draws.0 as f64, draws.1 as f64);
    let statistic: f64 = cells
        .iter()
        .map(|&(main, branch)| {
            let total = (main + branch) as f64;
            let expected = (total * n1 / (n1 + n2), total * n2 / (n1 + n2));
            (main as f64 - expected.0).powi(2) / expected.0
                + (branch as f64 - expected.1).powi(2) / expected.1
        })
        .sum();
    let df = cells.len().saturating_sub(1);
    let z = if df == 0 {
        0.0
    } else {
        let df = df as f64;
        let shape = 2.0 / (9.0 * df);
        ((statistic / df).cbrt() - (1.0 - shape)) / shape.sqrt()
    };
    let pass = df > 0 && z <= Z_LIMIT;
    *failures += u64::from(!pass);
    println!(
        "{{\"game\":\"{game}\",\"measure\":\"{measure}\",\"category\":\"chi_square\",\"pairs_per_side\":{},\"statistic\":{statistic:.1},\"df\":{df},\"cells_beyond_z_limit\":{cell_failures},\"z\":{z:.3},\"pass\":{pass}}}",
        draws.0
    );
}

fn check(game: &str, chords: &ChordVocabulary, seed: u64) -> Result<u64, Box<dyn Error>> {
    let main = main_side(chords, seed)?;
    let branch = branch_side(chords, seed ^ 0x9e37_79b9_7f4a_7c15)?;
    let draws = (main.draws, branch.draws);
    let mut failures = 0;
    for bit in 0..8 {
        compare(
            game,
            "button",
            format!("bit{bit}"),
            (main.bits[bit], branch.bits[bit]),
            draws,
            &mut failures,
        );
    }
    let chords_seen: BTreeSet<u8> = main
        .chords
        .keys()
        .chain(branch.chords.keys())
        .copied()
        .collect();
    for buttons in chords_seen {
        compare(
            game,
            "chord",
            format!("{buttons:02x}"),
            (
                main.chords.get(&buttons).copied().unwrap_or(0),
                branch.chords.get(&buttons).copied().unwrap_or(0),
            ),
            draws,
            &mut failures,
        );
    }
    let holds_seen: BTreeSet<u8> = main
        .holds
        .keys()
        .chain(branch.holds.keys())
        .copied()
        .collect();
    for hold in holds_seen {
        compare(
            game,
            "hold",
            hold.to_string(),
            (
                main.holds.get(&hold).copied().unwrap_or(0),
                branch.holds.get(&hold).copied().unwrap_or(0),
            ),
            draws,
            &mut failures,
        );
    }
    compare_pairs(
        game,
        "chord_pair",
        (&main, &branch),
        |chain| &chain.chord_pairs,
        &mut failures,
    );
    compare_pairs(
        game,
        "hold_pair",
        (&main, &branch),
        |chain| &chain.hold_pairs,
        &mut failures,
    );
    Ok(failures)
}

const GAMES: [&str; 8] = [
    "joust",
    "famidash",
    "zelda",
    "donkey-kong",
    "ice-climbers",
    "pac-man",
    "punchout",
    "twin-dragons",
];

fn vocabulary(game: &str) -> Result<ChordVocabulary, Box<dyn Error>> {
    Ok(match game {
        "joust" => Joust::CHORDS,
        "famidash" => <Famidash<0> as Game>::CHORDS,
        "zelda" => Zelda::CHORDS,
        "donkey-kong" => DonkeyKong::CHORDS,
        "ice-climbers" => IceClimbers::CHORDS,
        "pac-man" => PacMan::CHORDS,
        "punchout" => PunchOut::CHORDS,
        "twin-dragons" => TwinDragons::CHORDS,
        other => return Err(format!("no chord vocabulary for {other}").into()),
    })
}

fn main() -> Result<(), Box<dyn Error>> {
    let seed: u64 = env::args().nth(1).map_or(Ok(7), |seed| seed.parse())?;
    let chosen: Vec<String> = env::args().skip(2).collect();
    let games: Vec<&str> = if chosen.is_empty() {
        GAMES.to_vec()
    } else {
        chosen.iter().map(String::as_str).collect()
    };
    let mut failures = 0;
    for game in games {
        let offset = GAMES.iter().position(|known| *known == game).unwrap_or(0) as u64;
        failures += check(game, &vocabulary(game)?, seed.wrapping_add(offset))?;
    }
    println!(
        "{{\"failures\":{failures},\"z_limit\":{Z_LIMIT},\"draws_per_side\":{}}}",
        CHAINS * CHAIN_LENGTH
    );
    Ok(())
}
