// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, fs, path::PathBuf};

use blue_workload::{
    map::Overworld,
    target::{
        BlueAction, BlueTarget, CUR_MAP, CUR_MAP_WIDTH, CURRENT_MENU_ITEM, MAX_MENU_ITEM,
        NUMBER_OF_WARPS, X_COORD, Y_COORD, byte,
    },
};
use machine::{
    gambatte::GambatteMachine,
    gb::{A, ButtonChord, DOWN},
};
use searcher::target::Target;
use sha2::{Digest, Sha256};

const REDS_HOUSE_2F: u8 = 38;
const NAME_MENU_ITEMS: u8 = 3;
const NAME_MENUS: usize = 2;
const NAME_MENU_COOLDOWN: usize = 12;
const SETUP_CHORD_LIMIT: usize = 4_000;
const WALK_STALL_LIMIT: u8 = 3;
const PRESS_FRAMES: u8 = 8;
const RELEASE_FRAMES: u8 = 8;
const SETTLE_FRAMES: u8 = 90;

fn core_path() -> Result<PathBuf, Box<dyn Error>> {
    Ok(PathBuf::from(std::env::var("HARMONY_GAMBATTE_CORE")?))
}

fn rom_bytes() -> Result<Vec<u8>, Box<dyn Error>> {
    Ok(fs::read(std::env::var("HARMONY_BLUE_ROM")?)?)
}

fn core_identity(path: &std::path::Path) -> Result<String, Box<dyn Error>> {
    Ok(format!("{:x}", Sha256::digest(fs::read(path)?)))
}

fn read_chords(path: &str) -> Result<Vec<ButtonChord>, Box<dyn Error>> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn read_actions(path: &str) -> Result<Vec<BlueAction>, Box<dyn Error>> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn new_target(prefix: &[ButtonChord]) -> Result<BlueTarget, Box<dyn Error>> {
    let core = core_path()?;
    let identity = core_identity(&core)?;
    Ok(BlueTarget::from_rom_bytes_after(
        &rom_bytes()?,
        &core,
        &identity,
        prefix,
    )?)
}

fn describe(target: &BlueTarget) -> String {
    let state = target.state();
    let alphabet = target.alphabet();
    format!(
        "map={} x={} y={} facing={} badges={:#04x} flags={:#04x} party={} hp={} levels={} \
         battle={} opponent={} menu={}/{} column={:#04x} events={} alphabet={} \
         joy={:#04x} flags5={:#04x} walk={} text={} lead={}/{}/{} moves={:?}",
        state.map,
        state.x,
        state.y,
        state.facing,
        state.badges,
        state.milestone_flags(),
        state.party_count,
        state.party_hp(),
        state.party_levels(),
        state.in_battle,
        state.opponent,
        state.menu_item,
        state.max_menu_item,
        state.menu_column,
        state.events_set,
        alphabet.size(),
        byte(target.work_ram(), blue_workload::target::JOY_IGNORE),
        byte(target.work_ram(), blue_workload::target::STATUS_FLAGS_5),
        byte(
            target.work_ram(),
            blue_workload::target::WALK_BIKE_SURF_STATE
        ),
        state.text_box,
        state.party[0].species,
        state.party[0].level,
        state.party[0].hp,
        (0..4)
            .map(|slot| byte(
                target.work_ram(),
                blue_workload::target::PARTY_MONS + 8 + slot
            ))
            .collect::<Vec<_>>(),
    )
}

fn in_the_bedroom(wram: &[u8]) -> bool {
    byte(wram, CUR_MAP) == REDS_HOUSE_2F
        && byte(wram, CUR_MAP_WIDTH) != 0
        && byte(wram, NUMBER_OF_WARPS) != 0
}

