// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    io::Write,
    num::NonZeroUsize,
    process::{Command, Stdio},
};

use searcher::search::rand::RomuDuoJrRand;
use serde::{Deserialize, Serialize};

use crate::{
    archive::{ALPHABET_SLOTS, BlueArchiveKey},
    progress::MILESTONE_NAMES,
    target::{ACTION_KINDS, BlueAction},
};

pub const ADVICE_POLICY_IDENTIFIER: &str = "jev_choice_over_macro_kinds_by_place_v1";
pub const ADVICE_ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
pub const ADVICE_MODEL: &str = "jev-latest";
pub const ADVICE_KEY_VARIABLE: &str = "TYPESAFE_API_KEY";
pub const ADVICE_TIMEOUT_SECONDS: u32 = 30;

pub const KINDS: usize = ACTION_KINDS.len();
pub const WEIGHT_FLOOR: u16 = 16;
pub const WEIGHT_CEILING: u16 = 256;
const MAX_CONTEXTS_PER_RECORD: usize = 8;
pub const MAX_ADVISED_CONTEXTS: usize = 512;
const CONTEXT_OVERHEAD_BYTES: usize = 64;

const GOAL: &str = "A search is playing Pokemon Blue from a new game. The goal is \
Brock's Boulder Badge in the Pewter City gym. The route is: take a starter from \
Oak's lab, win the rival battle, cross Route 1 to Viridian City, fetch Oak's \
parcel from the Viridian Poke Mart, carry it back to Pallet Town, take the \
Pokedex, go north through Viridian City to Route 2, cross Viridian Forest, reach \
Pewter City, and beat Brock. Each draw picks one macro and the search runs it \
from a saved position.";

const KIND_DESCRIPTIONS: [&str; KINDS] = [
    "walk to a warp, a door, a counter, or a nearby tile on this map, and \
     attack with a move instead when a battle is running",
    "press A at the tile ahead, which opens a conversation with the person or \
     sign there, and advances the text instead when a battle is running",
    "attack with one of the active Pokemon's moves, and walk to a tile on this \
     map instead when no battle is running",
    "use an item from the bag, and press A at the tile ahead instead when no \
     battle is running",
    "switch to another party member, and press A at the tile ahead instead when \
     no battle is running",
    "press B until the game hands control back, which is what carries a \
     conversation from its first text box through to its end, and what runs \
     from a wild battle",
];

