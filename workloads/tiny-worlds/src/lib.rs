// SPDX-License-Identifier: AGPL-3.0-or-later

pub mod actions;
pub mod assertions;
pub mod backtrack;
pub mod chain;
pub mod crossing;
pub mod deadline;
pub mod deadline_actions;
pub mod delayed;
pub mod fork;
pub mod graph;
pub mod held;
pub mod map;
pub mod maze;
pub mod passive_clock;
pub mod picks;
pub mod resource;
pub mod ridge;
pub mod route;
pub mod stairs;
pub mod targets;
pub mod trap;
pub mod worlds;
use worlds::{State, World};

pub const STAGE_PLACES: u16 = 1024;
pub const ARCHIVE_FORMAT: &str = "tiny-world-archive-v1";

#[derive(Clone, Copy, Debug, Default)]
pub struct SearchSettings {
    pub mixture: Option<searcher::search::draw::DrawMixture>,
    pub stop_on_objective: Option<bool>,
}

impl SearchSettings {
    fn apply<K: Keying>(self, config: &mut CampaignConfig<Workload<K>>) {
        if let Some(mixture) = self.mixture {
            config.mixture = mixture;
        }
        if let Some(stop) = self.stop_on_objective {
            config.stop_campaign_on_objective = stop;
        }
    }
}

type StreamRecord = CampaignStreamRecord<serde_json::Value, serde_json::Value>;

fn stream_lines(stream: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut lines = stream
        .split(|&c| c == b'\n')
        .filter(|line| !line.is_empty())
        .peekable();
    if lines.peek().is_some_and(|line| {
        serde_json::from_slice::<CampaignStreamHeader<serde_json::Value>>(line).is_ok()
    }) {
        lines.next();
    }
    lines
}

fn stream_records(stream: &[u8]) -> Result<Vec<StreamRecord>, Box<dyn Error>> {
    Ok(stream_lines(stream)
        .map(serde_json::from_slice)
        .collect::<Result<_, _>>()?)
}

const WORKLOAD_FIELDS: [&str; 3] = [
    "result_sha256",
    "draw_checkpoint_before",
    "draw_checkpoint_after",
];

fn decision_stream(stream: &[u8]) -> Result<(String, u64), Box<dyn Error>> {
    let mut canonical = Vec::new();
    let mut records = 0;
    for line in stream_lines(stream) {
        let mut record: serde_json::Value = serde_json::from_slice(line)?;
        if let Some(fields) = record.as_object_mut() {
            for field in WORKLOAD_FIELDS {
                fields.remove(field);
            }
            if let Some(draw) = fields
                .get_mut("duration_draw")
                .and_then(serde_json::Value::as_object_mut)
            {
                draw.remove("context");
            }
        }
        serde_json::to_writer(&mut canonical, &record)?;
        records += 1;
    }
    Ok((postcard_value_sha256(&canonical)?, records))
}

const MAX_REACHABLE_STATES: usize = 200_000;

fn reachable<S: Copy + Ord>(
    initial: S,
    goal: impl Fn(S) -> bool,
    step: impl Fn(S, u8) -> S,
) -> Result<bool, String> {
    let mut seen = std::collections::BTreeSet::from([initial]);
    let mut pending = std::collections::VecDeque::from([initial]);
    while let Some(state) = pending.pop_front() {
        if goal(state) {
            return Ok(true);
        }
        for action in 0..4 {
            let next = step(state, action);
            if seen.insert(next) {
                if seen.len() > MAX_REACHABLE_STATES {
                    return Err(format!(
                        "reachability search exceeded {MAX_REACHABLE_STATES} states"
                    ));
                }
                pending.push_back(next);
            }
        }
    }
    Ok(false)
}

fn pattern_action(pattern: u64, position: u8) -> u8 {
    ((pattern >> (2 * u32::from(position))) & 3) as u8
}

fn path_name(path: SelectorPath) -> &'static str {
    match path {
        SelectorPath::Continuation => "continuation",
        SelectorPath::Tiers => "tiers",
        SelectorPath::Recent => "recent",
        SelectorPath::Shrink => "shrink",
    }
}

