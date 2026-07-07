# Task 87 — `dissonance/film`: the visible replay, rendered by the core itself

> **DELEGABLE (with named sibling deps) · the resolution layer's showpiece: `(reproducer,
> Moment) → what the screen showed.`** The obvious way to watch a discovered game reproducer —
> re-run it with the emulator's rendering switched on — is forbidden by the one-reproducer rule
> (`docs/LAYERS.md`): extra render instructions change the guest instruction stream, so the
> Moments and every `state_hash` diverge, and the thing being filmed is no longer the thing the
> searcher found. Film is instead a **pure observation query over the one timeline**: replay
> the reproducer, and at each recorded frame-clock `Moment` read the billboard (task 86's
> always-on core-state export) with the task-80 `read` verb — then, **host-side and outside
> the timeline**, load each captured savestate into the *same commit-pinned core* and run
> exactly one frame with the recorded joypad byte. The emitted picture is the core's own
> rendering of that exact frame — **1:1 by construction** (integrator fidelity ruling,
> 2026-07-07): there is no reconstruction step whose faithfulness could be doubted, so no
> investigation can be misled by an approximated pixel. The filmed and unfilmed replays are
> the *same timeline* — and the box gate proves it. Every wire verb this composes is already
> ruled: materialize(`MomentRef`) + `run(until)` + `read` (`docs/RESOLUTION.md`'s verb table,
> tasks 58/80/82).
>
> Depends on **tasks 80/82** for the live gate (the `read`/`regs` verbs and the session
> client); on **task 86** for the live filming target (the billboard layout + `REG_FRAME`
> frame clock + the shared core pin). The crate's plan/projector/output logic builds and
> **fully gates against an in-crate mock** (the task-82 pattern — code against the wire
> contract and the billboard header, both fixed by those specs), so it is dispatchable as
> soon as 82's spec is stable; the renderer path and the one thin box gate queue behind
> 80/82/86 and are handed to the foreman.

Read first: `tasks/00-CONVENTIONS.md`, `docs/GLOSSARY.md` (binding register; note **timeline**
is the resolution layer's user-facing word), `docs/RESOLUTION.md` (the moment address, the verb
table, the transcript-as-artifact principle), `docs/LAYERS.md` (the one-reproducer constraint —
this task's doctrine), `tasks/80-inspection-verbs.md` (`read` semantics + the length cap),
`tasks/82-resolution-crate.md` (`MomentRef`, the session client this drives),
`tasks/86-game-workload.md` (the billboard header + the core pin this shares),
`dissonance/control-proto/src/` (the codec), `dissonance/environment/src/`.

**Dependency grant (hard rule 2 exception, explicit):** `dissonance/control-proto`,
`dissonance/environment`, and `dissonance/resolution` (as the client library) as normal
workspace deps — this crate is a client of that wire contract, exactly like task 82's grant.
No dependency on `explorer`. **`unsafe` grant (named):** the libretro C-ABI FFI in the
renderer, behind the `FrameRenderer` seam so default tests never cross it; every block gets
`// SAFETY:`.

## Environment