pub const MAP_NAMES: [&str; 248] = [
    "pallet_town",
    "viridian_city",
    "pewter_city",
    "cerulean_city",
    "lavender_town",
    "vermilion_city",
    "celadon_city",
    "fuchsia_city",
    "cinnabar_island",
    "indigo_plateau",
    "saffron_city",
    "unused_map_0b",
    "route_1",
    "route_2",
    "route_3",
    "route_4",
    "route_5",
    "route_6",
    "route_7",
    "route_8",
    "route_9",
    "route_10",
    "route_11",
    "route_12",
    "route_13",
    "route_14",
    "route_15",
    "route_16",
    "route_17",
    "route_18",
    "route_19",
    "route_20",
    "route_21",
    "route_22",
    "route_23",
    "route_24",
    "route_25",
    "reds_house_1f",
    "reds_house_2f",
    "blues_house",
    "oaks_lab",
    "viridian_pokecenter",
    "viridian_mart",
    "viridian_school_house",
    "viridian_nickname_house",
    "viridian_gym",
    "digletts_cave_route_2",
    "viridian_forest_north_gatehouse",
    "route_2_trade_house",
    "route_2_gatehouse",
    "viridian_forest_south_gatehouse",
    "viridian_forest",
    "museum_1f",
    "museum_2f",
    "pewter_gym",
    "pewter_nidoran_house",
    "pewter_mart",
    "pewter_speech_house",
    "pewter_pokecenter",
    "mt_moon_1f",
    "mt_moon_b1f",
    "mt_moon_b2f",
    "cerulean_trashed_house",
    "cerulean_trade_house",
    "cerulean_pokecenter",
    "cerulean_gym",
    "bike_shop",
    "cerulean_mart",
    "mt_moon_pokecenter",
    "cerulean_trashed_house_copy",
    "route_5_gatehouse",
    "underground_path_route_5",
    "daycare",
    "route_6_gatehouse",
    "underground_path_route_6",
    "underground_path_route_6_copy",
    "route_7_gatehouse",
    "underground_path_route_7",
    "underground_path_route_7_copy",
    "route_8_gatehouse",
    "underground_path_route_8",
    "rock_tunnel_pokecenter",
    "rock_tunnel_1f",
    "power_plant",
    "route_11_gatehouse_1f",
    "digletts_cave_route_11",
    "route_11_gatehouse_2f",
    "route_12_gatehouse_1f",
    "bills_house",
    "vermilion_pokecenter",
    "pokemon_fan_club",
    "vermilion_mart",
    "vermilion_gym",
    "vermilion_pidgey_house",
    "vermilion_dock",
    "ss_anne_1f",
    "ss_anne_2f",
    "ss_anne_3f",
    "ss_anne_b1f",
    "ss_anne_bow",
    "ss_anne_kitchen",
    "ss_anne_captains_room",
    "ss_anne_1f_rooms",
    "ss_anne_2f_rooms",
    "ss_anne_b1f_rooms",
    "unused_map_69",
    "unused_map_6a",
    "unused_map_6b",
    "victory_road_1f",
    "unused_map_6d",
    "unused_map_6e",
    "unused_map_6f",
    "unused_map_70",
    "lances_room",
    "unused_map_72",
    "unused_map_73",
    "unused_map_74",
    "unused_map_75",
    "hall_of_fame",
    "underground_path_north_south",
    "champions_room",
    "underground_path_west_east",
    "celadon_mart_1f",
    "celadon_mart_2f",
    "celadon_mart_3f",
    "celadon_mart_4f",
    "celadon_mart_roof",
    "celadon_mart_elevator",
    "celadon_mansion_1f",
    "celadon_mansion_2f",
    "celadon_mansion_3f",
    "celadon_mansion_roof",
    "celadon_mansion_roof_house",
    "celadon_pokecenter",
    "celadon_gym",
    "game_corner",
    "celadon_mart_5f",
    "game_corner_prize_room",
    "celadon_diner",
    "celadon_chief_house",
    "celadon_hotel",
    "lavender_pokecenter",
    "pokemon_tower_1f",
    "pokemon_tower_2f",
    "pokemon_tower_3f",
    "pokemon_tower_4f",
    "pokemon_tower_5f",
    "pokemon_tower_6f",
    "pokemon_tower_7f",
    "mr_fujis_house",
    "lavender_mart",
    "lavender_cubone_house",
    "fuchsia_mart",
    "fuchsia_bills_grandpas_house",
    "fuchsia_pokecenter",
    "wardens_house",
    "safari_zone_gatehouse",
    "fuchsia_gym",
    "fuchsia_meeting_room",
    "seafoam_islands_b1f",
    "seafoam_islands_b2f",
    "seafoam_islands_b3f",
    "seafoam_islands_b4f",
    "vermilion_old_rod_house",
    "fuchsia_good_rod_house",
    "pokemon_mansion_1f",
    "cinnabar_gym",
    "cinnabar_lab",
    "cinnabar_lab_trade_room",
    "cinnabar_lab_metronome_room",
    "cinnabar_lab_fossil_room",
    "cinnabar_pokecenter",
    "cinnabar_mart",
    "cinnabar_mart_copy",
    "indigo_plateau_lobby",
    "copycats_house_1f",
    "copycats_house_2f",
    "fighting_dojo",
    "saffron_gym",
    "saffron_pidgey_house",
    "saffron_mart",
    "silph_co_1f",
    "saffron_pokecenter",
    "mr_psychics_house",
    "route_15_gatehouse_1f",
    "route_15_gatehouse_2f",
    "route_16_gatehouse_1f",
    "route_16_gatehouse_2f",
    "route_16_fly_house",
    "route_12_super_rod_house",
    "route_18_gatehouse_1f",
    "route_18_gatehouse_2f",
    "seafoam_islands_1f",
    "route_22_gatehouse",
    "victory_road_2f",
    "route_12_gatehouse_2f",
    "vermilion_trade_house",
    "digletts_cave",
    "victory_road_3f",
    "rocket_hideout_b1f",
    "rocket_hideout_b2f",
    "rocket_hideout_b3f",
    "rocket_hideout_b4f",
    "rocket_hideout_elevator",
    "unused_map_cc",
    "unused_map_cd",
    "unused_map_ce",
    "silph_co_2f",
    "silph_co_3f",
    "silph_co_4f",
    "silph_co_5f",
    "silph_co_6f",
    "silph_co_7f",
    "silph_co_8f",
    "pokemon_mansion_2f",
    "pokemon_mansion_3f",
    "pokemon_mansion_b1f",
    "safari_zone_east",
    "safari_zone_north",
    "safari_zone_west",
    "safari_zone_center",
    "safari_zone_center_rest_house",
    "safari_zone_secret_house",
    "safari_zone_west_rest_house",
    "safari_zone_east_rest_house",
    "safari_zone_north_rest_house",
    "cerulean_cave_2f",
    "cerulean_cave_b1f",
    "cerulean_cave_1f",
    "name_raters_house",
    "cerulean_badge_house",
    "unused_map_e7",
    "rock_tunnel_b1f",
    "silph_co_9f",
    "silph_co_10f",
    "silph_co_11f",
    "silph_co_elevator",
    "unused_map_ed",
    "unused_map_ee",
    "trade_center",
    "colosseum",
    "unused_map_f1",
    "unused_map_f2",
    "unused_map_f3",
    "unused_map_f4",
    "loreleis_room",
    "brunos_room",
    "agathas_room",
];

