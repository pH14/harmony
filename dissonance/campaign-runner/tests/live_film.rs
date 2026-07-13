// SPDX-License-Identifier: AGPL-3.0-or-later
//! **Task-86 M0 film live gate** (task 87's re-homed box gate) —
//! `#![cfg(target_os = "linux")]` **and** `#[ignore]`: needs real + LOADED
//! patched KVM, the det-cfl-v1 host, and the built game image
//! (`initramfs-game.cpio.gz` with the ROM baked in). One test drives the whole
//! re-homed gate:
//!
//! - **(a) core loads in the box guest** — the boot to `GAME_READY` proves
//!   `retro_load_game` (the play-agent FATALs before the marker otherwise).
//! - **(b) env_cb validation** — the render step's first successful
//!   `unserialize` + `retro_run` through [`film::CoreReplay`] (load-time
//!   `RenderError::Unavailable` if the pinned core demands services film's
//!   `env_cb` refuses).
//! - **(c) a ≥300-frame clip renders** from a real captured campaign timeline
//!   (zero header mismatches, zero unserialize failures; PPM frames + the
//!   contact sheet written to `FILM_OUT_DIR`).
//! - **(d) render determinism** — the same bundle rendered twice is
//!   byte-identical (blake3 over every frame + the sheet).
//! - **(e) hash-neutrality 25/25** — the filmed replay's terminal
//!   `state_hash` equals the unfilmed replay's, same seed, `REPS`/`REPS`
//!   (observation verbs leave the one timeline untouched: proven, not
//!   asserted).
//!
//! ## The capture path
//!
//! `film::film()` wants a [`resolution::Server`], and it gets the **production**
//! one: [`resolution::SocketServer`] (task 107), speaking the real
//! `control-proto` wire to vmm-core's `ControlServer` — the same verb socket the
//! explorer drives. This gate carried a test-local copy of that adapter until
//! the seam it was standing in for was built; it does not any more, and must not
//! grow one again.
//!
//! Two properties of that adapter this gate leans on, both part of its contract
//! rather than local liberties: `hello` is **negotiated once per stream** (later
//! calls answer from the cached caps), so the raw scrape pass below and the
//! `Session` layered over the same adapter share one wire session; and the
//! session is rooted with [`Session::connect_rooted`] at the base snapshot the
//! frame-clock ticks were scraped from — absolute `Moment`s are only reachable
//! from that root, so `film()`'s materialize must branch off it and not off a
//! fresh snapshot taken wherever the server happens to sit.
//!
//! Run (per `docs/BOX-PINNING.md`; needs `HARMONY_SMB_CORE` +
//! `HARMONY_SMB_ROM` for the render half):
//!
//! ```sh
//! HARMONY_SMB_CORE=guest/build/fceumm_libretro.so \
//! HARMONY_SMB_ROM=/root/roms/smb.nes \
//! FILM_OUT_DIR=/root/t86-film \
//! taskset -c <leased core> timeout 7200 cargo test -p campaign-runner --test live_film \
//!     -- --ignored --nocapture --test-threads=1 2>&1 | tee /root/t86-film.log
//! ```
//!
//! Knobs: `FILM_DELTA_VNS` (v-time past the base the scrape runs; default
//! 4·10⁹), `FILM_SEED`, `FILM_REPS` (neutrality repetitions; default 25 — the
//! gate floor: lower values print an explicit BELOW-FLOOR line and fail),
//! `FILM_MIN_FRAMES` (default 300), `KERNEL`/`INITRAMFS`/`READY_MARKER`.
//!
//! **Box-safety (CRITICAL).** Stock `kvm` = 1396736; ALWAYS leave the box on
//! stock + verified after the run (the `box-window.sh` lease does this on
//! release).

#![cfg(target_os = "linux")]

use std::io::{Read, Write};

