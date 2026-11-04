// SPDX-License-Identifier: AGPL-3.0-or-later

mod boot;
mod game;
mod worker;

use std::{
    collections::BTreeMap,
    fs,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use engine::{
    archive::Archive,
    campaign::{Campaign, Config, Outcome, Stop, Stretch},
    report,
    target::{Action, Worker},
};
use machine::nes::{ButtonChord, core_state};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    game::{CART_LEN, Game, Predicate},
    worker::{Milestone, NesWorker},
};

const BASE_STEPS: u64 = 128;

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[derive(Deserialize)]
struct Host {
    core: PathBuf,
    roms: BTreeMap<String, PathBuf>,
}

impl Host {
    fn load(path: &Path) -> Result<Self, String> {
        let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| e.to_string())
    }

    fn core_sha256(&self) -> Result<String, String> {
        Ok(sha256_hex(
            &fs::read(&self.core).map_err(|e| e.to_string())?,
        ))
    }
}

#[derive(Deserialize)]
struct Start {
    tape: PathBuf,
    tape_sha256: String,
    core_state_sha256: String,
}

#[derive(Deserialize)]
struct Request {
    rom_sha256: String,
    cartridge_ram: bool,
    start: Start,
    milestone: Vec<Predicate>,
    work_budget: u64,
    workers: usize,
    memory_limit: u64,
    seed: u64,
}

#[derive(Serialize)]
struct BenchResult {
    stop: String,
    work: u64,
    job_stream_sha256: String,
    message: String,
}

#[derive(Serialize, Deserialize)]
struct Route {
    game: String,
    rom_sha256: String,
    start_tape_sha256: Option<String>,
    actions: Vec<[u64; 3]>,
    buttons: Vec<[u8; 2]>,
    #[serde(default)]
    stretches: Vec<Stretch>,
}

fn tape_chords(tape: &[u8]) -> Vec<ButtonChord> {
    let mut out: Vec<ButtonChord> = Vec::new();
    for &b in tape {
        match out.last_mut() {
            Some(c) if c.buttons == b && c.hold_frames < 120 => c.hold_frames += 1,
            _ => out.push(ButtonChord::new(b, 1)),
        }
    }
    out
}

fn play(worker: &mut NesWorker, start: &[u8], chords: &[ButtonChord]) -> Result<Vec<u8>, String> {
    let frames: u64 = chords.iter().map(|c| u64::from(c.hold_frames)).sum();
    if frames == 0 {
        return Ok(start.to_vec());
    }
    worker.stage(start, chords)?;
    for _ in 0..frames {
        worker.frame()?;
    }
    worker.core_state()
}

fn trace(
    worker: &mut NesWorker,
    game: &Game,
    start: &[u8],
    chords: &[ButtonChord],
    path: &std::path::Path,
) -> Result<Vec<u8>, String> {
    let frames: u64 = chords.iter().map(|c| u64::from(c.hold_frames)).sum();
    let mut out = std::io::BufWriter::new(fs::File::create(path).map_err(|e| e.to_string())?);
    if frames > 0 {
        worker.stage(start, chords)?;
    }
    let mut scalars = Vec::new();
    for f in 0..frames {
        worker.frame()?;
        let (wram, cart) = worker.ram();
        game.scalars_into(wram, cart, &mut scalars);
        writeln!(
            out,
            "{}",
            serde_json::json!({"frame": f + 1, "scalars": scalars})
        )
        .map_err(|e| e.to_string())?;
    }
    out.flush().map_err(|e| e.to_string())?;
    if frames == 0 {
        Ok(start.to_vec())
    } else {
        worker.core_state()
    }
}

fn start_ram(worker: &NesWorker) -> Vec<u8> {
    let (wram, cart) = worker.ram();
    let mut out = wram.to_vec();
    out.resize(0x800, 0);
    out.extend_from_slice(cart);
    out
}