fn setup(output: &str) -> Result<(), Box<dyn Error>> {
    let core = core_path()?;
    let identity = core_identity(&core)?;
    let rom = rom_bytes()?;
    let mut machine = GambatteMachine::from_rom_bytes(&rom, &core, &identity)?;
    machine.set_wram_capture(false);
    let mut tape: Vec<ButtonChord> = Vec::new();
    let mut names_taken = 0;
    let mut cooldown = 0;
    for _ in 0..SETUP_CHORD_LIMIT {
        let wram = machine.read_wram()?;
        if in_the_bedroom(&wram) {
            break;
        }
        if tape.len().is_multiple_of(40) {
            println!(
                "chords={} map={} width={} warps={} menu={}/{}",
                tape.len(),
                byte(&wram, CUR_MAP),
                byte(&wram, CUR_MAP_WIDTH),
                byte(&wram, NUMBER_OF_WARPS),
                byte(&wram, CURRENT_MENU_ITEM),
                byte(&wram, MAX_MENU_ITEM),
            );
        }
        let naming = names_taken < NAME_MENUS
            && cooldown == 0
            && byte(&wram, MAX_MENU_ITEM) == NAME_MENU_ITEMS
            && byte(&wram, CURRENT_MENU_ITEM) == 0;
        let chords = if naming {
            names_taken += 1;
            cooldown = NAME_MENU_COOLDOWN;
            vec![
                ButtonChord::new(DOWN, PRESS_FRAMES),
                ButtonChord::new(0, RELEASE_FRAMES),
                ButtonChord::new(A, PRESS_FRAMES),
                ButtonChord::new(0, RELEASE_FRAMES),
            ]
        } else {
            cooldown = cooldown.saturating_sub(1);
            vec![
                ButtonChord::new(A, PRESS_FRAMES),
                ButtonChord::new(0, RELEASE_FRAMES),
            ]
        };
        machine.run_chords(&chords)?;
        tape.extend(chords);
    }
    let wram = machine.read_wram()?;
    if !in_the_bedroom(&wram) {
        return Err(format!(
            "power-on walk did not reach the bedroom: map={} chords={}",
            byte(&wram, CUR_MAP),
            tape.len()
        )
        .into());
    }
    let settle = ButtonChord::new(0, SETTLE_FRAMES);
    machine.run_chords(std::slice::from_ref(&settle))?;
    tape.push(settle);
    let wram = machine.read_wram()?;
    println!(
        "bedroom reached: chords={} x={} y={} names={}",
        tape.len(),
        byte(&wram, X_COORD),
        byte(&wram, Y_COORD),
        names_taken
    );
    fs::write(output, serde_json::to_vec_pretty(&tape)?)?;
    Ok(())
}

fn state(prefix: &str) -> Result<(), Box<dyn Error>> {
    let target = new_target(&read_chords(prefix)?)?;
    println!("{}", describe(&target));
    Ok(())
}

fn route(prefix: &str, actions: &str) -> Result<(), Box<dyn Error>> {
    let mut target = new_target(&read_chords(prefix)?)?;
    println!("start {}", describe(&target));
    for (index, action) in read_actions(actions)?.iter().enumerate() {
        target.apply(action);
        println!(
            "{index:4} {}[{}] {}",
            action.kind.name(),
            action.index,
            describe(&target)
        );
        if target.exit_kind() != searcher::target::ExitKind::Ok {
            return Err("the target failed".into());
        }
        if target.is_victory() {
            println!("badge at action {index}");
            break;
        }
    }
    Ok(())
}

fn alphabet(prefix: &str, actions: Option<&str>) -> Result<(), Box<dyn Error>> {
    let mut target = new_target(&read_chords(prefix)?)?;
    if let Some(actions) = actions {
        for action in read_actions(actions)? {
            target.apply(&action);
        }
    }
    let state = target.state();
    let alphabet = target.alphabet();
    println!("{}", describe(&target));
    if let Some(overworld) = Overworld::decode(target.work_ram(), &rom_bytes()?) {
        println!(
            "map is {} by {} steps, standing on a walkable tile: {}",
            overworld.width(),
            overworld.height(),
            overworld.walkable(state.x, state.y)
        );
    }
    for (index, destination) in alphabet.destinations.iter().enumerate() {
        println!(
            "{index:3} walk_to x={} y={} face={:?}",
            destination.x, destination.y, destination.face
        );
    }
    for action in alphabet.actions() {
        if action.kind != blue_workload::target::ActionKind::WalkTo {
            println!("     {}[{}]", action.kind.name(), action.index);
        }
    }
    Ok(())
}