use control_proto::{HashScope, Moment, Reproducer, StopConditions, StopMask, StopReason};
use environment::{EnvSpec, FaultPolicy};
use film::{
    BillboardWindow, CaptureBundle, ClipSelect, CoreReplay, FilmPlan, FrameRenderer, FrameTick,
    Server, Session, blake3_hex, contact_sheet, film, write_ppm,
};
use resolution::{SessionError, SocketServer};
use vmm_backend::Backend;
use vmm_core::bringup::{BackendKind, boot_linux_selected};
use vmm_core::control::{ControlServer, VmmFactory};
use vmm_core::vmm::{Step, Vmm};

/// 2 GiB guest RAM (the game-image boot shape, matching `campaign-runner game box`).
const GUEST_RAM_LEN: usize = 2 << 30;
/// The boot seed the live VM runs under (matches the branching demo).
const BOOT_SEED: u64 = 0x0028_C0FF_EE5E_EDC0;
/// The determinism command line (identical to the campaign-runner box modes).
const CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t,force tsc=reliable no_timer_check \
                       lpj=4000000 nokaslr nosmp maxcpus=1 nox2apic hpet=disable";
/// A safety cap on the boot-to-marker drive.
const MAX_BOOT_STEPS: u64 = 50_000_000_000;

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.replace('_', "").parse().ok())
        .unwrap_or(default)
}

// ---------------------------------------------------------------------------
// Boot (the live_materialization shape, on the game image).
// ---------------------------------------------------------------------------

fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn artifact(name: &str) -> Option<Vec<u8>> {
    for p in [
        repo_root().join("guest/build").join(name),
        repo_root().join("guest/linux").join(name),
    ] {
        if let Ok(bytes) = std::fs::read(&p) {
            return Some(bytes);
        }
    }
    None
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

fn drive_to_marker(vmm: &mut Vmm<Box<dyn Backend>>, marker: &[u8]) -> Result<u64, String> {
    let stderr = std::io::stderr();
    let mut printed = vmm.serial().len();
    let overlap = marker.len().saturating_sub(1);
    let mut scan_from = printed.saturating_sub(overlap);
    let mut steps = 0u64;
    while steps < MAX_BOOT_STEPS {
        match vmm.step() {
            Ok(Step::Continued) => {}
            Ok(Step::Terminal(r)) => {
                return Err(format!(
                    "guest terminal ({r:?}) before the readiness marker"
                ));
            }
            Ok(Step::SdkStop) => {
                return Err("guest SDK stop before the readiness marker".to_string());
            }
            Err(e) => return Err(format!("step error at {steps}: {e}")),
        }
        steps += 1;
        let serial = vmm.serial();
        if serial.len() > printed {
            let mut h = stderr.lock();
            let _ = h.write_all(&serial[printed..]);
            let _ = h.flush();
            printed = serial.len();
            if contains(&serial[scan_from..], marker) {
                return Ok(steps);
            }
            scan_from = serial.len().saturating_sub(overlap);
        }
    }
    Err(format!("marker not seen within {MAX_BOOT_STEPS} steps"))
}

fn boot_game_server() -> ControlServer<Box<dyn Backend>> {
    assert!(
        std::path::Path::new("/dev/kvm").exists(),
        "/dev/kvm absent — run on the determinism box (patched KVM loaded)"
    );
    let kernel = artifact(&env_or("KERNEL", "bzImage")).expect("bzImage under guest/build");
    let initramfs = artifact(&env_or("INITRAMFS", "initramfs-game.cpio.gz"))
        .expect("initramfs-game.cpio.gz under guest/build (make -C guest game-image)");
    let marker = env_or("READY_MARKER", "GAME_READY");
    let mut live = boot_linux_selected(
        BackendKind::Patched,
        &kernel,
        &initramfs,
        GUEST_RAM_LEN,
        CMDLINE,
        BOOT_SEED,
    )
    .expect("patched boot");
    let steps = drive_to_marker(&mut live, marker.as_bytes()).expect("boot to GAME_READY");
    eprintln!("\n[live_film] readiness at step {steps}");
    let factory: VmmFactory<Box<dyn Backend>> = Box::new(move || {
        boot_linux_selected(
            BackendKind::Patched,
            &kernel,
            &initramfs,
            GUEST_RAM_LEN,
            CMDLINE,
            BOOT_SEED,
        )
    });
    ControlServer::new(live, factory)
}

// ---------------------------------------------------------------------------
// The scrape fold: SDK events → frame ticks + billboard window.
// ---------------------------------------------------------------------------

/// Fold the drained SDK capture into `REG_FRAME` ticks and the billboard
/// `(gpa, len)` — the play-agent's register catalog, via the same
/// `sdk_events::decode_events` path the campaign uses.
fn scrape_plan_inputs(raw: &[(u64, u32, Vec<u8>)]) -> (Vec<FrameTick>, Option<(u64, u64)>) {
    use campaign_runner::gamecampaign::reg;
    let decoded = sdk_events::decode_events(
        &raw.iter()
            .map(|(m, id, b)| (explorer::Moment(*m), *id, b.clone()))
            .collect::<Vec<_>>(),
    );
    let mut ticks: Vec<FrameTick> = Vec::new();
    let (mut gpa, mut len) = (None, None);
    for (moment, ev) in &decoded {
        if ev.kind != sdk_events::KIND_STATE {
            continue;
        }
        let (Some(explorer::Value::UInt(reg)), Some(explorer::Value::UInt(value))) =
            (ev.attrs.get("reg"), ev.attrs.get("value"))
        else {
            continue;
        };
        match *reg {
            reg::FRAME => {
                let tick = FrameTick {
                    frame: u32::try_from(*value).unwrap_or(u32::MAX),
                    moment: moment.0,
                };
                // SDK events are batch-stamped at drain time, so several
                // frames can share one `Moment`. Only the LAST frame written
                // at-or-before a moment is what the billboard holds when a
                // `run(until = moment)` lands there — keep-last per moment
                // (FilmPlan::derive requires strictly increasing moments).
                match ticks.last_mut() {
                    Some(prev) if prev.moment == tick.moment => *prev = tick,
                    _ => ticks.push(tick),
                }
            }
            reg::BILLBOARD_GPA => gpa = Some(*value),
            reg::BILLBOARD_LEN => len = Some(*value),
            _ => {}
        }
    }
    (ticks, gpa.zip(len))
}

// ---------------------------------------------------------------------------
// The gate.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "box-only: patched KVM + game image + core/ROM (see module doc)"]
fn film_live_gate() {
    let seed = env_u64("FILM_SEED", 0x0086_F11A_0001);
    let delta = env_u64("FILM_DELTA_VNS", 4_000_000_000);
    let reps = env_u64("FILM_REPS", 25) as usize;
    let min_frames = env_u64("FILM_MIN_FRAMES", 300) as usize;
    let out_dir = std::path::PathBuf::from(env_or("FILM_OUT_DIR", "/tmp/t86-film"));
    assert!(reps >= 25, "FILM_REPS {reps} is BELOW the 25/25 gate floor");

    let mut server = boot_game_server();
    let (served, gate) = campaign_runner::run_session(&mut server, move |stream| {
        run_gate(stream, seed, delta, reps, min_frames, &out_dir)
            .map_err(explorer::MachineError::Transport)
    });
    served.expect("server session");
    gate.expect("film live gate");
}

fn run_gate<S: Read + Write>(
    stream: S,
    seed: u64,
    delta: u64,
    reps: usize,
    min_frames: usize,
    out_dir: &std::path::Path,
) -> Result<(), String> {
    let spec = EnvSpec::Seeded {
        seed,
        policy: FaultPolicy::none(),
    };
    let wire_env = Reproducer {
        blob_version: EnvSpec::BLOB_VERSION,
        bytes: spec.encode(),
    };

    let mut adapter = SocketServer::new(stream);
    adapter
        .hello(resolution::client_caps())
        .map_err(|e| format!("hello: {e}"))?;

    // The base: the server sits at GAME_READY; probe its v-time (deadline-0
    // run — checked before entering the guest) and snapshot. GAME_READY is not
    // necessarily a snapshottable boundary — retry with small v-time steps
    // until one is reached, exactly the campaign's seal shape (same constants
    // as `GameCampaignConfig`: 1 ms steps, bounded attempts).
    let mut base_vtime = match adapter
        .run(StopConditions {
            deadline: Some(Moment(0)),
            on: StopMask::NONE,
        })
        .map_err(|e| format!("vtime probe: {e}"))?
    {
        StopReason::Deadline { vtime } => vtime.0,
        other => return Err(format!("vtime probe stopped oddly: {other:?}")),
    };
    const SNAPSHOT_RETRY_STEP: u64 = 1_000_000;
    const SNAPSHOT_MAX_ATTEMPTS: usize = 100_000;
    let mut base = None;
    for _ in 0..SNAPSHOT_MAX_ATTEMPTS {
        match adapter.snapshot() {
            Ok(snap) => {
                base = Some(snap.id);
                break;
            }
            Err(SessionError::Control(control_proto::ControlError::NotQuiescent)) => {
                base_vtime = match adapter
                    .run(StopConditions {
                        deadline: Some(Moment(base_vtime.saturating_add(SNAPSHOT_RETRY_STEP))),
                        on: StopMask::NONE,
                    })
                    .map_err(|e| format!("seal-retry run: {e}"))?
                {
                    StopReason::Deadline { vtime } => vtime.0,
                    other => return Err(format!("guest died during the seal retry: {other:?}")),
                };
            }
            Err(e) => return Err(format!("base snapshot: {e}")),
        }
    }
    let base = base.ok_or("no snapshottable boundary within the retry budget")?;
    let terminal = base_vtime.saturating_add(delta);
    eprintln!("[live_film] base at v-time {base_vtime}; terminal {terminal}");

    // --- Scrape pass: one branch to the terminal, harvesting the frame clock
    // + billboard registers and the unfilmed terminal hash.
    let run_to_terminal = |a: &mut SocketServer<S>| -> Result<[u8; 32], String> {
        a.branch(base, &wire_env)
            .map_err(|e| format!("branch: {e}"))?;
        match a
            .run(StopConditions {
                deadline: Some(Moment(terminal)),
                on: StopMask::NONE,
            })
            .map_err(|e| format!("run: {e}"))?
        {
            StopReason::Deadline { .. } => {}
            other => return Err(format!("rollout died before its deadline: {other:?}")),
        }
        a.hash(HashScope::Whole).map_err(|e| format!("hash: {e}"))
    };

    let h_unfilmed = run_to_terminal(&mut adapter)?;
    let raw = adapter
        .sdk_events()
        .map_err(|e| format!("sdk_events: {e}"))?;
    let (stamps, billboard) = scrape_plan_inputs(&raw);
    let (gpa, len) = billboard.ok_or("scrape saw no billboard (gpa, len) registers")?;
    eprintln!(
        "[live_film] scraped {} distinct event stamps; billboard gpa={gpa:#x} len={len}",
        stamps.len()
    );

    // --- Unfilmed determinism floor: REPS identical terminal hashes.
    for i in 0..reps {
        let h = run_to_terminal(&mut adapter)?;
        if h != h_unfilmed {
            return Err(format!("unfilmed replay {i} diverged — determinism broken"));
        }
    }
    eprintln!("[live_film] unfilmed terminal hash stable {reps}/{reps}");

    // --- Calibration: the event stamps are anchored to the last V-time
    // intercept (~10⁷ v-ns apart on this substrate), so a stamp is a LOWER
    // BOUND on when its frame ran, not an exact address — landing exactly on
    // a stamp shows the pre-batch billboard (surfaced live; recorded in
    // IMPLEMENTATION-task86.md as a spine finding, not patched here). The
    // billboard itself is the address authority: advance ONE branch through
    // the distinct stamp moments and read each header's ACTUAL frame counter
    // — the observed (frame, moment) pairs are exact by determinism, so
    // film()'s landings re-observe them by construction.
    // The header parse validates the region table against the buffer it is
    // given, so calibration reads the WHOLE window (15–35 KiB, under the
    // read cap), not just the 32 header bytes.
    let window_len32 = u32::try_from(len).map_err(|_| "billboard len exceeds u32")?;
    if window_len32 > resolution::READ_CAP {
        return Err(format!(
            "billboard window ({window_len32} bytes) exceeds the read cap — chunked calibration \
             not implemented"
        ));
    }
    adapter
        .branch(base, &wire_env)
        .map_err(|e| format!("calibration branch: {e}"))?;
    let mut ticks: Vec<FrameTick> = Vec::new();
    for stamp in &stamps {
        if stamp.moment <= base_vtime {
            continue;
        }
        match adapter
            .run(StopConditions {
                deadline: Some(Moment(stamp.moment)),
                on: StopMask::NONE,
            })
            .map_err(|e| format!("calibration run: {e}"))?
        {
            StopReason::Deadline { .. } => {}
            other => return Err(format!("guest died during calibration: {other:?}")),
        }
        let window = adapter
            .read(gpa, window_len32)
            .map_err(|e| format!("calibration read: {e}"))?;
        let header = film::BillboardHeader::parse(&window)
            .map_err(|e| format!("calibration header at {}: {e}", stamp.moment))?;
        match ticks.last() {
            Some(prev) if prev.frame == header.frame => {}
            Some(prev) if header.frame < prev.frame => {
                return Err(format!(
                    "billboard frame went backwards during calibration ({} then {})",
                    prev.frame, header.frame
                ));
            }
            _ => ticks.push(FrameTick {
                frame: header.frame,
                moment: stamp.moment,
            }),
        }
    }
    eprintln!(
        "[live_film] calibrated {} exact (frame, moment) pairs",
        ticks.len()
    );
    if ticks.len() < min_frames {
        return Err(format!(
            "only {} calibrated frames within {delta} v-ns (need >= {min_frames}) — raise \
             FILM_DELTA_VNS",
            ticks.len()
        ));
    }

    // --- The plan: the first `min_frames` calibrated pairs (stride none — a
    // contiguous clip), chunked reads under the client cap.
    let clip_last = ticks[min_frames - 1].frame;
    let clip_first = ticks[0].frame;
    let len32 = u32::try_from(len).map_err(|_| "billboard len exceeds u32")?;
    let plan = FilmPlan::derive(
        &ticks,
        BillboardWindow { gpa, len: len32 },
        ClipSelect::FrameRange {
            first: clip_first,
            last: clip_last,
        },
        None,
        resolution::READ_CAP,
    )
    .map_err(|e| format!("plan: {e}"))?;
    eprintln!("[live_film] plan: {} frames", plan.frames.len());

    // --- Filmed passes: capture, then run on to the same terminal — the
    // hash must equal the unfilmed one (observation is not an event), REPS/REPS.
    //
    // The session is ROOTED AT `base`: the plan's frame moments are absolute
    // V-times harvested from runs branched off that snapshot, so `film()`'s
    // materialize must branch off it too. (`connect` would take a fresh snapshot
    // of wherever the server sits now — a different root, and every absolute
    // Moment in the plan would name a different instant.) The `hello` the session
    // sends is answered from the adapter's cached negotiation, so this shares the
    // one wire session the scrape pass above ran on.
    let mut session =
        Session::connect_rooted(&mut adapter, base).map_err(|e| format!("connect: {e}"))?;
    let mut first_bundle: Option<CaptureBundle> = None;
    for i in 0..reps {
        let bundle = film(&mut session, &spec, &plan).map_err(|e| format!("film pass {i}: {e}"))?;
        match &first_bundle {
            None => first_bundle = Some(bundle),
            Some(first) => {
                let a = serde_json::to_vec(first).map_err(|e| e.to_string())?;
                let b = serde_json::to_vec(&bundle).map_err(|e| e.to_string())?;
                if a != b {
                    return Err(format!("capture pass {i} produced a different bundle"));
                }
            }
        }
        // Continue the filmed timeline to the terminal and compare.
        let mut mat = session.materialized().map_err(|e| format!("mat: {e}"))?;
        let stop = mat
            .run(terminal)
            .map_err(|e| format!("filmed run-on {i}: {e}"))?;
        if !matches!(stop, StopReason::Deadline { .. }) {
            return Err(format!(
                "filmed timeline {i} died before terminal: {stop:?}"
            ));
        }
        let h = mat.hash().map_err(|e| format!("filmed hash {i}: {e}"))?;
        if h != h_unfilmed {
            return Err(format!(
                "HASH-NEUTRALITY VIOLATION at filmed pass {i}: filmed {} != unfilmed {}",
                hex(&h),
                hex(&h_unfilmed)
            ));
        }
    }
    eprintln!("[live_film] hash-neutrality {reps}/{reps} (filmed == unfilmed)");
    let bundle = first_bundle.expect("reps >= 25 > 0");
    drop(session);

    // --- Persist the bundle (film render's input artifact).
    std::fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;
    let bundle_path = out_dir.join("bundle.json");
    std::fs::write(
        &bundle_path,
        serde_json::to_vec_pretty(&bundle).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    eprintln!("[live_film] bundle: {}", bundle_path.display());

    // --- Render (gates b/c/d): CoreReplay from HARMONY_SMB_CORE/ROM, every
    // frame + the contact sheet, twice, byte-identical.
    let render_all = || -> Result<(Vec<film::Frame>, Vec<String>, film::Frame), String> {
        let mut core = CoreReplay::from_env()
            .map_err(|e| format!("core-replay load (env_cb validation): {e}"))?
            .ok_or("HARMONY_SMB_CORE/HARMONY_SMB_ROM unset — the render half is the gate")?;
        let mut hashes = Vec::new();
        let mut frames = Vec::new();
        for capture in &bundle.frames {
            let frame = core
                .render(capture)
                .map_err(|e| format!("render frame {}: {e}", capture.frame))?;
            hashes.push(blake3_hex(frame.rgb()));
            frames.push(frame);
        }
        let sheet =
            contact_sheet(&frames, 8, [0, 0, 0]).map_err(|e| format!("contact sheet: {e}"))?;
        Ok((frames, hashes, sheet))
    };
    let (frames_a, hashes_a, sheet_a) = render_all()?;
    let (_, hashes_b, sheet_b) = render_all()?;
    if hashes_a != hashes_b || sheet_a.rgb() != sheet_b.rgb() {
        return Err("RENDER-DETERMINISM VIOLATION: two renders differ".to_string());
    }
    eprintln!(
        "[live_film] render determinism: {} frames + sheet, twice, identical",
        hashes_a.len()
    );

    // Write the PPMs + sheet for the visual record.
    for (i, f) in frames_a.iter().enumerate() {
        std::fs::write(out_dir.join(format!("frame-{i:04}.ppm")), write_ppm(f))
            .map_err(|e| e.to_string())?;
    }
    std::fs::write(out_dir.join("contact.ppm"), write_ppm(&sheet_a)).map_err(|e| e.to_string())?;
    eprintln!(
        "[live_film] wrote {} PPMs + contact.ppm under {}; sheet blake3 {}",
        frames_a.len(),
        out_dir.display(),
        blake3_hex(sheet_a.rgb())
    );
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