fn resident_bytes() -> Option<u64> {
    let Ok(status) = fs::read_to_string("/proc/self/status") else {
        let out = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
            .ok()?;
        let kib: u64 = String::from_utf8(out.stdout).ok()?.trim().parse().ok()?;
        return Some(kib * 1024);
    };
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

struct Setup {
    game: Arc<Game>,
    workers: Vec<Box<dyn Worker>>,
    root: Vec<u8>,
    root_scalars: Vec<u64>,
}

fn setup(
    host: &Host,
    game: Game,
    rom: &[u8],
    tape: &[u8],
    expect_state: Option<&str>,
    predicates: Vec<Predicate>,
    workers: usize,
) -> Result<Setup, (String, String)> {
    let core_sha = host.core_sha256().map_err(|e| ("error".into(), e))?;
    let game = Arc::new(game);
    let placeholder = Arc::new(Milestone {
        predicates: Vec::new(),
        start: Vec::new(),
    });
    let mut first = NesWorker::new(game.clone(), placeholder, rom, &host.core, &core_sha)
        .map_err(|e| ("error".to_string(), e))?;
    let power = first.power_on().map_err(|e| ("error".to_string(), e))?;
    let root =
        play(&mut first, &power, &tape_chords(tape)).map_err(|e| ("error".to_string(), e))?;
    if let Some(want) = expect_state {
        let got = sha256_hex(core_state(&root));
        if got != want {
            return Err((
                "start_mismatch".into(),
                format!("core state {got}, spec {want}"),
            ));
        }
    }
    let ram = if tape.is_empty() {
        let mut w = first
            .machine
            .read_wram()
            .map_err(|e| ("error".into(), format!("{e:?}")))?
            .to_vec();
        w.resize(0x800, 0);
        w
    } else {
        start_ram(&first)
    };
    let milestone = Arc::new(Milestone {
        predicates,
        start: ram,
    });
    drop(first);
    let mut out: Vec<Box<dyn Worker>> = Vec::new();
    for _ in 0..workers.max(1) {
        out.push(Box::new(
            NesWorker::new(game.clone(), milestone.clone(), rom, &host.core, &core_sha)
                .map_err(|e| ("error".to_string(), e))?,
        ));
    }
    let obs = out[0]
        .observe_start(&root)
        .map_err(|e| ("error".to_string(), e))?;
    Ok(Setup {
        game,
        workers: out,
        root,
        root_scalars: obs.scalars,
    })
}

fn route_of(c: &Campaign, game: &Game, state: u32, tape_sha: Option<String>) -> Route {
    let actions: Vec<Action> = c.route(state);
    Route {
        game: game.name.clone(),
        rom_sha256: game.rom_sha256.clone(),
        start_tape_sha256: tape_sha,
        actions: actions
            .iter()
            .map(|a| [u64::from(a.input), a.arg, u64::from(a.hold)])
            .collect(),
        buttons: actions
            .iter()
            .map(|a| [game.buttons(a.input), a.hold.min(120) as u8])
            .collect(),
        stretches: c.stretches(state),
    }
}

fn stop_name(stop: &Stop) -> (String, String) {
    match stop {
        Stop::Milestone => ("milestone".into(), String::new()),
        Stop::Budget => ("budget".into(), String::new()),
        Stop::Memory => ("memory".into(), String::new()),
        Stop::Error(e) => ("error".into(), e.clone()),
    }
}

fn memory_guard(limit: u64) -> impl FnMut(&Archive) -> bool {
    move |_| resident_bytes().is_none_or(|r| r <= limit)
}

fn bench(args: &[String]) -> Result<(), String> {
    let mut host = None;
    let mut rest = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--host" {
            host = it.next().cloned();
        } else {
            rest.push(a.clone());
        }
    }
    let [req_path, res_path] = rest.as_slice() else {
        return Err("usage: v4-nes bench --host HOST REQUEST RESULT".into());
    };
    let write = |r: BenchResult| -> Result<(), String> {
        fs::write(
            res_path,
            serde_json::to_string(&r).map_err(|e| e.to_string())? + "\n",
        )
        .map_err(|e| e.to_string())
    };
    let fail = |stop: &str, message: String| BenchResult {
        stop: stop.into(),
        work: 0,
        job_stream_sha256: String::new(),
        message,
    };
    let host = Host::load(Path::new(&host.ok_or("--host is required")?))?;
    let req: Request =
        serde_json::from_str(&fs::read_to_string(req_path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let game = match game::by_rom(&req.rom_sha256) {
        Ok(g) => g,
        Err(e) => return write(fail("error", e)),
    };
    if game.cartridge_ram != req.cartridge_ram {
        return write(fail(
            "error",
            "cartridge_ram differs from the game config".into(),
        ));
    }
    for p in &req.milestone {
        if let Err(e) = p.validate(req.cartridge_ram) {
            return write(fail("error", e));
        }
    }
    let Some(rom_path) = host.roms.get(&req.rom_sha256) else {
        return write(fail(
            "rom_mismatch",
            "host has no ROM with that hash".into(),
        ));
    };
    let rom = fs::read(rom_path).map_err(|e| e.to_string())?;
    if sha256_hex(&rom) != req.rom_sha256 {
        return write(fail(
            "rom_mismatch",
            format!("{} has another hash", rom_path.display()),
        ));
    }
    let tape = fs::read(&req.start.tape).map_err(|e| e.to_string())?;
    if sha256_hex(&tape) != req.start.tape_sha256 {
        return write(fail("start_mismatch", "tape hash differs".into()));
    }
    let mut s = match setup(
        &host,
        game,
        &rom,
        &tape,
        Some(&req.start.core_state_sha256),
        req.milestone.clone(),
        req.workers,
    ) {
        Ok(s) => s,
        Err((stop, m)) => return write(fail(&stop, m)),
    };
    let mut campaign = Campaign::new(
        s.game.kinds(),
        s.game.vocabulary(BASE_STEPS),
        Config::new(req.seed, req.work_budget),
        s.root,
        s.root_scalars,
    );
    let out_dir = Path::new(res_path)
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();
    let mut log = BufWriter::new(
        fs::File::create(out_dir.join("v4-progress.jsonl")).map_err(|e| e.to_string())?,
    );
    let outcome = campaign.run(
        &mut s.workers,
        &mut memory_guard(req.memory_limit),
        &mut |c| {
            if c.batches.is_multiple_of(64) {
                let _ = writeln!(log, "{}", report::summary_line(c));
            }
        },
    );
    let _ = writeln!(log, "{}", report::summary_line(&campaign));
    drop(log);
    if let Some(state) = outcome.milestone_state {
        let route = route_of(
            &campaign,
            &s.game,
            state,
            Some(req.start.tape_sha256.clone()),
        );
        fs::write(
            out_dir.join("v4-route.json"),
            serde_json::to_string(&route).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }
    let (stop, message) = stop_name(&outcome.stop);
    write(BenchResult {
        stop,
        work: outcome.work,
        job_stream_sha256: outcome.job_stream_sha256,
        message,
    })
}

struct Opts {
    host: PathBuf,
    game: String,
    seed: u64,
    workers: usize,
    budget: u64,
    memory_limit: u64,
    out: PathBuf,
    root_only: bool,
    tape: Option<PathBuf>,
    route: Option<PathBuf>,
    trace: Option<PathBuf>,
    level: u8,
}

fn opts(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        host: PathBuf::new(),
        game: String::new(),
        seed: 1,
        workers: 1,
        budget: u64::MAX,
        memory_limit: u64::MAX,
        out: PathBuf::from("."),
        root_only: false,
        tape: None,
        route: None,
        trace: None,
        level: 0,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut v = || it.next().cloned().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--host" => o.host = v()?.into(),
            "--game" => o.game = v()?,
            "--seed" => o.seed = v()?.parse().map_err(|e| format!("{e}"))?,
            "--workers" => o.workers = v()?.parse().map_err(|e| format!("{e}"))?,
            "--budget" => o.budget = v()?.parse().map_err(|e| format!("{e}"))?,
            "--memory-limit" => o.memory_limit = v()?.parse().map_err(|e| format!("{e}"))?,
            "--out" => o.out = v()?.into(),
            "--tape" => o.tape = Some(v()?.into()),
            "--route" => o.route = Some(v()?.into()),
            "--trace" => o.trace = Some(v()?.into()),
            "--root-only" => o.root_only = true,
            "--level" => o.level = v()?.parse().map_err(|e| format!("{e}"))?,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(o)
}

fn rom_for(host: &Host, game: &Game) -> Result<Vec<u8>, String> {
    let path = host
        .roms
        .get(&game.rom_sha256)
        .ok_or("host has no ROM for this game")?;
    let rom = fs::read(path).map_err(|e| e.to_string())?;
    if sha256_hex(&rom) != game.rom_sha256 {
        return Err(format!("{} has another hash", path.display()));
    }
    Ok(rom)
}

fn scalar_extremes(archive: &Archive) -> Vec<[u64; 2]> {
    let n = archive.kinds.len();
    let mut out = vec![[u64::MAX, 0]; n];
    for cell in &archive.cells {
        for &s in &cell.front {
            for (i, v) in archive.states[s as usize].scalars.iter().enumerate() {
                out[i][0] = out[i][0].min(*v);
                out[i][1] = out[i][1].max(*v);
            }
        }
    }
    out
}

fn boot_tape(host: &Host, game: &Game, rom: &[u8], level: u8) -> Result<Vec<u8>, String> {
    let core_sha = host.core_sha256()?;
    let milestone = Arc::new(Milestone {
        predicates: Vec::new(),
        start: Vec::new(),
    });
    let mut w = NesWorker::new(
        Arc::new(game.clone()),
        milestone,
        rom,
        &host.core,
        &core_sha,
    )?;
    let power = w.power_on()?;
    let mut chords = boot::tape(&game.name, level)?;
    let frames: u64 = chords.iter().map(|c| u64::from(c.hold_frames)).sum();
    chords.push(ButtonChord::new(0, 120));
    let mut bytes: Vec<u8> = Vec::new();
    w.stage(&power, &chords)?;
    for c in &chords {
        bytes.extend(std::iter::repeat_n(c.buttons, usize::from(c.hold_frames)));
    }
    for f in 0..frames + boot::SETTLE_FRAMES {
        if f % 120 == 0 && f >= frames {
            let state = w.core_state()?;
            w.stage(&state, &[ButtonChord::new(0, 120)])?;
        }
        w.frame()?;
        if f + 1 >= frames && boot::playing(&game.name, level, w.ram().0) {
            bytes.resize((f + 1) as usize, 0);
            return Ok(bytes);
        }
    }
    Err(format!(
        "{} never reached play after its boot tape",
        game.name
    ))
}

fn power_on(args: &[String]) -> Result<(), String> {
    let o = opts(args)?;
    let host = Host::load(&o.host)?;
    let game = game::by_name(&o.game)?;
    let rom = rom_for(&host, &game)?;
    fs::create_dir_all(&o.out).map_err(|e| e.to_string())?;
    let tape = match &o.tape {
        Some(p) => fs::read(p).map_err(|e| e.to_string())?,
        None => {
            let t = boot_tape(&host, &game, &rom, o.level)?;
            fs::write(o.out.join("boot.tape"), &t).map_err(|e| e.to_string())?;
            t
        }
    };
    let tape_sha = Some(sha256_hex(&tape));
    let goal = game.goal.clone();
    let mut s = setup(&host, game, &rom, &tape, None, goal, o.workers)
        .map_err(|(a, b)| format!("{a}: {b}"))?;
    fs::create_dir_all(&o.out).map_err(|e| e.to_string())?;
    let mut config = Config::new(o.seed, o.budget);
    config.root_only = o.root_only;
    let mut campaign = Campaign::new(
        s.game.kinds(),
        s.game.vocabulary(BASE_STEPS),
        config,
        s.root,
        s.root_scalars,
    );
    let mut log =
        BufWriter::new(fs::File::create(o.out.join("progress.jsonl")).map_err(|e| e.to_string())?);
    let outcome: Outcome = campaign.run(
        &mut s.workers,
        &mut memory_guard(o.memory_limit),
        &mut |c| {
            if c.batches.is_multiple_of(256) {
                let _ = writeln!(
                    log,
                    "{{\"summary\":{},\"extremes\":{},\"rss\":{}}}",
                    report::summary_line(c),
                    serde_json::to_string(&scalar_extremes(&c.archive)).unwrap_or_default(),
                    resident_bytes().unwrap_or(0)
                );
                let _ = log.flush();
            }
        },
    );
    let _ = writeln!(log, "{}", report::summary_line(&campaign));
    drop(log);
    let mut cells =
        BufWriter::new(fs::File::create(o.out.join("cells.jsonl")).map_err(|e| e.to_string())?);
    report::write_cells(&campaign.archive, &mut cells).map_err(|e| e.to_string())?;
    let mut edges: Vec<_> = campaign
        .archive
        .edges
        .iter()
        .map(|((a, b), n)| [*a, *b, *n as u32])
        .collect();
    edges.sort_unstable();
    fs::write(
        o.out.join("edges.json"),
        serde_json::to_string(&edges).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if let Some(state) = outcome.milestone_state {
        let route = route_of(&campaign, &s.game, state, tape_sha);
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({ "route_stretches": &route.stretches }))
                .map_err(|e| e.to_string())?
        );
        fs::write(
            o.out.join("route.json"),
            serde_json::to_string(&route).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }
    fs::write(
        o.out.join("result.json"),
        serde_json::to_string(&outcome).map_err(|e| e.to_string())? + "\n",
    )
    .map_err(|e| e.to_string())?;
    println!(
        "{}",
        serde_json::to_string(&outcome).map_err(|e| e.to_string())?
    );
    Ok(())
}

fn replay(args: &[String]) -> Result<(), String> {
    let o = opts(args)?;
    let host = Host::load(&o.host)?;
    let route: Route = serde_json::from_str(
        &fs::read_to_string(o.route.as_ref().ok_or("--route is required")?)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let game = game::by_name(&route.game)?;
    let rom = rom_for(&host, &game)?;
    let core_sha = host.core_sha256()?;
    let game = Arc::new(game);
    let tape = match &o.tape {
        Some(p) => fs::read(p).map_err(|e| e.to_string())?,
        None => Vec::new(),
    };
    if route.start_tape_sha256.as_deref() != o.tape.as_ref().map(|_| sha256_hex(&tape)).as_deref() {
        return Err("the route's start tape differs from --tape".into());
    }
    let milestone = Arc::new(Milestone {
        predicates: Vec::new(),
        start: Vec::new(),
    });
    let mut w = NesWorker::new(game.clone(), milestone, &rom, &host.core, &core_sha)?;
    let power = w.power_on()?;
    let mut chords = tape_chords(&tape);
    let start = play(&mut w, &power, &chords)?;
    let steps: Vec<ButtonChord> = route
        .buttons
        .iter()
        .map(|b| ButtonChord::new(b[0], b[1]))
        .collect();
    let end = match &o.trace {
        Some(path) => trace(&mut w, &game, &start, &steps, path)?,
        None => play(&mut w, &start, &steps)?,
    };
    chords.extend(steps);
    let (wram, cart) = w.ram();
    let goal_holds = !game.goal.is_empty() && game.goal.iter().all(|p| p.holds(wram, cart, 0));
    let mut scalars = Vec::new();
    game.scalars_into(wram, cart, &mut scalars);
    let frames: u64 = chords.iter().map(|c| u64::from(c.hold_frames)).sum();
    println!(
        "{}",
        serde_json::json!({
            "frames": frames,
            "goal_holds": goal_holds,
            "scalars": scalars,
            "core_state_sha256": sha256_hex(core_state(&end)),
            "cart_len": cart.len().min(CART_LEN),
        })
    );
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("bench") => bench(&args[1..]),
        Some("run") => power_on(&args[1..]),
        Some("replay") => replay(&args[1..]),
        _ => Err("usage: v4-nes bench|run|replay ...".into()),
    };
    if let Err(e) = result {
        eprintln!("v4-nes: {e}");
        std::process::exit(2);
    }
}
