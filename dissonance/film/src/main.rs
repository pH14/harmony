// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `film` driver — a thin, scriptable shell over the crate.
//!
//! Three subcommands, matching the pass split (task 87 §2):
//!
//! - `plan`   — derive a [`FilmPlan`] from a frame-clock trace + a billboard
//!   window and emit it as JSON (the query as a replayable artifact),
//! - `render` — render a captured [`CaptureBundle`] JSON to a PPM sequence + a
//!   contact sheet, printing the committed blake3 hashes (the host-side render
//!   pass; this is where `CoreReplay` runs on the box),
//! - `demo`   — run the whole pipeline end-to-end against the in-crate mock
//!   server with the deterministic stamp renderer (no core, no ROM) — the
//!   laptop-visible proof of the plan → project → render → output loop.
//!
//! Video encoding stays outside the repo: see the crate README for the ffmpeg
//! one-liner over the PPM sequence.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use film::{
    BillboardScenario, BillboardWindow, CaptureBundle, ClipSelect, FilmPlan, Frame, FrameRenderer,
    FrameTick, Session, StampRenderer, blake3_hex, contact_sheet, film, write_ppm,
};

/// The `film` driver CLI.
#[derive(Parser)]
#[command(
    name = "film",
    about = "The visible replay: plan, project, and render a reproducer clip"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Derive a FilmPlan from a frame-clock trace and emit it as JSON.
    Plan {
        /// JSON file: an array of `{ "frame": u32, "moment": u64 }` ticks.
        #[arg(long)]
        trace: PathBuf,
        /// Billboard buffer guest-physical base (decimal or `0x`-hex).
        #[arg(long, value_parser = parse_u64)]
        gpa: u64,
        /// Billboard buffer total length in bytes.
        #[arg(long)]
        len: u32,
        /// Clip by inclusive frame range `FIRST..=LAST` (mutually exclusive with
        /// `--clip-moments`).
        #[arg(long, num_args = 2, value_names = ["FIRST", "LAST"])]
        clip_frames: Option<Vec<u32>>,
        /// Clip by inclusive moment span `START..=END`.
        #[arg(long, num_args = 2, value_names = ["START", "END"])]
        clip_moments: Option<Vec<u64>>,
        /// Keep every Nth selected frame (contact-sheet density).
        #[arg(long)]
        stride: Option<u32>,
        /// Per-read cap (task-80 length cap); defaults to the client cap.
        #[arg(long, default_value_t = resolution_read_cap())]
        read_cap: u32,
        /// Output plan JSON path.
        #[arg(long, short)]
        out: PathBuf,
    },
    /// Render a captured bundle JSON to PPM frames + a contact sheet.
    Render {
        /// CaptureBundle JSON (produced by the capture pass).
        #[arg(long)]
        bundle: PathBuf,
        /// Directory to write `frame-NNNN.ppm` + `contact.ppm` into.
        #[arg(long)]
        out_dir: PathBuf,
        /// Contact-sheet columns.
        #[arg(long, default_value_t = 8)]
        contact_cols: u32,
        /// Use the real libretro core renderer (needs the `core-replay` build +
        /// `HARMONY_SMB_CORE`/`HARMONY_SMB_ROM`); otherwise the stamp renderer.
        #[arg(long)]
        core_replay: bool,
    },
    /// Run the full pipeline against the in-crate mock server (no core/ROM).
    Demo {
        /// Number of frames to film.
        #[arg(long, default_value_t = 8)]
        frames: u32,
        /// Directory to write outputs into.
        #[arg(long)]
        out_dir: PathBuf,
        /// Contact-sheet columns.
        #[arg(long, default_value_t = 4)]
        contact_cols: u32,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("film: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), String> {
    match cli.command {
        Command::Plan {
            trace,
            gpa,
            len,
            clip_frames,
            clip_moments,
            stride,
            read_cap,
            out,
        } => {
            let clip = match (clip_frames, clip_moments) {
                (Some(_), Some(_)) => {
                    return Err("pass at most one of --clip-frames / --clip-moments".into());
                }
                (Some(f), None) => ClipSelect::FrameRange {
                    first: f[0],
                    last: f[1],
                },
                (None, Some(m)) => ClipSelect::MomentSpan {
                    start: m[0],
                    end: m[1],
                },
                (None, None) => ClipSelect::All,
            };
            cmd_plan(
                &trace,
                BillboardWindow { gpa, len },
                clip,
                stride,
                read_cap,
                &out,
            )
        }
        Command::Render {
            bundle,
            out_dir,
            contact_cols,
            core_replay,
        } => cmd_render(&bundle, &out_dir, contact_cols, core_replay),
        Command::Demo {
            frames,
            out_dir,
            contact_cols,
        } => cmd_demo(frames, &out_dir, contact_cols),
    }
}

fn cmd_plan(
    trace: &Path,
    window: BillboardWindow,
    clip: ClipSelect,
    stride: Option<u32>,
    read_cap: u32,
    out: &Path,
) -> Result<(), String> {
    let ticks: Vec<FrameTick> = read_json(trace)?;
    let plan = FilmPlan::derive(&ticks, window, clip, stride, read_cap)
        .map_err(|e| format!("plan derivation failed: {e}"))?;
    write_json(out, &plan)?;
    println!(
        "wrote plan with {} frame(s), {} read chunk(s) → {}",
        plan.frames.len(),
        plan.read_chunks().len(),
        out.display()
    );
    Ok(())
}

fn cmd_render(
    bundle_path: &Path,
    out_dir: &Path,
    contact_cols: u32,
    core_replay: bool,
) -> Result<(), String> {
    let bundle: CaptureBundle = read_json(bundle_path)?;
    if bundle.is_empty() {
        return Err("capture bundle has no frames".into());
    }
    let mut renderer = pick_renderer(core_replay)?;
    render_bundle(&bundle, renderer.as_mut(), out_dir, contact_cols)
}

fn cmd_demo(frames: u32, out_dir: &Path, contact_cols: u32) -> Result<(), String> {
    if frames == 0 {
        return Err("--frames must be at least 1".into());
    }
    // A synthetic frame clock + billboard scenario, filmed over the mock server.
    let ticks: Vec<FrameTick> = (0..frames)
        .map(|i| FrameTick {
            frame: i,
            moment: 1000 + u64::from(i) * 100,
        })
        .collect();
    let scenario = BillboardScenario::new(0x2_0000, ticks.clone());
    let window = scenario.window();
    let plan = FilmPlan::derive(&ticks, window, ClipSelect::All, None, resolution_read_cap())
        .map_err(|e| format!("plan derivation failed: {e}"))?;
    let reproducer = environment_seeded(0xF11B_0A2D);
    let server = film::MockBillboardServer::boot(scenario);
    let mut session = Session::connect(server).map_err(|e| format!("connect failed: {e}"))?;
    let bundle = film(&mut session, &reproducer, &plan).map_err(|e| format!("film failed: {e}"))?;
    println!("filmed {} frame(s) over the mock server", bundle.len());
    let mut renderer = StampRenderer::default();
    render_bundle(&bundle, &mut renderer, out_dir, contact_cols)
}

/// Render every capture, write `frame-NNNN.ppm` + `contact.ppm`, and print the
/// committed blake3 hashes (the images themselves are the game publisher's
/// imagery on the box — never committed; the hashes are).
fn render_bundle(
    bundle: &CaptureBundle,
    renderer: &mut dyn FrameRenderer,
    out_dir: &Path,
    contact_cols: u32,
) -> Result<(), String> {
    fs::create_dir_all(out_dir).map_err(|e| format!("cannot create {}: {e}", out_dir.display()))?;
    let mut frames: Vec<Frame> = Vec::with_capacity(bundle.len());
    for capture in &bundle.frames {
        let frame = renderer
            .render(capture)
            .map_err(|e| format!("render frame {} failed: {e}", capture.frame))?;
        let ppm = write_ppm(&frame);
        let name = format!("frame-{:04}.ppm", capture.frame);
        let path = out_dir.join(&name);
        fs::write(&path, &ppm).map_err(|e| format!("write {}: {e}", path.display()))?;
        println!("{name}  blake3={}", blake3_hex(&ppm));
        frames.push(frame);
    }
    let sheet = contact_sheet(&frames, contact_cols, [0, 0, 0])
        .map_err(|e| format!("contact sheet failed: {e}"))?;
    let sheet_ppm = write_ppm(&sheet);
    let sheet_path = out_dir.join("contact.ppm");
    fs::write(&sheet_path, &sheet_ppm).map_err(|e| format!("write contact sheet: {e}"))?;
    println!(
        "contact.ppm  {}x{}  blake3={}",
        sheet.width(),
        sheet.height(),
        blake3_hex(&sheet_ppm)
    );
    Ok(())
}

/// Pick a renderer. `--core-replay` uses the real core when the feature is
/// compiled in and the env vars are set; it SKIPs loudly to the stamp renderer
/// otherwise, so a missing core/ROM is never a silent success.
fn pick_renderer(core_replay: bool) -> Result<Box<dyn FrameRenderer>, String> {
    if core_replay {
        #[cfg(feature = "core-replay")]
        {
            match film::CoreReplay::from_env() {
                Ok(Some(core)) => return Ok(Box::new(core)),
                Ok(None) => {
                    eprintln!(
                        "SKIP: --core-replay requested but HARMONY_SMB_CORE/HARMONY_SMB_ROM unset; \
                         using the stamp renderer"
                    );
                }
                Err(e) => return Err(format!("core load failed: {e}")),
            }
        }
        #[cfg(not(feature = "core-replay"))]
        {
            eprintln!(
                "SKIP: --core-replay requested but this binary was built without the `core-replay` \
                 feature; using the stamp renderer"
            );
        }
    }
    Ok(Box::new(StampRenderer::default()))
}

/// The task-80 per-read cap the client uses (re-exposed here so the CLI default
/// matches the library).
fn resolution_read_cap() -> u32 {
    // `resolution::READ_CAP` is re-exported transitively; name it directly.
    resolution::READ_CAP
}

/// A genesis-complete, fault-free reproducer for the demo pipeline.
fn environment_seeded(seed: u64) -> film::EnvSpec {
    environment::EnvCodec::seeded(seed, environment::FaultPolicy::none())
}

fn parse_u64(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let parsed = if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16)
    } else {
        s.parse()
    };
    parsed.map_err(|_| format!("not a u64: {s:?}"))
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let bytes = fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {e}", path.display()))
}

fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let json = serde_json::to_vec_pretty(value).map_err(|e| format!("serialize: {e}"))?;
    fs::write(path, json).map_err(|e| format!("write {}: {e}", path.display()))
}