use searcher::search::{
    archive::{
        ArchiveEntryReport, ArchiveKey, Input, KeyShape, MAX_ARCHIVE_ENTRIES, RetentionPolicy,
        SelectorPath, entries_by_suffix,
    },
    assertion::{
        Accumulator, AssertionKey, Compiled, Record, ShadowCursor, ShadowReport, ShadowTable,
        Strategy,
    },
    campaign::{
        ArchiveFormat, ArchiveFormatName, ArchiveReportState, CampaignActionResult,
        CampaignAdmissionDecision, CampaignConfig, CampaignExecutionOptions, CampaignJobResult,
        CampaignOrigin, CampaignStreamHeader, CampaignStreamRecord, CampaignTypes, Evaluation,
        InputPolicy, Reporting, TargetExecution, WorkloadPolicies, postcard_value_sha256,
        replay_campaign_checkpointed, run_campaign_checkpointed_with_options,
    },
    choice::{ChoicePlan, ChoicePolicy, ChoiceRecord, ChoiceSite, Chooser},
    rand::RomuDuoJrRand,
    rollout::ExecutionDisposition,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{error::Error, io::Write, num::NonZeroUsize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Keep {
    #[default]
    Portfolio,
    CapacityTwo,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Key<const CAPACITY_TWO: bool = false> {
    pub stock: u8,
    pub place: u16,
    pub context: u16,
    pub charge: u8,
    pub health: u8,
    pub goal: bool,
    pub tier: u16,
}
impl<const FROM: bool> Key<FROM> {
    pub fn kept<const CAPACITY_TWO: bool>(self) -> Key<CAPACITY_TWO> {
        let Key {
            stock,
            place,
            context,
            charge,
            health,
            goal,
            tier,
        } = self;
        Key {
            stock,
            place,
            context,
            charge,
            health,
            goal,
            tier,
        }
    }
}
impl<const CAPACITY_TWO: bool> ArchiveKey for Key<CAPACITY_TWO> {
    type Place = u16;
    type Progress = (bool, u16);
    type Identity = u16;
    type Lineage = ();
    fn place(self) -> u16 {
        self.place
    }
    fn progress(self) -> (bool, u16) {
        (self.goal, self.tier)
    }
    fn identity(self) -> u16 {
        self.context
    }
    fn capacity() -> usize {
        if CAPACITY_TWO { 2 } else { 1 }
    }
    fn preferences() -> usize {
        if CAPACITY_TWO { 1 } else { 2 }
    }
    fn preference_cmp(self, preference: usize, other: Self) -> std::cmp::Ordering {
        if self.place / STAGE_PLACES != other.place / STAGE_PLACES {
            return self.stock.cmp(&other.stock);
        }
        if preference == 0 {
            (self.stock, self.charge, self.health).cmp(&(other.stock, other.charge, other.health))
        } else {
            (self.health, self.charge, self.stock).cmp(&(other.health, other.charge, other.stock))
        }
    }
    fn complete(self, _: Option<(Self, &())>) -> Self {
        self
    }
    fn record(_: &mut (), _: Self) {}
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Evidence {
    pub crossing_first_entry_work: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crossing_first_entry_execution: Option<u64>,
    pub crossing_actions: u64,
    pub crossing_pool_actions: u64,
    pub route_trace: Vec<route::Trace>,
    pub observations: u64,
    pub chain_stage_actions: Vec<u64>,
    pub chain_parent_selections: Vec<u64>,
    pub chain_pre_objective_parent_selections: Vec<u64>,
    pub chain_pre_objective_work_by_parent_stage: Vec<u64>,
    pub chain_work_by_parent_stage: Vec<u64>,
    pub chain_selected_charge: Vec<Vec<u64>>,
    pub chain_first_reach_work: Vec<Option<u64>>,
    pub chain_stage_entries: Vec<u64>,
    pub chain_entry_charge: Vec<Vec<u64>>,
    pub arrivals: [u64; 32],
    pub refills: u64,
    pub objectives: u64,
    pub health_recoveries: u64,
    pub correct_history_arrivals: u64,
    pub wrong_history_arrivals: u64,
    pub loop_resets: u64,
    pub explicit_resets: u64,
    pub action_attempts: [u64; 12],
    pub action_advances: [u64; 3],
    pub deadline_arrivals_by_remaining: Vec<u64>,
    pub deadline_expirations: u64,
    pub deadline_obstacle_attempts: u64,
    pub delayed_useful_state_actions: u64,
    pub delayed_distraction_actions: u64,
    pub delayed_progress_observations: Vec<u64>,
    pub job_parents: Vec<(u64, u16, u16)>,
    pub backtrack_first_items: Vec<Option<u64>>,
    pub map_first: Vec<Option<u64>>,
    pub map_first_tier: Vec<Option<u64>>,
    pub map_first_boss_room: Option<u64>,
    pub map_first_stocked: Option<u64>,
    pub map_first_hit_stock: Option<u8>,
    pub map_max_hits: u8,
    pub admitted_job_work: u64,
    pub job_start_work: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passive_clock: Option<passive_clock::Evidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub targets: Option<targets::Evidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fork_first: Vec<Option<u64>>,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TinyArchive;

impl ArchiveFormatName for TinyArchive {
    const FORMAT: &'static str = ARCHIVE_FORMAT;
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(bound = "K: ArchiveKey")]
pub struct ArchiveReport<K = Key> {
    pub format: ArchiveFormat<TinyArchive>,
    #[serde(with = "entries_by_suffix")]
    pub entries: Vec<ArchiveEntryReport<u8, K, bool>>,
    pub evidence: Evidence,
    pub selector: searcher::search::archive::SelectorAccounting,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Observation {
    before: State,
    after: State,
    work_in_job: u64,
    job_work: Option<u64>,
    #[serde(skip)]
    asked: Option<ChoiceRecord>,
}
pub struct Target<L = ()> {
    state: State,
    work: u64,
    observation: Option<Observation>,
    lane: L,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(bound = "L: Lane")]
pub struct Snapshot<L = ()> {
    pub state: State,
    pub payload: Vec<u8>,
    pub lane: L,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Scale {
    pub workers: u32,
    pub window: usize,
    pub memory_budget_mib: usize,
    pub archive_entries: usize,
    pub action_cost_ns: u64,
    pub action_sleep_ns: u64,
    pub snapshot_bytes: usize,
}
impl Default for Scale {
    fn default() -> Self {
        Self {
            workers: 1,
            window: 1,
            memory_budget_mib: 16_384,
            archive_entries: 4096,
            action_cost_ns: 0,
            action_sleep_ns: 0,
            snapshot_bytes: 0,
        }
    }
}
impl Scale {
    pub const MAX_WORK_BUDGET: u64 = 10_000_000_000;

    pub fn validate(&self) -> Result<(), String> {
        if !(1..=64).contains(&self.workers) {
            return Err("scale workers must be 1..=64".into());
        }
        if !(1..=512).contains(&self.window) {
            return Err("scale window must be 1..=512".into());
        }
        if !(1..=16_384).contains(&self.memory_budget_mib) {
            return Err("scale memory budget must be 1..=16384 MiB".into());
        }
        if !(1..=MAX_ARCHIVE_ENTRIES).contains(&self.archive_entries) {
            return Err(format!(
                "scale archive entries must be 1..={MAX_ARCHIVE_ENTRIES}"
            ));
        }
        if self.action_sleep_ns > 10_000_000 {
            return Err("scale action sleep must be at most 10 ms".into());
        }
        if self.action_cost_ns > 10_000_000 {
            return Err("scale action cost must be at most 10 ms".into());
        }
        if self.snapshot_bytes > 1 << 20 {
            return Err("scale snapshot payload must be at most 1 MiB".into());
        }
        Ok(())
    }
}
pub trait Lane:
    Clone + std::fmt::Debug + Default + Eq + Send + Sync + Serialize + DeserializeOwned + 'static
{
    fn heap_bytes(&self) -> usize {
        0
    }
}

impl Lane for () {}

pub trait Keying: Sync {
    type Key: ArchiveKey + std::fmt::Debug + Eq + Send + Sync;
    type Lane: Lane;
    fn arrive(&self, world: &World, state: State, broken: bool, lane: &mut Self::Lane);
    fn key(
        &self,
        world: &World,
        state: State,
        broken: bool,
        lane: &Self::Lane,
    ) -> Result<Self::Key, Box<dyn Error>>;
    fn rejected(&self, _world: &World, _state: State, _broken: bool, _lane: &Self::Lane) -> bool {
        false
    }
    fn shape(&self) -> KeyShape;
    fn identity(&self) -> String;
    fn policy(&self) -> Option<&str>;
    fn shadow(&self) -> Result<Option<ShadowReport>, Box<dyn Error>> {
        Ok(None)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Hand<const CAPACITY_TWO: bool>;

impl<const CAPACITY_TWO: bool> Keying for Hand<CAPACITY_TWO> {
    type Key = Key<CAPACITY_TWO>;
    type Lane = ();
    fn arrive(&self, _: &World, _: State, _: bool, _: &mut ()) {}
    fn key(
        &self,
        world: &World,
        state: State,
        broken: bool,
        _: &(),
    ) -> Result<Self::Key, Box<dyn Error>> {
        if matches!(world, World::Ridge(_)) {
            return Err("the ridge family keys by hand with ridge::Hand".into());
        }
        Ok(world.key(state, broken).kept())
    }
    fn shape(&self) -> KeyShape {
        KeyShape::of::<Key<CAPACITY_TWO>>()
    }
    fn identity(&self) -> String {
        format!("hand-key-capacity-two-{CAPACITY_TWO}")
    }
    fn policy(&self) -> Option<&str> {
        None
    }
}

#[derive(Clone, Debug)]
pub struct Strategic {
    pub compiled: Compiled,
}

impl Strategic {
    pub fn new(world: &World, broken: bool, text: &str) -> Result<Self, Box<dyn Error>> {
        Self::compiled(world, broken, &Strategy::parse(text)?)
    }

    pub fn compiled(
        world: &World,
        broken: bool,
        strategy: &Strategy,
    ) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            compiled: Compiled::new(strategy, &assertions::catalog(world, broken)?)?,
        })
    }

    pub fn shipped(world: &World, broken: bool, keep: Keep) -> Result<Self, Box<dyn Error>> {
        Self::compiled(world, broken, &assertions::shipped_strategy(world, keep)?)
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
enum Arrival {
    #[default]
    Absent,
    Keyed(AssertionKey),
    Rejected,
    RejectedGoal,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct StrategyLane {
    accumulator: Accumulator,
    arrival: Arrival,
}

impl Lane for StrategyLane {
    fn heap_bytes(&self) -> usize {
        self.accumulator.heap_bytes()
    }
}

impl Keying for Strategic {
    type Key = AssertionKey;
    type Lane = StrategyLane;
    fn arrive(&self, world: &World, state: State, broken: bool, lane: &mut StrategyLane) {
        let observed = self.compiled.observe(
            &assertions::record(world, state, broken),
            &mut lane.accumulator,
        );
        lane.arrival = match observed.key {
            _ if observed.rejected.is_some() && !world.goal(state) => Arrival::Rejected,
            Some(key) => Arrival::Keyed(key),
            None if observed.rejected.is_some() => Arrival::RejectedGoal,
            None => Arrival::Absent,
        };
    }
    fn key(
        &self,
        _: &World,
        _: State,
        _: bool,
        lane: &StrategyLane,
    ) -> Result<AssertionKey, Box<dyn Error>> {
        match lane.arrival {
            Arrival::Keyed(key) => Ok(key),
            Arrival::Absent => Err("a tiny-world strategy marked a state absent".into()),
            Arrival::Rejected => Err("a tiny-world strategy rejected the state".into()),
            Arrival::RejectedGoal => Err(
                "a tiny-world strategy's reject line matches a state where the world's goal holds"
                    .into(),
            ),
        }
    }
    fn rejected(&self, _: &World, _: State, _: bool, lane: &StrategyLane) -> bool {
        lane.arrival == Arrival::Rejected
    }
    fn shape(&self) -> KeyShape {
        self.compiled.shape()
    }
    fn identity(&self) -> String {
        self.compiled.policy_identifier().to_owned()
    }
    fn policy(&self) -> Option<&str> {
        Some(self.compiled.policy_identifier())
    }
}

pub struct Shadowed<K: Keying> {
    hand: K,
    compiled: Compiled,
    table: ShadowTable<K::Key>,
}

impl<K: Keying> Shadowed<K> {
    pub fn new(hand: K, strategic: Strategic) -> Self {
        Self {
            hand,
            compiled: strategic.compiled,
            table: ShadowTable::default(),
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(bound = "L: Lane, H: ArchiveKey")]
pub struct ShadowLane<L, H: ArchiveKey> {
    hand: L,
    cursor: ShadowCursor<H>,
}

impl<L: Default, H: ArchiveKey> Default for ShadowLane<L, H> {
    fn default() -> Self {
        Self {
            hand: L::default(),
            cursor: ShadowCursor::default(),
        }
    }
}

impl<L: std::fmt::Debug, H: ArchiveKey + std::fmt::Debug> std::fmt::Debug for ShadowLane<L, H>
where
    H::Lineage: std::fmt::Debug,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ShadowLane")
            .field("hand", &self.hand)
            .field("cursor", &self.cursor)
            .finish()
    }
}

impl<L: PartialEq, H: ArchiveKey> PartialEq for ShadowLane<L, H>
where
    H::Lineage: PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        self.hand == other.hand && self.cursor == other.cursor
    }
}

impl<L: Eq, H: ArchiveKey> Eq for ShadowLane<L, H> where H::Lineage: Eq {}

impl<L: Lane, H: ArchiveKey + std::fmt::Debug + Send + Sync + 'static> Lane for ShadowLane<L, H>
where
    H::Lineage: std::fmt::Debug + Eq + Send + Sync,
{
    fn heap_bytes(&self) -> usize {
        self.hand.heap_bytes() + self.cursor.heap_bytes()
    }
}

impl<K: Keying> Keying for Shadowed<K>
where
    K::Key: 'static,
    <K::Key as ArchiveKey>::Lineage: std::fmt::Debug + Eq + Send + Sync,
{
    type Key = K::Key;
    type Lane = ShadowLane<K::Lane, K::Key>;
    fn arrive(&self, world: &World, state: State, broken: bool, lane: &mut Self::Lane) {
        self.hand.arrive(world, state, broken, &mut lane.hand);
        lane.cursor.observe_with_objective(
            &self.compiled,
            &assertions::record(world, state, broken),
            world.goal(state),
        );
        let raw = if self.hand.rejected(world, state, broken, &lane.hand) {
            None
        } else {
            self.hand.key(world, state, broken, &lane.hand).ok()
        };
        lane.cursor.pair(&self.table, raw);
    }
    fn key(
        &self,
        world: &World,
        state: State,
        broken: bool,
        lane: &Self::Lane,
    ) -> Result<K::Key, Box<dyn Error>> {
        self.hand.key(world, state, broken, &lane.hand)
    }
    fn rejected(&self, world: &World, state: State, broken: bool, lane: &Self::Lane) -> bool {
        self.hand.rejected(world, state, broken, &lane.hand)
    }
    fn shape(&self) -> KeyShape {
        self.hand.shape()
    }
    fn identity(&self) -> String {
        self.hand.identity()
    }
    fn policy(&self) -> Option<&str> {
        self.hand.policy()
    }
    fn shadow(&self) -> Result<Option<ShadowReport>, Box<dyn Error>> {
        Ok(Some(self.table.report(&self.compiled)?))
    }
}

pub struct Workload<K: Keying = Hand<false>> {
    pub config: World,
    pub broken: bool,
    pub scale: Option<Scale>,
    pub keying: K,
}
impl<K: Keying> Workload<K> {
    fn payload(&self, state: State) -> Vec<u8> {
        let bytes = self.scale.map_or(0, |scale| scale.snapshot_bytes);
        if bytes == 0 {
            return Vec::new();
        }
        let digest = postcard_value_sha256(&state).expect("serializable state");
        let mut rand = RomuDuoJrRand::with_seed(
            u64::from_str_radix(&digest[..16], 16).expect("hexadecimal digest"),
        );
        let mut payload = Vec::with_capacity(bytes.next_multiple_of(8));
        while payload.len() < bytes {
            payload.extend_from_slice(&rand.next_u64().to_le_bytes());
        }
        payload.truncate(bytes);
        payload
    }

    fn genesis_lane(&self) -> K::Lane {
        let mut lane = K::Lane::default();
        self.keying
            .arrive(&self.config, self.config.initial(), self.broken, &mut lane);
        lane
    }

    fn spend_action_cost(&self) {
        if let Some(sleep) = self
            .scale
            .map(|scale| scale.action_sleep_ns)
            .filter(|&sleep| sleep > 0)
        {
            std::thread::sleep(std::time::Duration::from_nanos(sleep));
        }
        let Some(cost) = self
            .scale
            .map(|scale| scale.action_cost_ns)
            .filter(|&c| c > 0)
        else {
            return;
        };
        let started = cpu_time::ThreadTime::now();
        let cost = std::time::Duration::from_nanos(cost);
        while started.elapsed() < cost {
            std::hint::spin_loop();
        }
    }
}
impl<K: Keying> CampaignTypes for Workload<K> {
    type JobObservation = ();
    type Target = Target<K::Lane>;
    type Action = u8;
    type Key = K::Key;
    type Milestones = bool;
    type Progress = bool;
    type Snapshot = Snapshot<K::Lane>;
    type Observations = Observation;
    type Evidence = Evidence;
    type ArchiveReport = ArchiveReport<K::Key>;
    type Run = ();
}
impl<K: Keying> Reporting for Workload<K> {
    fn stream_format(&self) -> &'static str {
        "tiny-world-v2"
    }
    fn checkpoint_format(&self) -> &'static str {
        "tiny-world-checkpoint-v3"
    }
    fn write_evidence_checkpoint(
        evidence: &Evidence,
        out: &mut dyn Write,
    ) -> Result<(), Box<dyn Error>> {
        serde_json::to_writer(out, evidence)?;
        Ok(())
    }
    fn evidence_from_checkpoint(bytes: &[u8]) -> Result<Evidence, Box<dyn Error>> {
        Ok(serde_json::from_slice(bytes)?)
    }
    fn catalog_signals(&self, _: &()) -> Vec<String> {
        assertions::catalog(&self.config, self.broken).map_or_else(
            |_| Vec::new(),
            |catalog| {
                catalog
                    .signals()
                    .iter()
                    .map(|signal| signal.name.clone())
                    .collect()
            },
        )
    }
    fn state_record(&self, _: &(), snapshot: &Snapshot<K::Lane>) -> Option<Record> {
        assertions::lowers(&self.config)
            .then(|| assertions::record(&self.config, snapshot.state, self.broken))
    }
    fn observation_record(&self, _: &(), observation: &Observation) -> Option<Record> {
        assertions::lowers(&self.config)
            .then(|| assertions::record(&self.config, observation.after, self.broken))
    }
    fn choice_records(
        &self,
        _: &(),
        _: Option<&ChoicePlan>,
        _: usize,
        _: &u8,
        observations: &[Observation],
    ) -> Vec<ChoiceRecord> {
        observations
            .iter()
            .filter_map(|observation| observation.asked.clone())
            .collect()
    }
    fn workload_identity_sha256(&self) -> String {
        postcard_value_sha256(&(
            &self.config,
            self.broken,
            self.keying.identity(),
            self.scale,
        ))
        .expect("serializable config")
    }
    fn action_cost_unit(&self) -> &'static str {
        self.config.work_unit()
    }
    fn execution_work_unit(&self) -> &'static str {
        self.config.work_unit()
    }
    fn result_sha256(&self, result: &CampaignJobResult<Self>) -> Result<String, Box<dyn Error>> {
        if self.scale.is_none_or(|scale| scale.snapshot_bytes == 0) {
            return postcard_value_sha256(result);
        }
        let mut result = result.clone();
        for candidate in result
            .actions
            .iter_mut()
            .filter_map(|a| a.candidate.as_mut())
        {
            candidate.snapshot.payload = Vec::new();
        }
        postcard_value_sha256(&result)
    }
    fn snapshot_identity_sha256(
        &self,
        snapshot: &Self::Snapshot,
    ) -> Result<String, Box<dyn Error>> {
        if self.scale.is_none_or(|scale| scale.snapshot_bytes == 0) {
            return postcard_value_sha256(snapshot);
        }
        let mut logical = snapshot.clone();
        logical.payload.clear();
        postcard_value_sha256(&logical)
    }
    fn archive_report(
        &self,
        evidence: &Evidence,
        state: ArchiveReportState<Self>,
    ) -> ArchiveReport<K::Key> {
        ArchiveReport {
            format: ArchiveFormat::default(),
            entries: state.entries,
            evidence: evidence.clone(),
            selector: state.selector,
        }
    }
}
impl<K: Keying> InputPolicy for Workload<K> {
    fn policies(&self, _: &()) -> WorkloadPolicies {
        [(
            "tiny_actions".into(),
            if matches!(self.config, World::PassiveClock(_)) {
                "weighted_hold_bands_v1".into()
            } else if let World::Picks(w) = &self.config {
                match w.draw {
                    picks::Draw::Flat => "picks-flat-eight-v1".into(),
                    picks::Draw::Choices(_) => "picks-choices-v1".into(),
                }
            } else if let World::Crossing(w) = &self.config
                && w.action_denominator.is_some()
            {
                format!(
                    "crossing-action-denominator-{}-v1",
                    w.action_denominator.unwrap()
                )
            } else if self.config.changes_actions() && self.broken {
                "frozen-land-v1".into()
            } else {
                "uniform-four-v1".into()
            },
        )]
        .into_iter()
        .chain(
            self.keying
                .policy()
                .map(|policy| ("tiny_key".to_owned(), policy.to_owned())),
        )
        .collect()
    }
    fn resolve_recorded(&self, policies: &WorkloadPolicies) -> Result<(), Box<dyn Error>> {
        if *policies != self.policies(&()) {
            return Err("unknown tiny-world policy".into());
        }
        Ok(())
    }
    fn choice_sites(&self, _: &()) -> Vec<ChoiceSite> {
        match &self.config {
            World::Picks(w) if w.choices().is_some() => w.sites(),
            _ => Vec::new(),
        }
    }
    fn choice_policy(&self, _: &()) -> ChoicePolicy {
        match &self.config {
            World::Picks(w) => w.choices().unwrap_or_default(),
            _ => ChoicePolicy::default(),
        }
    }
    fn sample_alphabet(
        &self,
        _: &(),
        _: Option<&u8>,
        rand: &mut RomuDuoJrRand,
    ) -> Result<u8, Box<dyn Error>> {
        if let World::PassiveClock(w) = &self.config {
            return Ok(w.sample(rand));
        }
        if let World::Crossing(w) = &self.config {
            return Ok(w.sample(rand));
        }
        if let World::Picks(w) = &self.config {
            return Ok(match w.draw {
                picks::Draw::Flat => {
                    rand.below(NonZeroUsize::new(usize::from(picks::LABELS)).unwrap()) as u8
                }
                picks::Draw::Choices(_) => picks::ASK,
            });
        }
        Ok(self
            .config
            .sample_action(rand.below(NonZeroUsize::new(4).unwrap()) as u8, self.broken))
    }
}
impl<K: Keying> TargetExecution for Workload<K> {
    fn new_target(&self) -> Result<Target<K::Lane>, String> {
        self.config.validate()?;
        Ok(Target {
            state: self.config.initial(),
            work: 0,
            observation: None,
            lane: self.genesis_lane(),
        })
    }
    fn reset(&self, target: &mut Target<K::Lane>) {
        target.state = self.config.initial();
        target.observation = None;
        target.lane = self.genesis_lane();
    }
    fn restore(
        &self,
        target: &mut Target<K::Lane>,
        snapshot: &Snapshot<K::Lane>,
    ) -> Result<(), Box<dyn Error>> {
        if !self.config.valid_state(snapshot.state) {
            return Err("invalid snapshot state for world".into());
        }
        target.state = snapshot.state;
        target.observation = None;
        target.lane = snapshot.lane.clone();
        Ok(())
    }
    fn execution_work(&self, target: &Target<K::Lane>) -> u64 {
        target.work
    }
    fn planned_cost_fn(&self) -> fn(&u8) -> u64 {
        self.config.planned_cost_fn()
    }
    fn snapshot_memory_charge(snapshot: &Snapshot<K::Lane>) -> usize {
        std::mem::size_of::<Snapshot<K::Lane>>()
            + snapshot.payload.capacity()
            + snapshot.lane.heap_bytes()
    }
    fn apply_action(
        &self,
        target: &mut Target<K::Lane>,
        action: &u8,
        milestones: &mut bool,
    ) -> Result<(), Box<dyn Error>> {
        if let World::PassiveClock(w) = &self.config
            && !w.valid_action(*action)
        {
            return Err("unavailable passive-clock duration".into());
        }
        let before = target.state;
        let cost = self.config.step_work(before, *action);
        self.spend_action_cost();
        target.state = self.config.step(target.state, *action);
        self.keying
            .arrive(&self.config, target.state, self.broken, &mut target.lane);
        target.observation = Some(Observation {
            before,
            after: target.state,
            work_in_job: target.work + cost,
            job_work: None,
            asked: None,
        });
        target.work += cost;
        *milestones |= self.config.goal(target.state);
        Ok(())
    }
    fn rollout_observations(&self, target: &Target<K::Lane>) -> Vec<Observation> {
        target.observation.iter().cloned().collect()
    }
    fn resolve_action(
        &self,
        target: &Target<K::Lane>,
        action: &u8,
        chooser: &mut Chooser,
    ) -> Result<u8, Box<dyn Error>> {
        match (&self.config, target.state) {
            (World::Picks(w), State::Picks(s)) if *action == picks::ASK => {
                let depth = s.depth.min(w.depth - 1);
                let pick = w.pick(depth);
                Ok(picks::Config::encode(chooser.choose(
                    u16::from(depth),
                    &pick.offered,
                    pick.default,
                )))
            }
            _ => Ok(*action),
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn execute_job(
        &self,
        run: &<Self as CampaignTypes>::Run,
        target: &mut <Self as CampaignTypes>::Target,
        origin_snapshot: &<Self as CampaignTypes>::Snapshot,
        replay: &[<Self as CampaignTypes>::Action],
        parent_milestones: <Self as CampaignTypes>::Milestones,
        suffix: &[<Self as CampaignTypes>::Action],
        retention: RetentionPolicy,
        stop: searcher::search::rollout::RolloutStop,
    ) -> Result<CampaignJobResult<Self>, Box<dyn Error>> {
        let start_work = target.work;
        let mut result = searcher::search::rollout::execute_job(
            self,
            run,
            target,
            origin_snapshot,
            replay,
            parent_milestones,
            suffix,
            retention,
            stop,
        )?;
        let work = target.work - start_work;
        if let World::Picks(w) = &self.config {
            for (asked, action) in suffix.iter().zip(&mut result.actions) {
                if *asked == picks::ASK
                    && action.action != picks::ASK
                    && let Some(observation) = action.observations.first_mut()
                    && let State::Picks(s) = observation.before
                {
                    let depth = s.depth.min(w.depth - 1);
                    let pick = w.pick(depth);
                    observation.asked = Some(ChoiceRecord::new(
                        u16::from(depth),
                        pick.offered,
                        pick.default,
                        picks::Config::decode(action.action),
                    ));
                }
            }
        }
        for observation in result
            .actions
            .iter_mut()
            .flat_map(|action| action.observations.iter_mut())
        {
            observation.work_in_job -= start_work;
        }
        if let Some(observation) = result
            .actions
            .first_mut()
            .and_then(|a| a.observations.first_mut())
        {
            observation.job_work = Some(work);
        } else if work != 0 {
            return Err("job executed work without an observation".into());
        }
        Ok(result)
    }
    fn snapshot(&self, target: &mut Target<K::Lane>) -> Result<Snapshot<K::Lane>, Box<dyn Error>> {
        Ok(Snapshot {
            state: target.state,
            payload: self.payload(target.state),
            lane: target.lane.clone(),
        })
    }
}
impl<K: Keying> searcher::search::shrink::ShrinkPolicy for Workload<K> {
    fn window_actions(&self, _: &()) -> Vec<u8> {
        (0..4)
            .map(|action| self.config.sample_action(action, self.broken))
            .collect()
    }

    fn window_fingerprint(&self, target: &Target<K::Lane>) -> Option<u64> {
        Some(
            format!("{:?}", target.state)
                .bytes()
                .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
                    (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
                }),
        )
    }
}

impl<K: Keying> Evaluation for Workload<K> {
    fn key_shape(&self) -> KeyShape {
        self.keying.shape()
    }
    fn execution_disposition(&self, target: &Target<K::Lane>) -> ExecutionDisposition {
        match target.state {
            State::Picks(s) if s.dead => ExecutionDisposition::Terminal,
            _ => ExecutionDisposition::Runnable,
        }
    }
    fn objective_reached(&self, _: &(), target: &Target<K::Lane>) -> Result<bool, Box<dyn Error>> {
        Ok(self.config.goal(target.state))
    }
    fn is_doomed(&self, target: &Target<K::Lane>) -> bool {
        self.keying
            .rejected(&self.config, target.state, self.broken, &target.lane)
    }
    fn action_cost(&self, action: &u8, observations: &[Observation]) -> u64 {
        observations.last().map_or_else(
            || (self.config.planned_cost_fn())(action),
            |observation| self.config.step_work(observation.before, *action),
        )
    }
    fn current_key(&self, target: &Target<K::Lane>) -> Result<K::Key, Box<dyn Error>> {
        self.keying
            .key(&self.config, target.state, self.broken, &target.lane)
    }
    fn complete_candidate_key(
        &self,
        key: K::Key,
        _: &Snapshot<K::Lane>,
    ) -> Result<K::Key, Box<dyn Error>> {
        Ok(key)
    }
    fn merge_milestones(&self, into: &mut bool, from: bool) {
        *into |= from;
    }
    fn aggregate_milestones(e: &Evidence) -> bool {
        e.objectives > 0
    }
    fn aggregate_progress(e: &Evidence) -> bool {
        e.objectives > 0
    }
    fn merge_origin_evidence(&self, e: &mut Evidence, s: &ArchiveReport<K::Key>) {
        *e = s.evidence.clone();
    }
    fn merge_snapshot_root_evidence(
        &self,
        e: &mut Evidence,
        t: &Target<K::Lane>,
    ) -> Result<(), Box<dyn Error>> {
        e.objectives += u64::from(self.config.goal(t.state));
        Ok(())
    }
    fn merge_import_evidence(&self, e: &mut Evidence, m: bool, _: &Input<u8>) {
        e.objectives += u64::from(m);
    }
    fn merge_action_evidence<F>(
        &self,
        e: &mut Evidence,
        a: &CampaignActionResult<Self>,
        sequence: u64,
        _: F,
    ) -> Result<(), Box<dyn Error>>
    where
        F: FnOnce() -> Result<Input<u8>, Box<dyn Error>>,
    {
        for observation in &a.observations {
            if let Some(work) = observation.job_work {
                e.job_start_work = e.admitted_job_work;
                e.admitted_job_work += work;
            }
            let reached_work = e.job_start_work + observation.work_in_job;
            e.observations += 1;
            if observation.job_work.is_some() && self.scale.is_none() {
                let key = self.config.key(observation.before, false);
                e.job_parents.push((sequence, key.tier, key.place));
            }
            match (&self.config, observation.before, observation.after) {
                (World::Route(_), State::Route(_), State::Route(_)) if self.scale.is_some() => {}
                (World::Route(_), State::Route(before), State::Route(after)) => {
                    e.route_trace.push(route::Trace {
                        sequence,
                        action: a.action,
                        candidate: a.candidate.is_some(),
                        objective: a.outcome.objective_reached,
                        before,
                        after,
                    });
                }
                (World::Resource(w), State::Resource(before), State::Resource(after)) => {
                    if after.place == w.corridor_len && before.place != after.place {
                        e.arrivals[usize::from(after.charge)] += 1;
                    }
                    e.refills += u64::from(after.charge > before.charge);
                    e.health_recoveries += u64::from(after.health > before.health);
                }
                (World::Maze(w), State::Maze(before), State::Maze(after)) => {
                    if after.place == w.length && before.place != after.place {
                        if w.goal(w.step(after, 0)) {
                            e.correct_history_arrivals += 1;
                        } else {
                            e.wrong_history_arrivals += 1;
                        }
                    }
                    e.loop_resets +=
                        u64::from(before.place == w.length && after.place == 0 && a.action == 0);
                    e.explicit_resets +=
                        u64::from(before.place > 0 && after.place == 0 && a.action == 2);
                }
                (World::Actions(_), State::Actions(before), State::Actions(after)) => {
                    if !before.goal {
                        let index = usize::from(before.regime) * 4 + usize::from(a.action);
                        e.action_attempts[index] += 1;
                        e.action_advances[usize::from(before.regime)] += u64::from(before != after);
                    }
                }
                (World::Deadline(w), State::Deadline(before), State::Deadline(after)) => {
                    e.deadline_arrivals_by_remaining.resize(65, 0);
                    if before.position != after.position {
                        e.deadline_arrivals_by_remaining[usize::from(after.remaining)] += 1;
                    }
                    e.deadline_expirations +=
                        u64::from(before.remaining > 0 && after.remaining == 0 && !after.goal);
                    e.deadline_obstacle_attempts += u64::from(
                        before.position == w.length && before.remaining > 0 && a.action == 2,
                    );
                }
                (
                    World::PassiveClock(w),
                    State::PassiveClock(before),
                    State::PassiveClock(after),
                ) => {
                    let clock = e.passive_clock.get_or_insert_with(Default::default);
                    clock.held_actions.resize(w.maximum_hold() as usize + 1, 0);
                    clock.held_actions[usize::from(a.action)] += 1;
                    clock.parent_phase_jobs.resize(w.events.len() + 1, 0);
                    clock.maximum_parent_elapsed.resize(w.events.len() + 1, 0);
                    if observation.job_work.is_some() {
                        let phase = w.phase(before);
                        clock.parent_phase_jobs[phase] += 1;
                        clock.maximum_parent_elapsed[phase] =
                            clock.maximum_parent_elapsed[phase].max(before.elapsed);
                    }
                    clock.first_event_work.resize(w.events.len(), None);
                    clock.first_event_execution.resize(w.events.len(), None);
                    for (i, &event) in w.events.iter().enumerate() {
                        if after.elapsed >= event {
                            clock.first_event_work[i].get_or_insert(reached_work);
                            clock.first_event_execution[i].get_or_insert(sequence);
                        }
                    }
                }
                (World::Delayed(_), State::Delayed(before), State::Delayed(after)) => {
                    if !before.goal {
                        e.delayed_useful_state_actions += u64::from(before.lane == 0);
                        e.delayed_distraction_actions += u64::from(before.lane != 0);
                        e.delayed_progress_observations.resize(17, 0);
                        e.delayed_progress_observations[usize::from(after.progress)] += 1;
                    }
                }
                (
                    World::DeadlineActions(w),
                    State::DeadlineActions(before),
                    State::DeadlineActions(after),
                ) => {
                    if !w.goal(before) && before.remaining > 0 {
                        let index = usize::from(before.actions.regime) * 4 + usize::from(a.action);
                        e.action_attempts[index] += 1;
                        let advanced = before.actions != after.actions;
                        e.action_advances[usize::from(before.actions.regime)] +=
                            u64::from(advanced);
                        e.deadline_arrivals_by_remaining.resize(65, 0);
                        if advanced {
                            e.deadline_arrivals_by_remaining[usize::from(after.remaining)] += 1;
                        }
                        e.deadline_expirations += u64::from(after.remaining == 0 && !w.goal(after));
                    }
                }
                (World::Chain(w), State::Chain(before), State::Chain(after)) => {
                    e.chain_parent_selections.resize(w.stages.len(), 0);
                    e.chain_pre_objective_parent_selections
                        .resize(w.stages.len(), 0);
                    e.chain_pre_objective_work_by_parent_stage
                        .resize(w.stages.len(), 0);
                    e.chain_work_by_parent_stage.resize(w.stages.len(), 0);
                    e.chain_selected_charge
                        .resize_with(w.stages.len(), || vec![0; 32]);
                    if let Some(work) = observation.job_work {
                        let stage = usize::from(before.stage);
                        e.chain_parent_selections[stage] += 1;
                        if e.objectives == 0 {
                            e.chain_pre_objective_parent_selections[stage] += 1;
                            e.chain_pre_objective_work_by_parent_stage[stage] += work;
                        }
                        e.chain_work_by_parent_stage[stage] += work;
                        e.chain_selected_charge[stage][usize::from(before.charge)] += 1;
                    }
                    e.chain_first_reach_work.resize(w.stages.len(), None);
                    e.chain_first_reach_work[0] = Some(0);
                    e.chain_stage_actions.resize(w.stages.len(), 0);
                    e.chain_stage_entries.resize(w.stages.len(), 0);
                    e.chain_entry_charge
                        .resize_with(w.stages.len(), || vec![0; 32]);
                    if !w.goal(before) {
                        e.chain_stage_actions[usize::from(before.stage)] += 1;
                    }
                    if after.stage != before.stage {
                        e.chain_first_reach_work[usize::from(after.stage)]
                            .get_or_insert(reached_work);
                        e.chain_stage_entries[usize::from(after.stage)] += 1;
                        e.chain_entry_charge[usize::from(after.stage)]
                            [usize::from(after.charge)] += 1;
                    }
                }
                (World::Trap(_), State::Trap(_), State::Trap(_)) => {}
                (World::Backtrack(w), State::Backtrack(before), State::Backtrack(after)) => {
                    e.backtrack_first_items
                        .resize(usize::from(w.barriers) + 1, None);
                    e.backtrack_first_items[0] = Some(0);
                    if after.items > before.items {
                        e.backtrack_first_items[usize::from(after.items)]
                            .get_or_insert(reached_work);
                    }
                }
                (World::Map(w), State::Map(before), State::Map(after)) => {
                    e.map_first.resize(4, None);
                    let layout = w.layout();
                    let inside = |cell: u8| layout.inner[usize::from(cell)];
                    let reached = [
                        inside(after.cell) && !inside(before.cell),
                        after.item && !before.item,
                        after.item && !inside(after.cell) && inside(before.cell),
                        after.goal && !before.goal,
                    ];
                    for (first, reached) in e.map_first.iter_mut().zip(reached) {
                        if reached {
                            first.get_or_insert(reached_work);
                        }
                    }
                    e.map_first_tier.resize(usize::from(w.top_tier()) + 1, None);
                    for first in &mut e.map_first_tier[..=usize::from(w.tier(w.initial()))] {
                        first.get_or_insert(0);
                    }
                    if w.rooted {
                        e.map_first_boss_room.get_or_insert(0);
                        e.map_first_stocked.get_or_insert(0);
                    }
                    if w.tier(after) > w.tier(before) {
                        e.map_first_tier[usize::from(w.tier(after))].get_or_insert(reached_work);
                    }
                    if w.gauntlet
                        && after.arm == 0
                        && after.cell == layout.entry
                        && w.tier(after) == w.top_tier()
                        && after.health >= layout.door_to_goal
                    {
                        e.map_first_stocked.get_or_insert(reached_work);
                    }
                    if w.boss_stock > 0 && after.item && after.arm == 0 && after.cell == layout.goal
                    {
                        e.map_first_boss_room.get_or_insert(reached_work);
                    }
                    if w.boss_stock > 0
                        && after.item
                        && after.arm == 0
                        && after.cell == layout.goal
                        && after.stock + after.shield >= w.boss_stock + w.shield
                        && (w.arena == 0 || after.stock == w.farm_cap)
                        && after.hits == 0
                        && (!w.boss_hits_back || after.health >= w.boss_stock)
                    {
                        e.map_first_stocked.get_or_insert(reached_work);
                    }
                    if after.hits > 0 && before.hits == 0 {
                        e.map_first_hit_stock.get_or_insert(after.stock);
                    }
                    e.map_max_hits = e.map_max_hits.max(after.hits);
                }
                (World::Graph(_), State::Graph(_), State::Graph(_))
                | (World::Picks(_), State::Picks(_), State::Picks(_))
                | (World::Ridge(_), State::Ridge(_), State::Ridge(_))
                | (World::Stairs(_), State::Stairs(_), State::Stairs(_)) => {}
                (World::Fork(w), State::Fork(_), State::Fork(after)) => {
                    e.fork_first.resize(3, None);
                    let reached = [
                        w.live(after),
                        w.live(after) && after.position > w.tip(),
                        after.goal,
                    ];
                    for (first, reached) in e.fork_first.iter_mut().zip(reached) {
                        if reached {
                            first.get_or_insert(reached_work);
                        }
                    }
                }
                (World::Targets(w), State::Targets(before), State::Targets(after)) => {
                    let t = e.targets.get_or_insert_with(Default::default);
                    let levels = usize::from(w.cannons) + 1;
                    t.first_down_execution.resize(levels, None);
                    t.first_down_work.resize(levels, None);
                    t.first_down_execution[0].get_or_insert(0);
                    t.first_down_work[0].get_or_insert(0);
                    for down in usize::from(w.down(before)) + 1..=usize::from(w.down(after)) {
                        t.first_down_execution[down].get_or_insert(sequence);
                        t.first_down_work[down].get_or_insert(reached_work);
                    }
                    if after.stock < before.stock {
                        let (landed, double, decoy) = w.shot_outcome(before, after);
                        t.shots += 1;
                        t.landed += u64::from(landed);
                        t.double_kills += u64::from(double);
                        t.decoy_hits += u64::from(decoy);
                    }
                }
                (World::Crossing(w), State::Crossing(before), State::Crossing(after)) => {
                    if w.rooted {
                        e.crossing_first_entry_work.get_or_insert(0);
                        e.crossing_first_entry_execution.get_or_insert(0);
                    }
                    if before.position < w.cells && after.position >= w.cells {
                        e.crossing_first_entry_work.get_or_insert(reached_work);
                        e.crossing_first_entry_execution.get_or_insert(sequence);
                    }
                    if !w.goal(before) {
                        e.crossing_actions += u64::from(before.position >= w.cells);
                        e.crossing_pool_actions += u64::from(before.position < w.cells);
                    }
                }
                _ => return Err("observation family mismatch".into()),
            }
            e.objectives += u64::from(self.config.goal(observation.after));
        }
        Ok(())
    }
    fn source_entries<'a>(
        &self,
        s: &'a ArchiveReport<K::Key>,
    ) -> &'a [ArchiveEntryReport<u8, K::Key, bool>] {
        &s.entries
    }
    fn resume_input(&self, _: &ArchiveReport<K::Key>) -> Result<Input<u8>, Box<dyn Error>> {
        Err("archive origin unsupported".into())
    }
}

struct BoundedStream(Vec<u8>);
impl Write for BoundedStream {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.len() + bytes.len() > 1 << 30 {
            return Err(std::io::Error::other("stream bound exceeded"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct CountingStream(u64);
impl Write for CountingStream {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 += bytes.len() as u64;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub fn run_scaled<K: Keying>(
    workload: &Workload<K>,
    seed: u64,
    budget: u64,
    progress: &mut dyn Write,
    settings: SearchSettings,
) -> Result<serde_json::Value, Box<dyn Error>> {
    workload.config.validate()?;
    let scale = workload.scale.ok_or("scaled run needs scale settings")?;
    scale.validate()?;
    if budget == 0 || budget > Scale::MAX_WORK_BUDGET {
        return Err(format!("scaled work budget must be 1..={}", Scale::MAX_WORK_BUDGET).into());
    }
    let mut config = campaign_config(workload, seed, budget, scale.workers);
    settings.apply(&mut config);
    let mut stream = CountingStream(0);
    let started = telemetry_now();
    let live = run_campaign_checkpointed_with_options(
        workload,
        &config,
        &CampaignOrigin::Genesis,
        &mut stream,
        Some(progress),
        CampaignExecutionOptions {
            work_budget: Some(budget),
            checkpoints: None,
            placement: None,
            interrupt: None,
            job_host_path: None,
        },
    )?;
    let elapsed = started.elapsed().as_secs_f64();
    let report = live.0;
    let worker_time = (report.telemetry.search_ns as f64) * f64::from(scale.workers);
    let busy_ns: u64 = report.telemetry.workers.iter().map(|w| w.busy_ns).sum();
    Ok(serde_json::json!({
        "seed": seed,
        "broken": workload.broken,
        "config": workload.config,
        "scale": scale,
        "work_budget": budget,
        "work": report.execution_work,
        "first_objective_work": report.work_to_first_objective,
        "first_objective_execution": report.executions_to_first_objective,
        "executions": report.executions_completed,
        "work_unit": workload.config.work_unit(),
        "passive_clock": report.archive.evidence.passive_clock,
        "crossing": matches!(workload.config, World::Crossing(_)).then(|| serde_json::json!({
            "first_entry_execution": report.archive.evidence.crossing_first_entry_execution,
            "first_entry_work": report.archive.evidence.crossing_first_entry_work,
            "exit_actions": report.archive.evidence.crossing_actions,
            "pool_actions": report.archive.evidence.crossing_pool_actions,
        })),
        "mixture_policy": report.mixture_policy,
        "stop_on_objective": report.stop_campaign_on_objective,
        "success": report.work_to_first_objective.is_some_and(|w| w <= budget),
        "elapsed_seconds": elapsed,
        "stream_bytes": stream.0,
        "stream_sha256": report.stream_sha256,
        "resident_memory_bytes": report.resident_memory_bytes,
        "live_entries": report.live_entries,
        "selector": report.archive.selector,
        "worker_busy_fraction": busy_ns as f64 / worker_time,
        "admission_order_idle_fraction":
            report.telemetry.admission.idle_admission_order_ns as f64 / worker_time,
    }))
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum KeyKind {
    #[default]
    Hand,
    Strategy,
}

fn rekeyed<J: Keying, K: Keying>(workload: &Workload<J>, keying: K) -> Workload<K> {
    Workload {
        config: workload.config.clone(),
        broken: workload.broken,
        scale: workload.scale,
        keying,
    }
}

fn hand_campaign<K: Keying>(
    workload: &Workload,
    keying: K,
    shadow: Option<Strategic>,
    run: (u64, u64, bool, u32, SearchSettings),
) -> Result<serde_json::Value, Box<dyn Error>>
where
    K::Key: 'static,
    <K::Key as ArchiveKey>::Lineage: std::fmt::Debug + Eq + Send + Sync,
{
    let (seed, budget, verify, workers, settings) = run;
    match shadow {
        None => campaign(
            &rekeyed(workload, keying),
            seed,
            budget,
            verify,
            workers,
            settings,
        ),
        Some(strategic) => campaign(
            &rekeyed(workload, Shadowed::new(keying, strategic)),
            seed,
            budget,
            verify,
            workers,
            settings,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn run_kept(
    workload: &Workload,
    keep: Keep,
    key: KeyKind,
    strategy: Option<&str>,
    shadow: bool,
    seed: u64,
    budget: u64,
    verify: bool,
    workers: u32,
    settings: SearchSettings,
) -> Result<serde_json::Value, Box<dyn Error>> {
    if !(1..=64).contains(&workers) {
        return Err("workers must be 1..=64".into());
    }
    if strategy.is_some() && key != KeyKind::Strategy && !shadow {
        return Err("a strategy text needs key strategy or a shadow".into());
    }
    if shadow && key != KeyKind::Hand {
        return Err("a shadow runs beside key hand".into());
    }
    let strategic = || match strategy {
        Some(text) => Strategic::new(&workload.config, workload.broken, text),
        None => Strategic::shipped(&workload.config, workload.broken, keep),
    };
    let shadowed = if shadow { Some(strategic()?) } else { None };
    let run = (seed, budget, verify, workers, settings);
    let ridge = matches!(workload.config, World::Ridge(_));
    let mut report = match (key, keep) {
        (KeyKind::Hand, Keep::CapacityTwo) if ridge => {
            return Err("the ridge family keeps portfolio only".into());
        }
        (KeyKind::Hand, Keep::Portfolio) if ridge => {
            hand_campaign(workload, ridge::Hand, shadowed, run)?
        }
        (KeyKind::Hand, Keep::Portfolio) => hand_campaign(workload, Hand::<false>, shadowed, run)?,
        (KeyKind::Hand, Keep::CapacityTwo) => hand_campaign(workload, Hand::<true>, shadowed, run)?,
        (KeyKind::Strategy, _) => {
            let strategic = rekeyed(workload, strategic()?);
            let mut report = campaign(&strategic, seed, budget, verify, workers, settings)?;
            report["policy"] = serde_json::to_value(strategic.keying.compiled.policy_identifier())?;
            report
        }
    };
    report["keep"] = serde_json::to_value(keep)?;
    report["key"] = serde_json::to_value(key)?;
    Ok(report)
}

pub const ROOT_JOB_ACTIONS: [usize; 3] = [64, 256, 1024];

pub fn root_jobs<K: Keying>(
    workload: &Workload<K>,
    seed: u64,
    job_budget: u64,
) -> Result<serde_json::Value, Box<dyn Error>> {
    workload.config.validate()?;
    if job_budget == 0 {
        return Err("root jobs need a job budget".into());
    }
    let sites = workload.choice_sites(&());
    ChoiceSite::validate_all(&sites)?;
    let declared = ChoiceSite::declared_contrary(&sites);
    let policy = if sites.is_empty() {
        None
    } else {
        Some(workload.choice_policy(&()).validate()?)
    };
    let root_profile = policy.map(|policy| policy.root_profile(seed));
    let mut target = workload.new_target()?;
    let mut rand = RomuDuoJrRand::with_seed(seed);
    let (mut jobs, mut actions, mut terminal_jobs) = (0_u64, 0_u64, 0_u64);
    while jobs < job_budget {
        workload.reset(&mut target);
        let mutation_seed = rand.next_u64();
        let mut draw = RomuDuoJrRand::with_seed(mutation_seed);
        let mut chooser = policy.zip(root_profile).map(|(policy, profile)| {
            Chooser::new(&ChoicePlan::new(
                &policy,
                profile,
                mutation_seed,
                policy
                    .untried_first
                    .then(|| u32::try_from(jobs).unwrap_or(u32::MAX)),
                Vec::new(),
                declared.clone(),
            ))
        });
        let mut reached = false;
        let mut previous = None;
        for _ in 0..ROOT_JOB_ACTIONS[usize::try_from(jobs % 3)?] {
            let drawn = workload.sample_alphabet(&(), previous.as_ref(), &mut draw)?;
            let action = match chooser.as_mut() {
                Some(chooser) => workload.resolve_action(&target, &drawn, chooser)?,
                None => drawn,
            };
            workload.apply_action(&mut target, &action, &mut reached)?;
            actions += 1;
            previous = Some(action);
            if reached {
                break;
            }
            if workload.execution_disposition(&target).is_terminal() {
                terminal_jobs += 1;
                break;
            }
        }
        jobs += 1;
        if reached {
            break;
        }
    }
    let reached = workload.config.goal(target.state);
    Ok(serde_json::json!({
        "seed": seed,
        "jobs": jobs,
        "jobs_to_goal": reached.then_some(jobs),
        "censored": !reached,
        "actions": actions,
        "terminal_jobs": terminal_jobs,
        "root_profile": root_profile,
        "job_actions": ROOT_JOB_ACTIONS,
    }))
}

pub fn run(
    workload: &Workload,
    seed: u64,
    budget: u64,
    verify: bool,
) -> Result<serde_json::Value, Box<dyn Error>> {
    campaign(workload, seed, budget, verify, 1, SearchSettings::default())
}

fn campaign<K: Keying>(
    workload: &Workload<K>,
    seed: u64,
    budget: u64,
    verify: bool,
    workers: u32,
    settings: SearchSettings,
) -> Result<serde_json::Value, Box<dyn Error>> {
    workload.config.validate()?;
    if workload.scale.is_some() {
        return Err("scaled runs use run_scaled".into());
    }
    if budget == 0 || budget > 2_000_000 {
        return Err("work budget must be 1..=2000000".into());
    }
    let mut config = campaign_config(workload, seed, budget, workers);
    settings.apply(&mut config);
    let mut stream = BoundedStream(Vec::new());
    let started = telemetry_now();
    let live = run_campaign_checkpointed_with_options(
        workload,
        &config,
        &CampaignOrigin::Genesis,
        &mut stream,
        None,
        CampaignExecutionOptions {
            work_budget: Some(budget),
            ..Default::default()
        },
    )?;
    let elapsed = started.elapsed().as_secs_f64();
    let shadow = workload.keying.shadow()?;
    if verify {
        let replay = replay_campaign_checkpointed(workload, &stream.0, None, None)?;
        if replay != live {
            return Err("campaign replay mismatch".into());
        }
        let mut again = BoundedStream(Vec::new());
        run_campaign_checkpointed_with_options(
            workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut again,
            None,
            CampaignExecutionOptions {
                work_budget: Some(budget),
                ..Default::default()
            },
        )?;
        if again.0 != stream.0 {
            return Err("fixed-work rerun mismatch".into());
        }
    }
    let report = live.0;
    if let Some(witness) = &report.objective_witness {
        let mut state = workload.config.initial();
        for action in &witness.actions {
            state = workload.config.step(state, *action);
        }
        if !workload.config.goal(state) {
            return Err("invalid objective witness".into());
        }
    }
    let route_length = match &workload.config {
        World::Route(w) => w.length,
        _ => 0,
    };
    let route_evidence = route::summarize(
        &report.archive.evidence.route_trace,
        &stream.0,
        route_length,
    )?;
    let mut work = 0;
    let mut executions = 0;
    let mut first_objective_execution = None;
    let mut jobs_after_budget = 0_usize;
    let mut first_objective_work = None;
    let mut continuation_work = 0;
    let mut continuation_jobs = 0;
    let mut pre_objective_continuation_jobs = 0;
    let mut pre_objective_continuation_work = 0;
    let parents: std::collections::BTreeMap<u64, (u16, u16)> = report
        .archive
        .evidence
        .job_parents
        .iter()
        .map(|&(sequence, tier, place)| (sequence, (tier, place)))
        .collect();
    let mut parent_draws =
        std::collections::BTreeMap::<(bool, &str, Option<u8>, u16, u16), u64>::new();
    let mut skipped_draws = std::collections::BTreeMap::<(bool, &str, Option<u8>), u64>::new();
    let mut timeline = Vec::new();
    for record in stream_records(&stream.0)? {
        if let CampaignStreamRecord::Skip(skip) = &record {
            *skipped_draws
                .entry((
                    first_objective_work.is_none(),
                    path_name(skip.selector.path),
                    skip.selector.tier_rank,
                ))
                .or_default() += 1;
        }
        if let CampaignStreamRecord::Job(job) = &record {
            executions += 1;
            let job_work = job.execution_work;
            if work >= budget {
                jobs_after_budget += 1;
            }
            work += job_work;
            if let Some(&(tier, place)) = parents.get(&job.sequence) {
                timeline.push([work - job_work, u64::from(tier), u64::from(place)]);
                *parent_draws
                    .entry((
                        first_objective_work.is_none(),
                        path_name(job.selector.path),
                        job.selector.tier_rank,
                        tier,
                        place,
                    ))
                    .or_default() += 1;
            } else if job_work != 0 {
                return Err("job with work has no recorded parent".into());
            }
            if job.selector.path == SelectorPath::Continuation {
                continuation_work += job_work;
                continuation_jobs += 1;
                if first_objective_work.is_none() {
                    pre_objective_continuation_jobs += 1;
                    pre_objective_continuation_work += job_work;
                }
            }
            if job
                .decisions
                .contains(&CampaignAdmissionDecision::Objective)
            {
                first_objective_work.get_or_insert(work);
                first_objective_execution.get_or_insert(job.sequence);
            }
        }
    }
    if parents.len() != parent_draws.values().sum::<u64>() as usize {
        return Err("recorded job parents do not match stream jobs".into());
    }
    let parent_draws: Vec<_> = parent_draws
        .into_iter()
        .map(|((pre_objective, path, rank, tier, place), jobs)| {
            serde_json::json!([pre_objective, path, rank, tier, place, jobs])
        })
        .collect();
    let skipped_draws: Vec<_> = skipped_draws
        .into_iter()
        .map(|((pre_objective, path, rank), skips)| {
            serde_json::json!([pre_objective, path, rank, skips])
        })
        .collect();
    if jobs_after_budget >= config.window {
        return Err("a job was reserved after the work budget was spent".into());
    }
    if work != report.execution_work || executions != report.executions_completed {
        return Err("independent execution or work accounting mismatch".into());
    }
    if first_objective_work != report.work_to_first_objective
        || first_objective_execution != report.executions_to_first_objective
        || first_objective_work.is_some() != report.objective_witness.is_some()
    {
        return Err("independent first-objective scoring mismatch".into());
    }
    if matches!(workload.config, World::Chain(_))
        && report
            .archive
            .evidence
            .chain_work_by_parent_stage
            .iter()
            .sum::<u64>()
            != work
    {
        return Err("chain parent work differs from independently summed execution work".into());
    }
    if matches!(workload.config, World::Chain(_))
        && report
            .archive
            .evidence
            .chain_pre_objective_work_by_parent_stage
            .iter()
            .sum::<u64>()
            != first_objective_work.unwrap_or(work)
    {
        return Err("chain pre-objective work differs from first-objective accounting".into());
    }
    let (decision_stream_sha256, decision_stream_records) = decision_stream(&stream.0)?;
    let mut out = serde_json::json!({"seed":seed,"broken":workload.broken,
        "config":workload.config,"work_budget":budget,"work":work,"work_overshoot":work.saturating_sub(budget),
        "work_unit":workload.config.work_unit(),
        "mixture_policy":report.mixture_policy,
        "stop_on_objective":report.stop_campaign_on_objective,
        "executions":executions,"first_objective_execution":first_objective_execution,
        "chain_parent_selections":report.archive.evidence.chain_parent_selections,
        "chain_work_by_parent_stage":report.archive.evidence.chain_work_by_parent_stage,
        "chain_selected_charge":report.archive.evidence.chain_selected_charge,
        "route_evidence":route_evidence,"layout":workload.config.layout(),"parent_timeline":timeline,
        "pre_objective_continuation_jobs":pre_objective_continuation_jobs,
        "pre_objective_continuation_work":pre_objective_continuation_work,
        "continuation_work":continuation_work,"continuation_jobs":continuation_jobs,
        "first_objective_work":first_objective_work,"parent_draws":parent_draws,"skipped_draws":skipped_draws,
        "success":first_objective_work.is_some_and(|w|w<=budget),
        "elapsed_seconds":elapsed,"stream_bytes":stream.0.len(),"stream_sha256":report.stream_sha256,
        "decision_stream_sha256":decision_stream_sha256,"decision_stream_records":decision_stream_records,
        "resident_memory_bytes":report.resident_memory_bytes,"history_memory_bytes":report.history_memory_bytes,
        "evidence":report.archive.evidence,
        "exported_entry_count":report.archive.entries.len(),"live_entries":report.live_entries,"selector":report.archive.selector,"verified":verify});
    if let Some(shadow) = shadow {
        out["shadow"] = serde_json::to_value(shadow)?;
    }
    if let World::Stairs(w) = &workload.config {
        out["stairs"] = w.report(&report.archive.entries);
    }
    Ok(out)
}

fn campaign_config<K: Keying>(
    workload: &Workload<K>,
    seed: u64,
    budget: u64,
    workers: u32,
) -> CampaignConfig<Workload<K>> {
    let scale = workload.scale.unwrap_or(Scale {
        workers,
        window: searcher::search::campaign::default_window(workers),
        ..Scale::default()
    });
    CampaignConfig {
        campaign_seed: seed,
        workers: scale.workers,
        execution_budget: budget,
        host: "tiny-worlds".into(),
        wall_budget: None,
        stop_rollout_on_objective: true,
        stop_campaign_on_objective: workload.scale.is_none(),
        archive_entry_limit: scale.archive_entries,
        window: scale.window,
        memory_budget_mib: Some(scale.memory_budget_mib),
        materialize_final_artifacts: workload.scale.is_none(),
        run: (),
        mixture: workload.config.mixture(),
        retention: RetentionPolicy::Unprobed,
        objective_witness_path: None,
        minimize: None,
    }
}

#[allow(clippy::disallowed_methods)]
fn telemetry_now() -> std::time::Instant {
    std::time::Instant::now()
}

impl<L: Lane> searcher::search::snapshot_codec::CheckpointSnapshot for Snapshot<L> {
    type Codec = searcher::search::snapshot_codec::PostcardSnapshotCodec<Self>;
    fn checkpoint_image_charge(
        &self,
        budget: &mut searcher::search::checkpoint::image::ImageBudget,
    ) -> Result<(), Box<dyn std::error::Error>> {
        budget.reserve(std::mem::size_of::<Self>())?;
        budget.reserve(self.lane.heap_bytes())?;
        budget.array::<u8>(self.payload.capacity())
    }
    fn freeze_checkpoint(&self) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(self.clone())
    }
}

#[cfg(test)]
mod equivalence_tests;
#[cfg(test)]
fn test_seed() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let seed = std::env::var("HARMONY_TEST_SEED")
        .ok()
        .and_then(|seed| seed.parse().ok())
        .unwrap_or_else(|| {
            std::collections::hash_map::RandomState::new()
                .build_hasher()
                .finish()
        });
    eprintln!("campaign replay seed: {seed}");
    seed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_archive_in_another_format_is_refused_with_both_names() {
        let report = ArchiveReport::<AssertionKey> {
            format: ArchiveFormat::default(),
            entries: Vec::new(),
            evidence: Evidence::default(),
            selector: Default::default(),
        };
        let mut value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["format"], ARCHIVE_FORMAT);
        assert_eq!(
            serde_json::from_value::<ArchiveReport<AssertionKey>>(value.clone()).unwrap(),
            report
        );
        value["format"] = "tiny-world-archive-v0".into();
        let refused = serde_json::from_value::<ArchiveReport<AssertionKey>>(value.clone())
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains(&format!(
                "archive format is tiny-world-archive-v0; this build reads {ARCHIVE_FORMAT}"
            )),
            "{refused}"
        );
        value.as_object_mut().unwrap().remove("format");
        let refused = serde_json::from_value::<ArchiveReport<AssertionKey>>(value)
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains(&format!(
                "archive names no format; this build reads {ARCHIVE_FORMAT}"
            )),
            "{refused}"
        );
    }

    fn snap(state: State) -> Snapshot {
        Snapshot {
            state,
            payload: Vec::new(),
            lane: (),
        }
    }
    use searcher::search::campaign::{
        CampaignCheckpoint, SavedStates, SnapshotCheckpoint, SnapshotCheckpointEntry,
    };
    use std::collections::BTreeMap;

    fn world() -> Workload {
        Workload {
            keying: crate::Hand::<false>,
            config: World::Resource(resource::Config {
                initial_charge: 0,
                initial_health: 3,
                barrier_charge: 5,
                route_cost: 0,
                health_cost: 0,
                refill_amount: 1,
                max_charge: 8,
                corridor_len: 1,
                refill_health_cost: 0,
            }),
            broken: false,
            scale: None,
        }
    }

    #[test]
    fn saved_states_resumed_under_the_same_key_keep_their_recorded_keys() {
        let w: Workload = Workload {
            keying: crate::Hand::<false>,
            config: World::Maze(maze::Config {
                length: 8,
                pattern: 0b1011_0011,
                reverse_actions: false,
            }),
            broken: false,
            scale: None,
        };
        let directory = std::env::temp_dir().join(format!(
            "tiny-world-saved-states-{}-{}",
            std::process::id(),
            crate::test_seed()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        let mut config = campaign_config(&w, crate::test_seed(), 5000, 1);
        config.stop_campaign_on_objective = false;
        let (source, _) = run_campaign_checkpointed_with_options(
            &w,
            &config,
            &CampaignOrigin::Genesis,
            &mut BoundedStream(Vec::new()),
            None,
            CampaignExecutionOptions {
                checkpoints: Some(searcher::search::checkpoint::CheckpointPlan {
                    directory: directory.clone(),
                    every: None,
                    on_marks: false,
                    on_top_progress: false,
                    on_stall: false,
                    checkpoint_owner: None,
                    max_image_bytes: 64 << 20,
                }),
                ..CampaignExecutionOptions::default()
            },
        )
        .unwrap();
        let checkpoint = std::fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.to_string_lossy().ends_with("-final.ckpt"))
            .unwrap();
        let states = SavedStates::open(&w, &checkpoint).unwrap();
        let digest = states.file_sha256().to_owned();
        let recorded = source
            .archive
            .entries
            .iter()
            .map(|entry| (entry.input.actions.clone(), entry.key))
            .collect::<BTreeMap<_, _>>();
        config.execution_budget = 1;
        let (resumed, _) = run_campaign_checkpointed_with_options(
            &w,
            &config,
            &CampaignOrigin::SavedStates { states },
            &mut BoundedStream(Vec::new()),
            None,
            CampaignExecutionOptions::default(),
        )
        .unwrap();
        std::fs::remove_dir_all(&directory).unwrap();
        assert_eq!(resumed.origin.kind, "saved_states");
        assert_eq!(
            resumed.origin.checkpoint_sha256.as_deref(),
            Some(digest.as_str())
        );
        let imported = resumed
            .archive
            .entries
            .iter()
            .filter(|entry| entry.created_execution == 0 && !entry.input.actions.is_empty())
            .collect::<Vec<_>>();
        assert!(imported.len() > 1);
        for entry in imported {
            assert_eq!(recorded.get(&entry.input.actions), Some(&entry.key));
        }
    }

    fn stall_records<K: Keying>(
        w: &Workload<K>,
        config: &CampaignConfig<Workload<K>>,
        world: &str,
    ) -> Vec<serde_json::Value> {
        let run = |label: &str, on_stall: bool| {
            let directory = std::env::temp_dir().join(format!(
                "tiny-world-stall-{world}-{label}-{}-{}",
                std::process::id(),
                config.campaign_seed
            ));
            let _ = std::fs::remove_dir_all(&directory);
            let mut stream = BoundedStream(Vec::new());
            let mut progress = Vec::new();
            let (original, _) = run_campaign_checkpointed_with_options(
                w,
                config,
                &CampaignOrigin::Genesis,
                &mut stream,
                Some(&mut progress),
                CampaignExecutionOptions {
                    checkpoints: Some(searcher::search::checkpoint::CheckpointPlan {
                        directory: directory.clone(),
                        every: None,
                        on_marks: false,
                        on_top_progress: false,
                        on_stall,
                        checkpoint_owner: None,
                        max_image_bytes: 64 << 20,
                    }),
                    ..CampaignExecutionOptions::default()
                },
            )
            .unwrap();
            let stalls = std::fs::read_to_string(directory.join("stalls.jsonl")).ok();
            let mut checkpoints = std::fs::read_dir(&directory)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            checkpoints.sort();
            if on_stall {
                let newest = checkpoints
                    .iter()
                    .rev()
                    .find(|name| name.ends_with("-stall.ckpt"))
                    .expect("a kept stall checkpoint");
                let (resumed, _) = run_campaign_checkpointed_with_options(
                    w,
                    config,
                    &CampaignOrigin::SearchCheckpoint {
                        path: directory.join(newest),
                        expected_sha256: None,
                    },
                    &mut BoundedStream(Vec::new()),
                    None,
                    CampaignExecutionOptions::default(),
                )
                .unwrap();
                assert_eq!(resumed.origin.kind, "search_checkpoint");
                assert_eq!(resumed.archive, original.archive);
                assert_eq!(resumed.executions_completed, original.executions_completed);
            }
            std::fs::remove_dir_all(&directory).unwrap();
            (stream.0, stalls, checkpoints)
        };
        let (plain, no_stalls, _) = run("off", false);
        let (reported, stalls, checkpoints) = run("on", true);
        assert_eq!(plain, reported);
        assert!(no_stalls.is_none());
        let stalls = stalls.expect("the run stalls in its leading tier");
        let mut watches = std::collections::BTreeSet::new();
        let records: Vec<serde_json::Value> = stalls
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        for record in records.iter().rev().take(2) {
            let checkpoint = record["checkpoint"].as_str().unwrap();
            assert!(checkpoints.iter().any(|name| name == checkpoint));
        }
        assert!(
            checkpoints
                .iter()
                .filter(|name| name.ends_with("-stall.ckpt"))
                .count()
                <= 2
        );
        for record in &records {
            assert!(watches.insert((
                record["tier"].to_string(),
                record["tier_yields"].to_string()
            )));
            assert!(
                record["states"]
                    .as_array()
                    .is_some_and(|states| !states.is_empty())
            );
        }
        records
    }

    #[test]
    fn a_stall_report_leaves_the_stream_unchanged_and_names_the_stalled_tier() {
        let w: Workload = Workload {
            keying: crate::Hand::<false>,
            config: World::Maze(maze::Config {
                length: 4,
                pattern: 10,
                reverse_actions: false,
            }),
            broken: false,
            scale: None,
        };
        let mut config = campaign_config(&w, crate::test_seed(), 2_000, 1);
        config.stop_campaign_on_objective = false;
        for record in stall_records(&w, &config, "maze") {
            assert!(
                record["tier"]
                    .as_str()
                    .is_some_and(|tier| tier.starts_with('('))
            );
            assert_eq!(record["signals"], serde_json::json!([]));
            assert_eq!(record["signal_changes_by_action"], serde_json::json!({}));
            assert_eq!(record["choice_sites"], serde_json::json!([]));
            assert!(
                record["states"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|state| state.get("signals").is_none())
            );
        }
    }

    #[test]
    fn a_stall_report_gives_the_signals_of_states_stalled_for_want_of_charge() {
        let config = resource::Config {
            initial_charge: 0,
            initial_health: 3,
            barrier_charge: 15,
            route_cost: 0,
            health_cost: 0,
            refill_amount: 1,
            max_charge: 15,
            corridor_len: 2,
            refill_health_cost: 0,
        };
        let world = World::Resource(config);
        let w = Workload {
            keying: Strategic::new(
                &world,
                false,
                "capacity = 1\ntier = \"resource.place\"\nspread = \"resource.charge\"\nprefer = [[\"resource.health: more\"]]\n",
            )
            .unwrap(),
            config: world,
            broken: false,
            scale: None,
        };
        let mut campaign = campaign_config(&w, crate::test_seed(), 2_000, 1);
        campaign.stop_campaign_on_objective = false;
        let records = stall_records(&w, &campaign, "resource");
        let mut stalled_short = false;
        let mut unchanged = 0;
        for record in &records {
            let signals: Vec<&str> = record["signals"]
                .as_array()
                .unwrap()
                .iter()
                .map(|name| name.as_str().unwrap())
                .collect();
            let charge = signals
                .iter()
                .position(|name| *name == "resource.charge")
                .unwrap();
            let broken = signals.iter().position(|name| *name == "broken").unwrap();
            for state in record["states"].as_array().unwrap() {
                let values = state["signals"].as_array().unwrap();
                assert_eq!(values.len(), signals.len());
                stalled_short |=
                    values[charge].as_i64().unwrap() < i64::from(config.barrier_charge);
            }
            let changes = record["signal_changes_by_action"].as_object().unwrap();
            for (action, counts) in changes {
                let compared = counts["compared"].as_u64().unwrap();
                let held = counts["unchanged"].as_u64().unwrap();
                assert!(held <= compared);
                assert!(compared <= record["draws_by_action"][action].as_u64().unwrap());
                let changed = counts["changed"].as_array().unwrap();
                assert_eq!(changed.len(), signals.len());
                assert_eq!(changed[broken], 0);
                let changed: Vec<u64> = changed
                    .iter()
                    .map(|count| count.as_u64().unwrap())
                    .collect();
                assert!(changed.iter().all(|count| *count <= compared - held));
                assert!(changed.iter().sum::<u64>() >= compared - held);
                unchanged += held;
            }
        }
        assert!(stalled_short);
        assert!(unchanged > 0);
    }

    #[test]
    fn a_stall_report_counts_the_options_offered_and_taken_at_each_choice_site() {
        let w: Workload = Workload {
            keying: crate::Hand::<false>,
            config: World::Picks(picks::Config {
                depth: 6,
                seed: 7,
                default_right: 3,
                draw: picks::Draw::Choices(ChoicePolicy::default()),
            }),
            broken: false,
            scale: None,
        };
        let mut config = campaign_config(&w, crate::test_seed(), 2_000, 1);
        config.stop_campaign_on_objective = false;
        let records = stall_records(&w, &config, "picks");
        let mut at_site = false;
        for record in &records {
            assert_eq!(record["signals"], serde_json::json!([]));
            let sites = record["choice_sites"].as_array().unwrap();
            assert!(!sites.is_empty());
            for site in sites {
                let World::Picks(layout) = &w.config else {
                    unreachable!()
                };
                let index = u8::try_from(site["site"].as_u64().unwrap()).unwrap();
                assert_eq!(site["name"], format!("pick-{index}"));
                let asks = site["asks"].as_u64().unwrap();
                let offered = site["offered"].as_object().unwrap();
                let pick = layout.pick(index);
                assert_eq!(offered.len(), pick.offered.len());
                assert!(offered.values().all(|count| count.as_u64() == Some(asks)));
                let resolved: u64 = site["resolved"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(label, count)| {
                        assert!(offered.contains_key(label));
                        count.as_u64().unwrap()
                    })
                    .sum();
                assert_eq!(resolved, asks);
                at_site |= site["top_states_at_site"].as_u64().unwrap() > 0;
            }
        }
        assert!(at_site);
    }

    #[test]
    fn a_picks_job_reports_the_slots_it_resolved_and_not_a_copied_answer() {
        use searcher::search::choice::{Choice, ChoiceProfile};
        let layout = picks::Config {
            depth: 6,
            seed: 7,
            default_right: 3,
            draw: picks::Draw::Choices(ChoicePolicy::default()),
        };
        let copied = picks::Config::encode(Choice::Option(layout.pick(0).right));
        let w: Workload = Workload {
            keying: crate::Hand::<false>,
            config: World::Picks(layout),
            broken: false,
            scale: None,
        };
        let plan = ChoicePlan::new(
            &ChoicePolicy::default(),
            ChoiceProfile::default(),
            7,
            None,
            Vec::new(),
            Vec::new(),
        );
        let mut target = w.new_target().unwrap();
        let origin = w.snapshot(&mut target).unwrap();
        let result = w
            .execute_job(
                &(),
                &mut target,
                &origin,
                &[],
                false,
                &[copied, picks::ASK],
                RetentionPolicy::Unprobed,
                searcher::search::rollout::RolloutStop::new(false).with_choices(Some(plan.clone())),
            )
            .unwrap();
        let records: Vec<Vec<ChoiceRecord>> = result
            .actions
            .iter()
            .enumerate()
            .map(|(index, action)| {
                w.choice_records(
                    &(),
                    Some(&plan),
                    index,
                    &action.action,
                    &action.observations,
                )
            })
            .collect();
        let pick = layout.pick(1);
        assert_eq!(
            records,
            [
                Vec::new(),
                vec![ChoiceRecord::new(
                    1,
                    pick.offered,
                    pick.default,
                    picks::Config::decode(result.actions[1].action),
                )],
            ]
        );
    }

    #[test]
    fn restore_preserves_state_but_not_cumulative_work() {
        let w = world();
        let mut t = w.new_target().unwrap();
        let snapshot = w.snapshot(&mut t).unwrap();
        w.apply_action(&mut t, &1, &mut false).unwrap();
        assert_ne!(t.state, snapshot.state);
        w.restore(&mut t, &snapshot).unwrap();
        assert_eq!(t.state, snapshot.state);
        assert_eq!(w.execution_work(&t), 1);
    }

    #[test]
    fn root_objective_does_not_depend_on_first_observation() {
        let w = world();
        let mut state = w.config.initial();
        for _ in 0..5 {
            state = w.config.step(state, 1);
        }
        for _ in 0..3 {
            state = w.config.step(state, 0);
        }
        assert!(w.config.goal(state));
        assert_root_objective(w, state);
    }

    fn assert_root_objective(w: Workload, state: State) {
        let snapshots = SnapshotCheckpoint {
            format: w.checkpoint_format().into(),
            entries: vec![SnapshotCheckpointEntry {
                id: 0,
                snapshot: snap(state),
            }],
        };
        let origin = CampaignOrigin::SnapshotRoot {
            checkpoint: CampaignCheckpoint {
                path: "constructed-root".into(),
                file_sha256: snapshots.sha256().unwrap(),
                snapshots,
            },
        };
        let config = campaign_config(&w, crate::test_seed(), 20, 1);
        let mut stream = BoundedStream(Vec::new());
        let (report, _) = run_campaign_checkpointed_with_options(
            &w,
            &config,
            &origin,
            &mut stream,
            None,
            CampaignExecutionOptions::default(),
        )
        .unwrap();
        assert_eq!(report.work_to_first_objective, Some(0));
        assert_eq!(report.execution_work, 0);
    }

    #[test]
    fn maze_goal_at_root_and_delayed_submit_are_distinct() {
        let config = maze::Config {
            length: 5,
            pattern: 19,
            reverse_actions: true,
        };
        let w = Workload {
            keying: crate::Hand::<false>,
            config: World::Maze(config),
            broken: false,
            scale: None,
        };
        let prefix = (0..5).fold(config.initial(), |s, bit| {
            config.step(s, ((12 >> bit) & 1) as u8)
        });
        let mut target = w.new_target().unwrap();
        w.restore(&mut target, &snap(State::Maze(prefix))).unwrap();
        assert!(w.rollout_observations(&target).is_empty());
        assert!(!w.objective_reached(&(), &target).unwrap());
        let terminal = config.step(prefix, 0);
        assert!(
            w.restore(
                &mut target,
                &snap(State::Maze(maze::State {
                    goal: false,
                    ..terminal
                }))
            )
            .is_err()
        );
        assert_root_objective(w, State::Maze(terminal));
    }

    #[test]
    fn restore_rejects_wrong_family_and_out_of_bounds_state() {
        let w = world();
        let mut target = w.new_target().unwrap();
        assert!(
            w.restore(
                &mut target,
                &snap(State::Maze(maze::State {
                    place: 0,
                    history: 0,
                    goal: false
                }))
            )
            .is_err()
        );
        let mut invalid = match w.config.initial() {
            State::Resource(s) => s,
            _ => unreachable!(),
        };
        invalid.charge = 255;
        assert!(
            w.restore(&mut target, &snap(State::Resource(invalid)))
                .is_err()
        );
        let World::Resource(config) = w.config else {
            unreachable!()
        };
        let terminal = resource::State {
            place: config.corridor_len + 1,
            charge: 0,
            health: 1,
            goal: true,
        };
        w.restore(&mut target, &snap(State::Resource(terminal)))
            .unwrap();
        assert!(
            w.restore(
                &mut target,
                &snap(State::Resource(resource::State {
                    goal: false,
                    ..terminal
                }))
            )
            .is_err()
        );
    }

    #[test]
    fn exhaustive_small_oracle_matches_independent_resource_bound() {
        for corridor_len in 1..=3 {
            for route_cost in 0..=2 {
                for max_charge in 4..=12 {
                    for health_cost in 0..=3 {
                        let config = resource::Config {
                            corridor_len,
                            route_cost,
                            max_charge,
                            health_cost,
                            ..match world().config {
                                World::Resource(w) => w,
                                _ => unreachable!(),
                            }
                        };
                        let expected =
                            max_charge >= 5 + corridor_len * route_cost && 3 > health_cost;
                        assert_eq!(config.reachable().unwrap(), expected, "{config:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn observations_are_absent_until_an_action_and_never_restore_stale_events() {
        let w = world();
        let mut t = w.new_target().unwrap();
        assert!(w.rollout_observations(&t).is_empty());
        let root = w.snapshot(&mut t).unwrap();
        w.apply_action(&mut t, &1, &mut false).unwrap();
        assert_eq!(w.rollout_observations(&t).len(), 1);
        w.restore(&mut t, &root).unwrap();
        assert!(w.rollout_observations(&t).is_empty());
        assert!(!w.objective_reached(&(), &t).unwrap());
    }

    #[test]
    fn fixed_work_campaign_replays_and_accounts_for_every_transition() {
        let report = run(&world(), crate::test_seed(), 1000, true).unwrap();
        assert_eq!(report["verified"], true);
        let mut broken = world();
        broken.broken = true;
        let control = run(&broken, crate::test_seed(), 1000, true).unwrap();
        assert_eq!(control["verified"], true);
    }

    #[test]
    fn every_family_uses_the_real_engine_and_replays_at_fixed_work() {
        let configs = [
            World::Deadline(deadline::Config {
                length: 2,
                initial_time: 8,
                fast_ticks: 1,
                slow_ticks: 4,
                obstacle_ticks: 2,
            }),
            World::Delayed(delayed::Config {
                horizon: 5,
                distractions: 8,
                mode: delayed::Mode::Wait,
                placement: delayed::Placement::Identity,
                sticky_credit: false,
                ammo: 0,
            }),
            World::DeadlineActions(deadline_actions::Config {
                actions: actions::Config {
                    segment_len: 2,
                    return_to_land: true,
                    observable: true,
                    water_action: 1,
                },
                initial_time: 12,
                action_ticks: [1, 2, 1, 1],
            }),
            World::Maze(maze::Config {
                length: 4,
                pattern: 10,
                reverse_actions: true,
            }),
            World::Actions(actions::Config {
                segment_len: 3,
                return_to_land: true,
                observable: true,
                water_action: 1,
            }),
            World::Actions(actions::Config {
                segment_len: 3,
                return_to_land: true,
                observable: false,
                water_action: 1,
            }),
        ];
        for config in configs {
            for broken in [false, true] {
                let workload = Workload {
                    keying: crate::Hand::<false>,
                    config: config.clone(),
                    broken,
                    scale: None,
                };
                let report = run(&workload, crate::test_seed(), 1000, true).unwrap();
                assert_eq!(report["verified"], true);
            }
        }
    }

    #[test]
    fn deadline_restore_keeps_clock_separate_from_cumulative_work() {
        let config = deadline::Config {
            length: 2,
            initial_time: 10,
            fast_ticks: 1,
            slow_ticks: 4,
            obstacle_ticks: 2,
        };
        let w = Workload {
            keying: crate::Hand::<false>,
            config: World::Deadline(config),
            broken: false,
            scale: None,
        };
        let mut t = w.new_target().unwrap();
        let root = w.snapshot(&mut t).unwrap();
        w.apply_action(&mut t, &0, &mut false).unwrap();
        assert_eq!(
            t.state,
            State::Deadline(deadline::State {
                position: 1,
                remaining: 6,
                ..config.initial()
            })
        );
        assert_eq!(w.execution_work(&t), 1);
        w.restore(&mut t, &root).unwrap();
        assert_eq!(t.state, root.state);
        assert_eq!(w.execution_work(&t), 1);
        let goal = [1, 1, 1, 1, 2]
            .into_iter()
            .fold(config.initial(), |s, a| config.step(s, a));
        assert_root_objective(w, State::Deadline(goal));
    }

    #[test]
    fn clock_preference_preserves_fast_arrivals_in_both_orders() {
        use searcher::search::archive::{Archive, ArchiveCandidate};
        let w = deadline::Config {
            length: 2,
            initial_time: 10,
            fast_ticks: 1,
            slow_ticks: 4,
            obstacle_ticks: 3,
        };
        for fast_first in [false, true] {
            for broken in [false, true] {
                let mut archive = Archive::<u8, Key, bool, State>::new();
                for fast in [fast_first, !fast_first] {
                    let suffix = if fast { vec![1, 1, 1, 1] } else { vec![0, 0] };
                    let state = suffix.iter().fold(w.initial(), |s, &a| w.step(s, a));
                    archive
                        .insert(
                            None,
                            suffix.len() as u64,
                            ArchiveCandidate {
                                cost: suffix.len() as u64,
                                suffix,
                                key: w.key(state, broken),
                                milestones: false,
                            },
                            State::Deadline(state),
                        )
                        .unwrap();
                }
                assert_eq!(archive.active_count(), 1);
                let (id, _) = archive
                    .select_parent(&mut RomuDuoJrRand::with_seed(crate::test_seed()))
                    .unwrap();
                let (reports, snapshots) = archive.take_entry_reports_and_snapshots();
                let snapshot_id = reports[id].id;
                let (_, State::Deadline(state)) = snapshots
                    .into_iter()
                    .find(|(i, _)| *i == snapshot_id)
                    .unwrap()
                else {
                    panic!("deadline snapshot");
                };
                assert_eq!(state.remaining, if broken { 2 } else { 6 });
                assert_eq!(w.goal(w.step(state, 2)), !broken);
            }
        }
    }

    #[test]
    fn archive_retains_useful_maze_history_in_both_arrival_orders() {
        use searcher::search::archive::{Archive, ArchiveCandidate};
        let maze = maze::Config {
            length: 4,
            pattern: 10,
            reverse_actions: false,
        };
        for order in [[10_u16, 5_u16], [5_u16, 10_u16]] {
            for broken in [false, true] {
                let mut archive = Archive::<u8, Key, bool, State>::new();
                for history in order {
                    let input: Vec<u8> = (0..4).map(|bit| ((history >> bit) & 1) as u8).collect();
                    let state = input.iter().fold(maze.initial(), |s, &a| maze.step(s, a));
                    assert_eq!(state.place, 4);
                    archive
                        .insert(
                            None,
                            0,
                            ArchiveCandidate {
                                cost: input.len() as u64,
                                suffix: input,
                                key: maze.key(state, broken),
                                milestones: false,
                            },
                            State::Maze(state),
                        )
                        .unwrap();
                }
                assert_eq!(archive.active_count(), if broken { 1 } else { 2 });
                if !broken {
                    let (_, snapshots) = archive.take_entry_reports_and_snapshots();
                    assert!(snapshots.iter().any(|(_, state)| matches!(state,
                        State::Maze(state) if state.history == 10)));
                }
            }
        }
    }

    #[test]
    fn reversed_history_diagnostics_do_not_depend_on_control_or_reset_choice() {
        use searcher::search::rollout::Outcome;
        let config = maze::Config {
            length: 5,
            pattern: 19,
            reverse_actions: true,
        };
        for broken in [false, true] {
            let w = Workload {
                keying: crate::Hand::<false>,
                config: World::Maze(config),
                broken,
                scale: None,
            };
            let mut evidence = Evidence::default();
            for history in [12_u16, 19_u16] {
                let before = maze::State {
                    place: 4,
                    history: history & 15,
                    goal: false,
                };
                let action = ((history >> 4) & 1) as u8;
                let after = config.step(before, action);
                let result = CampaignActionResult::<Workload> {
                    action,
                    observations: vec![Observation {
                        work_in_job: 0,
                        job_work: None,
                        asked: None,
                        before: State::Maze(before),
                        after: State::Maze(after),
                    }],
                    milestones: false,
                    outcome: Outcome::default(),
                    candidate: None,
                    cost: searcher::search::cost::ShrinkCost::ZERO,
                    bound: searcher::search::cost::ShrinkCost::ZERO,
                };
                w.merge_action_evidence(&mut evidence, &result, 0, || Ok(Input::default()))
                    .unwrap();
            }
            assert_eq!(evidence.correct_history_arrivals, 1);
            assert_eq!(evidence.wrong_history_arrivals, 1);
            for action in [0, 2] {
                let before = maze::State {
                    place: 5,
                    history: 19,
                    goal: false,
                };
                let result = CampaignActionResult::<Workload> {
                    action,
                    observations: vec![Observation {
                        work_in_job: 0,
                        job_work: None,
                        asked: None,
                        before: State::Maze(before),
                        after: State::Maze(config.step(before, action)),
                    }],
                    milestones: false,
                    outcome: Outcome::default(),
                    candidate: None,
                    cost: searcher::search::cost::ShrinkCost::ZERO,
                    bound: searcher::search::cost::ShrinkCost::ZERO,
                };
                w.merge_action_evidence(&mut evidence, &result, 0, || Ok(Input::default()))
                    .unwrap();
            }
            assert_eq!(evidence.loop_resets, 1);
            assert_eq!(evidence.explicit_resets, 1);
        }
    }

    #[test]
    fn tagged_world_schema_rejects_unconsumed_fields() {
        let valid = serde_json::to_value(world().config).unwrap();
        let mut extra = valid.clone();
        extra["unconsumed"] = serde_json::json!(1);
        assert!(serde_json::from_value::<World>(extra).is_err());
        let mut wrong = valid;
        wrong["family"] = serde_json::json!("unsupported");
        assert!(serde_json::from_value::<World>(wrong).is_err());
    }

    #[test]
    fn a_run_with_many_workers_replays() {
        let w = world();
        for workers in [2, 4, 16] {
            let report = run_kept(
                &w,
                Keep::Portfolio,
                crate::KeyKind::Hand,
                None,
                false,
                test_seed(),
                2000,
                true,
                workers,
                SearchSettings::default(),
            )
            .unwrap();
            assert_eq!(report["verified"], true, "{workers} workers");
        }
        assert!(
            run_kept(
                &w,
                Keep::Portfolio,
                crate::KeyKind::Hand,
                None,
                false,
                test_seed(),
                2000,
                true,
                65,
                SearchSettings::default()
            )
            .is_err()
        );
        let clock = Workload {
            keying: crate::Hand::<false>,
            config: World::PassiveClock(passive_clock::Config {
                events: vec![33, 41],
                holds: vec![passive_clock::HoldBand {
                    minimum: 8,
                    maximum: 8,
                    weight: 1,
                }],
            }),
            broken: false,
            scale: None,
        };
        let spent = run_kept(
            &clock,
            Keep::Portfolio,
            crate::KeyKind::Hand,
            None,
            false,
            test_seed(),
            40,
            true,
            16,
            SearchSettings::default(),
        )
        .unwrap();
        assert_eq!(spent["verified"], true);
        assert!(spent["work"].as_u64().unwrap() >= 16 * 8);
        assert!(spent["work_overshoot"].as_u64().unwrap() > 0);
    }

    #[test]
    fn capacity_two_keeps_two_holders_under_one_preference_and_replays() {
        assert_eq!(
            (Key::<false>::capacity(), Key::<false>::preferences()),
            (1, 2)
        );
        assert_eq!(
            (Key::<true>::capacity(), Key::<true>::preferences()),
            (2, 1)
        );
        let w = world();
        let key = w.config.key(w.config.initial(), false);
        assert_eq!(key.kept::<true>().kept::<false>(), key);
        for keep in [Keep::Portfolio, Keep::CapacityTwo] {
            let report = run_kept(
                &w,
                keep,
                crate::KeyKind::Hand,
                None,
                false,
                test_seed(),
                2000,
                true,
                1,
                SearchSettings::default(),
            )
            .unwrap();
            assert_eq!(report["verified"], true);
            assert_eq!(report["keep"], serde_json::to_value(keep).unwrap());
        }
        assert!(serde_json::from_str::<Keep>(r#""capacity_two""#).is_ok());
        assert!(serde_json::from_str::<Keep>(r#""CapacityTwo""#).is_err());
    }

    #[test]
    fn broken_representation_erases_only_stock_preference() {
        let correct = world();
        let mut broken = world();
        broken.broken = true;
        let mut t = correct.new_target().unwrap();
        let empty = correct.current_key(&t).unwrap();
        for _ in 0..5 {
            correct.apply_action(&mut t, &1, &mut false).unwrap();
        }
        let stocked = correct.current_key(&t).unwrap();
        assert!(stocked.preference_cmp(0, empty).is_gt());
        assert_eq!(broken.current_key(&t).unwrap(), empty);
        assert_eq!(
            correct.config.step(t.state, 0),
            broken.config.step(t.state, 0)
        );
    }

    #[test]
    fn scale_bounds_are_checked() {
        assert!(Scale::default().validate().is_ok());
        for bad in [
            Scale {
                workers: 0,
                ..Scale::default()
            },
            Scale {
                workers: 65,
                ..Scale::default()
            },
            Scale {
                window: 0,
                ..Scale::default()
            },
            Scale {
                window: 513,
                ..Scale::default()
            },
            Scale {
                memory_budget_mib: 16_385,
                ..Scale::default()
            },
            Scale {
                archive_entries: MAX_ARCHIVE_ENTRIES + 1,
                ..Scale::default()
            },
            Scale {
                action_cost_ns: 10_000_001,
                ..Scale::default()
            },
            Scale {
                action_sleep_ns: 10_000_001,
                ..Scale::default()
            },
            Scale {
                snapshot_bytes: (1 << 20) + 1,
                ..Scale::default()
            },
        ] {
            assert!(bad.validate().is_err());
        }
    }

    #[test]
    fn payload_leaves_the_search_unchanged() {
        let config = World::Maze(maze::Config {
            length: 6,
            pattern: 0b101101,
            reverse_actions: false,
        });
        let seed = crate::test_seed();
        let scaled = |snapshot_bytes| {
            let workload = Workload {
                keying: crate::Hand::<false>,
                config: config.clone(),
                broken: false,
                scale: Some(Scale {
                    workers: 2,
                    window: 4,
                    action_cost_ns: 1_000,
                    snapshot_bytes,
                    ..Scale::default()
                }),
            };
            run_scaled(
                &workload,
                seed,
                5_000,
                &mut std::io::sink(),
                SearchSettings::default(),
            )
            .unwrap()
        };
        let plain = scaled(0);
        let padded = scaled(4096);
        for field in ["work", "first_objective_work", "selector", "live_entries"] {
            assert_eq!(plain[field], padded[field]);
        }
        let resident = |r: &serde_json::Value| r["resident_memory_bytes"].as_u64().unwrap();
        assert!(resident(&padded) >= resident(&plain) + 4096);
        let workload = Workload {
            keying: crate::Hand::<false>,
            config,
            broken: false,
            scale: Some(Scale::default()),
        };
        assert!(run(&workload, seed, 1000, false).is_err());
        assert!(
            run_scaled(
                &workload,
                seed,
                Scale::MAX_WORK_BUDGET + 1,
                &mut std::io::sink(),
                SearchSettings::default()
            )
            .is_err()
        );
        let mut target = workload.new_target().unwrap();
        let snapshot = Workload {
            keying: crate::Hand::<false>,
            scale: Some(Scale {
                snapshot_bytes: 100,
                ..Scale::default()
            }),
            ..workload
        }
        .snapshot(&mut target)
        .unwrap();
        assert_eq!(snapshot.payload.len(), 100);
        assert!(snapshot.payload.iter().any(|&b| b != 0));
    }
}