#[must_use]
pub fn map_name(map: u8) -> &'static str {
    MAP_NAMES
        .get(usize::from(map))
        .copied()
        .unwrap_or("unknown_map")
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct AdviceContext {
    pub badges: u8,
    pub route: u8,
    pub map: u8,
}

impl AdviceContext {
    #[must_use]
    pub fn of(key: BlueArchiveKey) -> Self {
        Self {
            badges: key.badges,
            route: key.route,
            map: key.map,
        }
    }

    #[must_use]
    pub fn key(self) -> BlueArchiveKey {
        BlueArchiveKey {
            badges: self.badges,
            route: self.route,
            map: self.map,
            ..BlueArchiveKey::default()
        }
    }

    fn reached(self) -> Vec<&'static str> {
        MILESTONE_NAMES
            .iter()
            .enumerate()
            .filter(|(bit, _)| self.route & (1 << bit) != 0)
            .map(|(_, name)| *name)
            .collect()
    }

    fn next_milestone(self) -> &'static str {
        MILESTONE_NAMES
            .iter()
            .enumerate()
            .find(|(bit, _)| self.route & (1 << bit) == 0)
            .map_or("boulder_badge", |(_, name)| *name)
    }

    fn state(self) -> serde_json::Value {
        serde_json::json!({
            "goal": GOAL,
            "map": map_name(self.map),
            "map_id": self.map,
            "badges": self.badges,
            "reached": self.reached(),
            "next_milestone": self.next_milestone(),
        })
    }
}

pub type AdviceWeights = [u16; KINDS];

#[must_use]
pub fn context_bytes() -> usize {
    std::mem::size_of::<AdviceContext>()
        + std::mem::size_of::<AdviceWeights>()
        + CONTEXT_OVERHEAD_BYTES
}

#[must_use]
pub fn reserve_bytes() -> usize {
    MAX_ADVISED_CONTEXTS * context_bytes()
}

#[must_use]
pub fn uniform_weights() -> AdviceWeights {
    [WEIGHT_FLOOR; KINDS]
}