fn dump_after(prefix: &str, actions: Option<&str>) -> Result<(), Box<dyn Error>> {
    use blue_workload::target::{
        CUR_MAP_HEIGHT, CUR_MAP_TILESET, CUR_MAP_WIDTH, NUMBER_OF_WARPS, OVERWORLD_MAP,
        TILESET_BANK, TILESET_BLOCKS_POINTER, TILESET_COLLISION_POINTER, word,
    };
    let mut target = new_target(&read_chords(prefix)?)?;
    if let Some(actions) = actions {
        for action in read_actions(actions)? {
            target.apply(&action);
        }
    }
    println!("{}", describe(&target));
    let wram = target.work_ram();
    println!(
        "tileset={} width={} height={} bank={} blocks={:#06x} collision={:#06x} warps={}",
        byte(wram, CUR_MAP_TILESET),
        byte(wram, CUR_MAP_WIDTH),
        byte(wram, CUR_MAP_HEIGHT),
        byte(wram, TILESET_BANK),
        word(wram, TILESET_BLOCKS_POINTER),
        word(wram, TILESET_COLLISION_POINTER),
        byte(wram, NUMBER_OF_WARPS),
    );
    println!(
        "standing={:#04x} front={:#04x} movement={:#04x} grass={:#04x}",
        byte(wram, 0xcf0e),
        byte(wram, 0xcfc6),
        byte(wram, 0xd736),
        byte(wram, 0xd535),
    );
    for row in 0..18_u16 {
        let line = (0..20_u16)
            .map(|column| format!("{:02x} ", byte(wram, 0xc3a0 + row * 20 + column)))
            .collect::<String>();
        println!("screen {row:2} {line}");
    }
    for index in 0..u16::from(byte(wram, NUMBER_OF_WARPS)) {
        let base =
            blue_workload::target::WARP_ENTRIES + blue_workload::target::WARP_ENTRY_BYTES * index;
        println!(
            "warp {index}: x={} y={} to map {} entry {}",
            byte(wram, base + 1),
            byte(wram, base),
            byte(wram, base + 3),
            byte(wram, base + 2),
        );
    }
    for index in 0..16_u16 {
        let base = blue_workload::target::SPRITE_STATE_DATA_2
            + blue_workload::target::SPRITE_STRUCT_BYTES * index;
        println!(
            "sprite {index}: y={} x={} picture={} movement={}",
            byte(wram, base + 4),
            byte(wram, base + 5),
            byte(wram, base + 13),
            byte(wram, base + 6),
        );
    }
    if let Some(overworld) = Overworld::decode(wram, &rom_bytes()?) {
        let state = target.state();
        for y in 0..overworld.height() {
            let row = (0..overworld.width())
                .map(|x| {
                    let (x8, y8) = (u8::try_from(x).unwrap_or(0), u8::try_from(y).unwrap_or(0));
                    if (x8, y8) == (state.x, state.y) {
                        '@'
                    } else if overworld.walkable(x8, y8) {
                        '.'
                    } else {
                        '#'
                    }
                })
                .collect::<String>();
            println!("{y:3} {row}");
        }
        println!("collision {:?}", overworld.collision());
        for y in 0..overworld.height() {
            let row = (0..overworld.width())
                .map(|x| {
                    format!(
                        "{:02x} ",
                        overworld.tile(u8::try_from(x).unwrap_or(0), u8::try_from(y).unwrap_or(0))
                    )
                })
                .collect::<String>();
            println!("{y:3} {row}");
        }
    }
    let base = usize::from(OVERWORLD_MAP - blue_workload::target::WRAM_BASE);
    let stride = usize::from(byte(wram, CUR_MAP_WIDTH)) + 6;
    for row in 0..usize::from(byte(wram, CUR_MAP_HEIGHT)) + 6 {
        println!(
            "{:?}",
            &wram[base + row * stride..base + (row + 1) * stride]
        );
    }
    Ok(())
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(tag = "goal", rename_all = "snake_case")]
enum Goal {
    Walk {
        x: u8,
        y: u8,
    },
    Warp {
        map: u8,
    },
    Interact,
    Leave {
        step: String,
    },
    Policy {
        #[serde(rename = "move")]
        attack: Option<u8>,
    },
    Talk {
        x: u8,
        y: u8,
    },
    Advance {
        times: usize,
    },
    Fight {
        #[serde(rename = "move")]
        attack: u8,
        times: usize,
    },
    Grind {
        from: [u8; 2],
        to: [u8; 2],
        #[serde(rename = "move")]
        attack: u8,
        level: u8,
        limit: usize,
    },
    Repeat {
        times: usize,
        level: u8,
        goals: Vec<Goal>,
    },
}