Pure-logic core of the crate, macOS + Linux, laptop-gated: plan/projector/header/output tests
run against an **in-crate mock** (the task-82 loopback pattern — a scripted server speaking
the real codec, serving synthetic billboard bytes) with a trivial test `FrameRenderer` (a fake
that stamps deterministic pixels from the billboard header). **No ROM and no core in the
default build or tests.** The real renderer is a **feature/bin-gated path**: it loads the
task-86 commit-pinned core (built by the same provisioning tooling — never vendored) and the
user-supplied ROM via `HARMONY_SMB_ROM`; when either is absent it reports **SKIP loudly**,
mirroring task 86's ROM discipline. **Box gate:** one live scenario, handed to the foreman
(pinned per `docs/BOX-PINNING.md`, box-safety per task 86's section) — the box builds guest
and renderer from the identical core pin on the identical platform, which is what makes
savestate loading a non-issue there.

## What to build

### 1. `FilmPlan` — the shot list, derived from the record

Pure derivation from a reproducer's recorded trace: the `REG_FRAME` channel gives the
frame-clock `Moment`s; the billboard address registers give the read windows (gpa + len,
respecting the task-80 length cap via chunking); a `[start, end]` `Span` (or frame range)
selects the clip; an optional stride (every Nth frame) selects contact-sheet density. A
`FilmPlan` is serializable and inspectable — the transcript principle: the query itself is a
replayable artifact.

### 2. The projector — the session driver

Over the task-82 client: materialize the reproducer at the clip's first frame `Moment`, then
per frame — `read` the billboard, verify the header (magic, layout version, **frame counter
matches the frame clock**; a mismatch is a hard error, never a silently misaligned frame),
store the capture, `run(until next frame Moment)`. One materialization per clip, linear from
there; on a dropped session, re-materialize at the failed frame and continue. Reads are
host-side and hash-neutral by construction — the projector never sends anything but
observation verbs. Capture (in-timeline, verbs only) and rendering (host-side, below) are
separate passes; a capture bundle can be rendered later or elsewhere.

### 3. `FrameRenderer` — the seam — and `CoreReplay` — the only impl

`FrameRenderer`: billboard capture → an RGB frame. `CoreReplay` does it with **zero
interpretation of pixels**: initialize the pinned core with the ROM, `retro_unserialize` the
capture's savestate, present the header's joypad byte through the input callback, call
`retro_run` exactly once, and take the frame the core hands the video callback. Every raster
trick, palette nuance, and mid-frame effect is exactly what the core would have displayed,
because the core displays it. The seam is console-agnostic — Metroid reuses it as-is; Super
Mario World changes only the core pin.

**Rejected alternative (recorded for the reviewer):** a hand-written PPU compositor
reconstructing frames from raw VRAM/OAM/palette state. Rejected by the integrator
(2026-07-07): reconstruction is approximate by nature (mid-frame raster effects, palette
interpretation), and an investigator shown an approximated frame can reach a wrong conclusion
about what happened. A measured-fidelity cross-check was considered and is strictly worse
than 1:1-by-construction. This also deleted the largest block of would-be code in the crate.

Determinism note: rendering is a pure function of (core pin, ROM, savestate, joypad byte) —
the box gate asserts it by rendering a clip twice to byte-identical output. Rule-4 discipline
applies to everything around the core (no floats in plan/capture/output state; goldens are
byte-exact).

### 4. Output — zero new dependencies

PPM (P6) frame sequence plus a contact-sheet mode (every Nth frame in one image) — trivially
hand-written, nothing added to the whitelist. Video encoding stays **outside the repo**
(document the ffmpeg one-liner in the crate README). Rendered SMB frames are **never
committed** (they are Nintendo's imagery; same hygiene posture as the ROM) — committed
artifacts are blake3 hashes; the contact sheet itself is attached to the PR/report.

Naming: `film` enters `docs/GLOSSARY.md` (Adopted vocabulary) in this task's implementation PR
— the name-when-built discipline the glossary's Reserved section sets.

## Acceptance gates

1. **Portable (macOS + Linux):** `FilmPlan` derivation proptested (≥256) over synthetic traces
   (frame clocks with gaps, chunked windows, stride edge cases); header-verification rejects
   every corruption class (bad magic, version skew, frame-counter mismatch); the projector
   driven end-to-end against the mock server with the fake `FrameRenderer` (a scripted 3-frame
   clip round-trips to 3 correct frames); PPM and contact-sheet writers golden-tested.
   Standard suite green. All of this runs with no ROM and no core present.
2. **Box gate (one, thin, foreman-handled):** film a ≥300-frame clip from task 86's deep
   reproducer end-to-end on real KVM. Assert (a) every frame renders through `CoreReplay`
   (zero header mismatches, zero unserialize failures) and the contact sheet is produced
   (hashes committed, sheet attached to the report); (b) **render determinism**: the same
   capture bundle rendered twice is byte-identical; (c) **hash-neutrality**: the filmed
   replay's terminal `state_hash` equals the unfilmed replay's, same seed, **25/25** — the
   one-timeline claim, proven, not asserted.

## Non-goals

- **Live/paced viewing, streaming, or any viewer UI** — a later tier; the natural embedding is
  task 83's findings page (a clip beside a `MomentRef`), not built here.
- **Video encoding in-repo** — PPM out, ffmpeg outside.
- **Hand-written PPU decoding** — rejected above; if a future console's core cannot
  serialize/replay, that console brings its own ruling.
- **Cross-platform savestate portability** — same-pin same-platform (the box) is the gated
  path; a mac laptop loading box-produced savestates is best-effort, documented.
- **Other consoles/workloads** — `FrameRenderer` is the seam; `CoreReplay` over the task-86
  pin is the only impl. Metroid reuses both unchanged; Super Mario World changes the pin,
  its own task.
- **Any guest-side change** — the billboard is task 86's surface; if its layout needs
  amending, that is a task-86 follow-up ruled by the integrator, not a patch here.
- **REPL/transcript work** — task 82 owns the interactive surface; film is a batch query.