#[must_use]
fn weight_of(probability: f64) -> u16 {
    let share = probability.clamp(0.0, 1.0);
    let span = f64::from(WEIGHT_CEILING - WEIGHT_FLOOR);
    WEIGHT_FLOOR.saturating_add(u16::try_from((share * span).round() as i64).unwrap_or(0))
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct AdviceCheckpoint {
    pub policy: String,
    pub records: u64,
    pub calls: u64,
    pub failures: u64,
    pub input_tokens: u64,
    pub weights: Vec<(AdviceContext, AdviceWeights)>,
}

impl AdviceCheckpoint {
    #[must_use]
    pub fn weights_for(&self, context: AdviceContext) -> AdviceWeights {
        self.weights
            .iter()
            .find(|(recorded, _)| *recorded == context)
            .map_or_else(uniform_weights, |(_, weights)| *weights)
    }
}

#[derive(Clone, Debug, Default)]
pub struct AdviceTable {
    weights: BTreeMap<AdviceContext, AdviceWeights>,
    records: u64,
    calls: u64,
    failures: u64,
    input_tokens: u64,
}

impl AdviceTable {
    #[must_use]
    pub fn weights_for(&self, context: AdviceContext) -> AdviceWeights {
        self.weights
            .get(&context)
            .copied()
            .unwrap_or_else(uniform_weights)
    }

    #[must_use]
    pub fn advised(&self) -> usize {
        self.weights.len()
    }

    #[must_use]
    pub fn calls(&self) -> u64 {
        self.calls
    }

    #[must_use]
    pub fn failures(&self) -> u64 {
        self.failures
    }

    #[must_use]
    pub fn input_tokens(&self) -> u64 {
        self.input_tokens
    }

    #[must_use]
    pub fn memory_bytes(&self) -> usize {
        self.weights.len() * context_bytes()
    }

    pub fn fill(&mut self, adviser: &BlueAdviser, pending: &BTreeSet<AdviceContext>) {
        let wanted = pending
            .iter()
            .filter(|context| !self.weights.contains_key(context))
            .copied()
            .take(MAX_CONTEXTS_PER_RECORD)
            .collect::<Vec<_>>();
        for context in &wanted {
            if self.weights.len() >= MAX_ADVISED_CONTEXTS {
                break;
            }
            self.calls += 1;
            match adviser.ask(*context) {
                Ok((weights, tokens)) => {
                    self.input_tokens += tokens;
                    self.weights.insert(*context, weights);
                }
                Err(_) => {
                    self.failures += 1;
                    self.weights.insert(*context, uniform_weights());
                }
            }
        }
    }

    #[must_use]
    pub fn finish_record(&mut self) -> AdviceCheckpoint {
        self.records += 1;
        self.checkpoint()
    }

    #[must_use]
    pub fn checkpoint(&self) -> AdviceCheckpoint {
        AdviceCheckpoint {
            policy: ADVICE_POLICY_IDENTIFIER.to_owned(),
            records: self.records,
            calls: self.calls,
            failures: self.failures,
            input_tokens: self.input_tokens,
            weights: self
                .weights
                .iter()
                .map(|(context, weights)| (*context, *weights))
                .collect(),
        }
    }

    pub fn load(&mut self, checkpoint: &AdviceCheckpoint) -> Result<(), Box<dyn Error>> {
        if checkpoint.policy != ADVICE_POLICY_IDENTIFIER {
            return Err("Blue stream carries an unknown adviser policy".into());
        }
        self.records = checkpoint.records;
        self.calls = checkpoint.calls;
        self.failures = checkpoint.failures;
        self.input_tokens = checkpoint.input_tokens;
        self.weights = checkpoint.weights.iter().copied().collect();
        Ok(())
    }
}

pub struct BlueAdviser {
    key: String,
    endpoint: String,
    model: String,
}

impl BlueAdviser {
    #[must_use]
    pub fn from_environment() -> Option<Self> {
        let key = std::env::var(ADVICE_KEY_VARIABLE)
            .ok()
            .filter(|value| !value.trim().is_empty())?;
        Some(Self {
            key,
            endpoint: ADVICE_ENDPOINT.to_owned(),
            model: ADVICE_MODEL.to_owned(),
        })
    }

    fn body(&self, context: AdviceContext) -> serde_json::Value {
        let criteria = ACTION_KINDS
            .iter()
            .enumerate()
            .map(|(index, kind)| {
                (
                    (*kind).name().to_owned(),
                    serde_json::Value::from(KIND_DESCRIPTIONS[index]),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        serde_json::json!({
            "state": context.state(),
            "model": self.model,
            "questions": {
                "pick": {
                    "type": "choice",
                    "instructions": format!(
                        "The search is standing in {}. Which macro should the draw pick most often to reach {} soonest?",
                        map_name(context.map),
                        context.next_milestone(),
                    ),
                    "criteria": criteria,
                }
            },
        })
    }

    fn ask(&self, context: AdviceContext) -> Result<(AdviceWeights, u64), Box<dyn Error>> {
        let body = serde_json::to_vec(&self.body(context))?;
        let path = std::env::temp_dir().join(format!(
            "blue-advice-{}-{}-{}.json",
            std::process::id(),
            context.map,
            context.route,
        ));
        std::fs::write(&path, &body)?;
        let answer = self.post(&path);
        let _ = std::fs::remove_file(&path);
        let answer: serde_json::Value = serde_json::from_slice(&answer?)?;
        let probabilities = answer
            .get("answers")
            .and_then(|answers| answers.get("pick"))
            .and_then(|pick| pick.get("probabilities"))
            .ok_or("Jev answered without a probability for the macro choice")?;
        let mut weights = uniform_weights();
        for (index, kind) in ACTION_KINDS.iter().enumerate() {
            let probability = probabilities
                .get((*kind).name())
                .and_then(serde_json::Value::as_f64)
                .ok_or("Jev answered without a probability for every macro")?;
            weights[index] = weight_of(probability);
        }
        let tokens = answer
            .get("usage")
            .and_then(|usage| usage.get("input_tokens"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        Ok((weights, tokens))
    }

    fn post(&self, body: &std::path::Path) -> Result<Vec<u8>, Box<dyn Error>> {
        let mut child = Command::new("curl")
            .arg("--disable")
            .arg("--config")
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let config = format!(
            "silent\nshow-error\nfail\nmax-time = \"{ADVICE_TIMEOUT_SECONDS}\"\nurl = \"{}\"\n\
             header = \"Authorization: Bearer {}\"\nheader = \"content-type: application/json\"\n\
             data-binary = \"@{}\"\n",
            self.endpoint,
            self.key,
            body.display(),
        );
        child
            .stdin
            .take()
            .ok_or("curl refused a request body")?
            .write_all(config.as_bytes())?;
        let finished = child.wait_with_output()?;
        if !finished.status.success() {
            return Err("Jev refused the request".into());
        }
        Ok(finished.stdout)
    }
}

pub fn sample_advised_action(
    rand: &mut RomuDuoJrRand,
    weights: AdviceWeights,
) -> Result<BlueAction, Box<dyn Error>> {
    let total: u32 = weights.iter().copied().map(u32::from).sum();
    let span = NonZeroUsize::new(usize::try_from(total)?).ok_or("empty macro weight")?;
    let mut drawn = u32::try_from(rand.below(span))?;
    let mut chosen = ACTION_KINDS[KINDS - 1];
    for (index, kind) in ACTION_KINDS.iter().enumerate() {
        let weight = u32::from(weights[index]);
        if drawn < weight {
            chosen = *kind;
            break;
        }
        drawn -= weight;
    }
    let slots = NonZeroUsize::new(ALPHABET_SLOTS).ok_or("empty alphabet slot range")?;
    Ok(BlueAction::new(chosen, u8::try_from(rand.below(slots))?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_map_id_has_a_name() {
        assert_eq!(MAP_NAMES.len(), 248);
        assert_eq!(map_name(0), "pallet_town");
        assert_eq!(map_name(54), "pewter_gym");
        assert_eq!(map_name(255), "unknown_map");
    }

    #[test]
    fn the_next_milestone_is_the_first_one_not_reached() {
        let context = AdviceContext {
            badges: 0,
            route: 0,
            map: 0,
        };
        assert_eq!(context.next_milestone(), "got_starter");
        let started = AdviceContext {
            route: 1,
            ..context
        };
        assert_eq!(started.next_milestone(), "got_parcel");
        assert_eq!(started.reached(), vec!["got_starter"]);
    }

    #[test]
    fn a_probability_becomes_a_weight_between_the_floor_and_the_ceiling() {
        assert_eq!(weight_of(-1.0), WEIGHT_FLOOR);
        assert_eq!(weight_of(0.0), WEIGHT_FLOOR);
        assert_eq!(weight_of(1.0), WEIGHT_CEILING);
        assert_eq!(weight_of(2.0), WEIGHT_CEILING);
        assert!(weight_of(0.5) > WEIGHT_FLOOR);
        assert!(weight_of(0.5) < WEIGHT_CEILING);
    }

    #[test]
    fn an_unadvised_place_draws_every_macro_evenly() {
        let table = AdviceTable::default();
        let context = AdviceContext {
            badges: 0,
            route: 0,
            map: 12,
        };
        assert_eq!(table.weights_for(context), uniform_weights());
        let mut rand = RomuDuoJrRand::with_seed(7);
        let mut counts = BTreeMap::new();
        for _ in 0..6_000 {
            let action = sample_advised_action(&mut rand, uniform_weights()).unwrap();
            *counts.entry(action.kind).or_insert(0_u32) += 1;
        }
        assert_eq!(counts.len(), KINDS);
        for count in counts.values() {
            assert!((800..1_200).contains(count), "{counts:?}");
        }
    }

    #[test]
    fn a_weighted_place_still_draws_every_macro() {
        let mut weights = uniform_weights();
        weights[0] = WEIGHT_CEILING;
        let mut rand = RomuDuoJrRand::with_seed(11);
        let mut counts = BTreeMap::new();
        for _ in 0..6_000 {
            let action = sample_advised_action(&mut rand, weights).unwrap();
            *counts.entry(action.kind).or_insert(0_u32) += 1;
        }
        assert_eq!(counts.len(), KINDS);
        assert!(counts[&ACTION_KINDS[0]] > 3_000, "{counts:?}");
        for kind in &ACTION_KINDS[1..] {
            assert!(counts[kind] > 100, "{counts:?}");
        }
    }

    #[test]
    fn a_checkpoint_round_trips_through_the_table() {
        let mut table = AdviceTable::default();
        let context = AdviceContext {
            badges: 0,
            route: 3,
            map: 13,
        };
        let mut weights = uniform_weights();
        weights[2] = WEIGHT_CEILING;
        table.weights.insert(context, weights);
        let checkpoint = table.finish_record();
        assert_eq!(checkpoint.records, 1);
        assert_eq!(checkpoint.weights_for(context), weights);
        let mut restored = AdviceTable::default();
        restored.load(&checkpoint).unwrap();
        assert_eq!(restored.weights_for(context), weights);
        assert_eq!(restored.advised(), 1);
    }

    #[test]
    fn an_answered_place_does_not_hold_a_slot_in_the_next_round() {
        let mut table = AdviceTable::default();
        let answered = (0..MAX_CONTEXTS_PER_RECORD)
            .map(|index| AdviceContext {
                badges: 0,
                route: 0,
                map: u8::try_from(index).unwrap(),
            })
            .collect::<Vec<_>>();
        for context in &answered {
            table.weights.insert(*context, uniform_weights());
        }
        let fresh = AdviceContext {
            badges: 0,
            route: 3,
            map: 42,
        };
        let mut pending = answered.iter().copied().collect::<BTreeSet<_>>();
        pending.insert(fresh);
        let wanted = pending
            .iter()
            .filter(|context| !table.weights.contains_key(context))
            .copied()
            .take(MAX_CONTEXTS_PER_RECORD)
            .collect::<Vec<_>>();
        assert_eq!(wanted, vec![fresh]);
    }

    #[test]
    fn a_foreign_checkpoint_is_refused() {
        let checkpoint = AdviceCheckpoint {
            policy: "something else".to_owned(),
            ..AdviceCheckpoint::default()
        };
        assert!(AdviceTable::default().load(&checkpoint).is_err());
    }
}