const WALK_ACTION_LIMIT: usize = 64;
const GRIND_HEALTH_DIVISOR: u32 = 3;
const AUTO_FIGHT_LIMIT: usize = 40;

struct Planner {
    target: BlueTarget,
    rom: Vec<u8>,
    actions: Vec<BlueAction>,
    auto_fight: Option<u8>,
    fighting: bool,
}

impl Planner {
    fn apply(&mut self, action: BlueAction) {
        self.target.apply(&action);
        self.actions.push(action);
        if std::env::var("BLUE_TRACE").is_ok() {
            println!(
                "  {:4} {}[{}] {}",
                self.actions.len(),
                action.kind.name(),
                action.index,
                describe(&self.target)
            );
        }
        self.clear_battle();
    }

    fn clear_battle(&mut self) {
        let Some(attack) = self.auto_fight else {
            return;
        };
        if self.fighting || !self.target.state().in_battle() {
            return;
        }
        self.fighting = true;
        for _ in 0..AUTO_FIGHT_LIMIT {
            if !self.target.state().in_battle() {
                break;
            }
            self.apply(BlueAction::new(
                blue_workload::target::ActionKind::BattleMove,
                attack,
            ));
        }
        for _ in 0..2 {
            self.apply(BlueAction::new(
                blue_workload::target::ActionKind::Advance,
                0,
            ));
        }
        self.fighting = false;
    }

    fn here(&self) -> (u8, u8) {
        let state = self.target.state();
        (state.x, state.y)
    }

    fn walk(&mut self, goal: (u8, u8)) -> Result<(), Box<dyn Error>> {
        let map = self.target.state().map;
        let mut stalls = 0;
        for _ in 0..WALK_ACTION_LIMIT {
            let state = self.target.state();
            if state.in_battle() || state.map != map || (state.x, state.y) == goal {
                return Ok(());
            }
            let Some(overworld) = Overworld::decode(self.target.work_ram(), &self.rom) else {
                return Err("the current map does not decode".into());
            };
            let here = self.here();
            let Some(route) = overworld.route(here, goal) else {
                return Err(format!("no route from {here:?} to {goal:?}").into());
            };
            let alphabet = self.target.alphabet();
            let mut along = here;
            let mut best = None;
            for step in route {
                let Some(next) = overworld.neighbour(along.0, along.1, step) else {
                    break;
                };
                along = next;
                if let Some(index) = alphabet.destinations.iter().position(|destination| {
                    (destination.x, destination.y) == along
                        && (destination.exit.is_none() || along == goal)
                }) {
                    best = Some(index);
                }
            }
            let Some(index) = best else {
                return Err(format!("no walk action advances from {here:?} to {goal:?}").into());
            };
            self.apply(BlueAction::new(
                blue_workload::target::ActionKind::WalkTo,
                u8::try_from(index)?,
            ));
            if self.here() == here && !self.target.state().in_battle() {
                stalls += 1;
                if stalls > WALK_STALL_LIMIT {
                    return Err(
                        format!("the walk from {here:?} to {goal:?} made no progress").into(),
                    );
                }
                self.apply(BlueAction::new(
                    blue_workload::target::ActionKind::Advance,
                    0,
                ));
            } else {
                stalls = 0;
            }
        }
        Err("the walk ran out of actions".into())
    }

    fn warp(&mut self, map: u8) -> Result<(), Box<dyn Error>> {
        use blue_workload::target::{WARP_ENTRIES, WARP_ENTRY_BYTES};
        let state = self.target.state();
        let overworld = Overworld::decode(self.target.work_ram(), &self.rom)
            .ok_or("the current map does not decode")?;
        let wram = self.target.work_ram();
        let mut doors = (0..u16::from(state.warps))
            .map(|index| WARP_ENTRIES + WARP_ENTRY_BYTES * index)
            .filter(|base| byte(wram, base + 3) == map)
            .map(|base| (byte(wram, base + 1), byte(wram, base)))
            .collect::<Vec<_>>();
        doors.sort_by_key(|door| !overworld.walkable(door.0, door.1));
        let goal = *doors
            .first()
            .ok_or_else(|| format!("map {} has no warp to map {map}", state.map))?;
        let from = state.map;
        self.walk(goal)?;
        if self.target.state().map == from {
            self.apply(BlueAction::new(
                blue_workload::target::ActionKind::WalkTo,
                u8::try_from(
                    self.target
                        .alphabet()
                        .destinations
                        .iter()
                        .position(|destination| (destination.x, destination.y) == goal)
                        .ok_or("the warp tile left the alphabet")?,
                )?,
            ));
        }
        Ok(())
    }

    fn talk(&mut self, anchor: (u8, u8)) -> Result<(), Box<dyn Error>> {
        let approaches = self
            .target
            .alphabet()
            .destinations
            .iter()
            .enumerate()
            .filter(|(_, destination)| destination.face == Some(anchor))
            .map(|(index, destination)| (index, (destination.x, destination.y)))
            .collect::<Vec<_>>();
        if approaches.is_empty() {
            return Err(format!("nothing to face at {anchor:?}").into());
        }
        for (index, tile) in approaches {
            self.apply(BlueAction::new(
                blue_workload::target::ActionKind::WalkTo,
                u8::try_from(index)?,
            ));
            if self.here() == tile {
                self.apply(BlueAction::new(
                    blue_workload::target::ActionKind::Interact,
                    0,
                ));
                return Ok(());
            }
        }
        Err(format!("every approach to {anchor:?} was blocked").into())
    }

    fn leave(&mut self, step: &str) -> Result<(), Box<dyn Error>> {
        let wanted = match step {
            "up" => blue_workload::map::STEP_UP,
            "down" => blue_workload::map::STEP_DOWN,
            "left" => blue_workload::map::STEP_LEFT,
            "right" => blue_workload::map::STEP_RIGHT,
            other => return Err(format!("unknown edge {other}").into()),
        };
        let map = self.target.state().map;
        for _ in 0..WALK_ACTION_LIMIT {
            let index = self
                .target
                .alphabet()
                .destinations
                .iter()
                .position(|destination| destination.exit == Some(wanted))
                .ok_or_else(|| format!("map {map} has no reachable {step} edge"))?;
            self.apply(BlueAction::new(
                blue_workload::target::ActionKind::WalkTo,
                u8::try_from(index)?,
            ));
            if self.target.state().map != map || self.target.state().in_battle() {
                return Ok(());
            }
        }
        Err(format!("leaving map {map} to the {step} made no progress").into())
    }

    fn fight(&mut self, attack: u8, times: usize) {
        for _ in 0..times {
            if !self.target.state().in_battle() {
                break;
            }
            self.apply(BlueAction::new(
                blue_workload::target::ActionKind::BattleMove,
                attack,
            ));
        }
    }

    fn party_max_hp(&self) -> u32 {
        let state = self.target.state();
        state.party[..usize::from(state.party_count).min(state.party.len())]
            .iter()
            .map(|member| u32::from(member.max_hp))
            .sum()
    }

    fn grind(
        &mut self,
        from: (u8, u8),
        to: (u8, u8),
        attack: u8,
        level: u8,
        limit: usize,
    ) -> Result<(), Box<dyn Error>> {
        let mut corner = false;
        let mut rounds = 0;
        let start = self.actions.len();
        while self.actions.len() - start < limit && rounds < limit {
            rounds += 1;
            let state = self.target.state();
            if state.party[0].level >= level {
                return Ok(());
            }
            if !state.in_battle() && state.party_hp() * GRIND_HEALTH_DIVISOR <= self.party_max_hp()
            {
                return Ok(());
            }
            if state.in_battle() {
                self.fight(attack, 24);
                for _ in 0..4 {
                    if !self.target.state().in_battle() {
                        break;
                    }
                    self.apply(BlueAction::new(
                        blue_workload::target::ActionKind::Advance,
                        0,
                    ));
                }
                continue;
            }
            let goal = if corner { from } else { to };
            corner = !corner;
            if let Err(error) = self.walk(goal) {
                println!("grind walk gave up: {error}");
            }
        }
        Ok(())
    }

    fn run(&mut self, goals: Vec<Goal>) -> Result<(), Box<dyn Error>> {
        for (index, goal) in goals.into_iter().enumerate() {
            match goal {
                Goal::Walk { x, y } => self.walk((x, y))?,
                Goal::Warp { map } => self.warp(map)?,
                Goal::Interact => self.apply(BlueAction::new(
                    blue_workload::target::ActionKind::Interact,
                    0,
                )),
                Goal::Talk { x, y } => self.talk((x, y))?,
                Goal::Leave { step } => self.leave(&step)?,
                Goal::Policy { attack } => self.auto_fight = attack,
                Goal::Advance { times } => {
                    for _ in 0..times {
                        self.apply(BlueAction::new(
                            blue_workload::target::ActionKind::Advance,
                            0,
                        ));
                    }
                }
                Goal::Fight { attack, times } => self.fight(attack, times),
                Goal::Grind {
                    from,
                    to,
                    attack,
                    level,
                    limit,
                } => self.grind((from[0], from[1]), (to[0], to[1]), attack, level, limit)?,
                Goal::Repeat {
                    times,
                    level,
                    goals,
                } => {
                    for _ in 0..times {
                        if self.target.state().party[0].level >= level {
                            break;
                        }
                        if let Err(error) = self.run(goals.clone()) {
                            println!("repeat gave up: {error}");
                        }
                    }
                }
            }
            println!("goal {index:3} -> {}", describe(&self.target));
            if self.target.exit_kind() != searcher::target::ExitKind::Ok {
                return Err("the target failed".into());
            }
        }
        Ok(())
    }
}

fn plan(prefix: &str, itinerary: &str, output: &str) -> Result<(), Box<dyn Error>> {
    let goals: Vec<Goal> = serde_json::from_slice(&fs::read(itinerary)?)?;
    let mut planner = Planner {
        target: new_target(&read_chords(prefix)?)?,
        rom: rom_bytes()?,
        actions: Vec::new(),
        auto_fight: None,
        fighting: false,
    };
    println!("start {}", describe(&planner.target));
    let outcome = planner.run(goals);
    fs::write(output, serde_json::to_vec_pretty(&planner.actions)?)?;
    println!(
        "wrote {} actions; badge={}",
        planner.actions.len(),
        planner.target.is_victory()
    );
    outcome
}

fn shot(prefix: &str, actions: &str, output: &str) -> Result<(), Box<dyn Error>> {
    let core = core_path()?;
    let identity = core_identity(&core)?;
    let mut target = BlueTarget::from_rom_bytes_capturing(
        &rom_bytes()?,
        &core,
        &identity,
        &read_chords(prefix)?,
    )?;
    let mut last = None;
    for action in read_actions(actions)? {
        target.apply(&action);
        last = target.drain_frames().pop().or(last);
        target.drain_audio();
    }
    let frame = last
        .or_else(|| target.drain_frames().pop())
        .ok_or("the core produced no video frame")?;
    let mut ppm = format!("P6\n{} {}\n255\n", frame.width, frame.height).into_bytes();
    ppm.extend_from_slice(&frame.rgb24);
    fs::write(output, ppm)?;
    println!("{}", describe(&target));
    println!("wrote {output} ({}x{})", frame.width, frame.height);
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    match arguments.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["setup", output] => setup(output),
        ["state", prefix] => state(prefix),
        ["dump", prefix] => dump_after(prefix, None),
        ["dump", prefix, actions] => dump_after(prefix, Some(actions)),
        ["plan", prefix, itinerary, output] => plan(prefix, itinerary, output),
        ["route", prefix, actions] => route(prefix, actions),
        ["shot", prefix, actions, output] => shot(prefix, actions, output),
        ["alphabet", prefix] => alphabet(prefix, None),
        ["alphabet", prefix, actions] => alphabet(prefix, Some(actions)),
        _ => Err("usage: blue-probe setup|state|dump|plan|route|alphabet|shot ...".into()),
    }
}
