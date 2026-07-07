# Task 87 — `dissonance/film`: frame reconstruction from the timeline (the visible replay)

> **DELEGABLE (with named sibling deps) · the resolution layer's showpiece: `(reproducer,
> Moment) → what the screen showed.`** The obvious way to watch a discovered game reproducer —
> re-run it with the emulator's rendering switched on — is forbidden by the one-reproducer rule
> (`docs/LAYERS.md`): extra render instructions change the guest instruction stream, so the
> Moments and every `state_hash` diverge, and the thing being filmed is no longer the thing the
> searcher found. Film is instead a **pure observation query over the one timeline**: replay
> the reproducer, and at each recorded frame-clock `Moment` read the billboard (task 86's
> always-on PPU-state export) with the task-80 `read` verb, composite the frame host-side, and
> emit it. The filmed and unfilmed replays are the *same timeline* — and the box gate proves
> it. Every verb this composes is already ruled: materialize(`MomentRef`) + `run(until)` +
> `read` (`docs/RESOLUTION.md`'s verb table, tasks 58/80/82).
>
> Depends on **tasks 80/82** for the live gate (the `read`/`regs` verbs and the session
> client); on **task 86** for the live filming target (the billboard layout + `REG_FRAME`
> frame clock). The crate itself builds and **fully gates against an in-crate mock** (the
> task-82 pattern — code against the wire contract and the billboard header, both fixed by
> those specs), so it is dispatchable as soon as 82's spec is stable; the one thin box gate
> queues behind 80/82/86 and is handed to the foreman.

Read first: `tasks/00-CONVENTIONS.md`, `docs/GLOSSARY.md` (binding register; note **timeline**
is the resolution layer's user-facing word), `docs/RESOLUTION.md` (the moment address, the verb
table, the transcript-as-artifact principle), `docs/LAYERS.md` (the one-reproducer constraint —
this task's doctrine), `tasks/80-inspection-verbs.md` (`read` semantics + the length cap),
`tasks/82-resolution-crate.md` (`MomentRef`, the session client this drives),
`tasks/86-game-workload.md` (the billboard header + register conventions),
`dissonance/control-proto/src/` (the codec), `dissonance/environment/src/`.

**Dependency grant (hard rule 2 exception, explicit):** `dissonance/control-proto`,
`dissonance/environment`, and `dissonance/resolution` (as the client library) as normal
workspace deps — this crate is a client of that wire contract, exactly like task 82's grant.
No dependency on `explorer`.

## Environment

Pure-logic, macOS + Linux, laptop-gated: all tests run against an **in-crate mock** (the
task-82 loopback pattern — a scripted server speaking the real codec, serving synthetic
billboard bytes) plus **hand-built synthetic PPU fixtures** (nametables, pattern tables,
palettes, sprite tables constructed in test code; golden frames pinned by blake3 hash). **No
ROM, no emulator, no core anywhere in this crate or its tests.** **Box gate:** one live
scenario, handed to the foreman (pinned per `docs/BOX-PINNING.md`, box-safety per task 86's
section).

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
decode, `run(until next frame Moment)`. One materialization per clip, linear from there; on a
dropped session, re-materialize at the failed frame and continue. Reads are host-side and
hash-neutral by construction — the projector never sends anything but observation verbs.

### 3. `FrameDecoder` — the seam — and `SmbDecoder` — the v1

`FrameDecoder`: billboard bytes → an RGB frame (integer-only math; rule 4 — floats never touch
frame state; goldens are byte-exact). `SmbDecoder` v1 composites the NES PPU from the
billboard: the single background layer from nametables + attribute tables, tiles from the
pattern tables (2bpp), colors through palette RAM and a **fixed 64-entry NES master-palette
RGB table committed as an integer constant** (the NES emits composite-video chroma, not RGB;
a canonical lookup is standard practice), sprites from OAM (front/behind-background priority,
8×8 and 8×16), scroll from SMB's WRAM mirrors (`HorizontalScroll`, `Mirror_PPU_CTRL_REG1` —
in the billboard's RAM region). 256×240 output (overscan cropping is the viewer's business,
not the decoder's).

**The fidelity bar is "recognizable gameplay," not bit-parity with a reference emulator** —
this is reconstruction from end-of-frame state, and the approximations are documented, not
hidden. One raster special case is sanctioned: SMB statically splits the screen at the status
bar (sprite-0 hit), so the decoder renders the status-bar rows with zero scroll and the
playfield with the mirrored scroll — without it every frame's HUD would smear. All other
mid-frame raster tricks are v1-out and render from end-of-frame state. Emulator debuggers'
tile/map/sprite viewers are the prior art for exactly this kind of state-driven compositing.

### 4. Output — zero new dependencies

PPM (P6) frame sequence plus a contact-sheet mode (every Nth frame in one image) — trivially
hand-written, nothing added to the whitelist. Video encoding stays **outside the repo**
(document the ffmpeg one-liner in the crate README). Golden tests pin blake3 hashes of decoded
synthetic frames; at most one small committed golden image.

Naming: `film` enters `docs/GLOSSARY.md` (Adopted vocabulary) in this task's implementation PR
— the name-when-built discipline the glossary's Reserved section sets.

## Acceptance gates

1. **Portable (macOS + Linux):** `FilmPlan` derivation proptested (≥256) over synthetic traces
   (frame clocks with gaps, chunked windows, stride edge cases); header-verification rejects
   every corruption class (bad magic, version skew, frame-counter mismatch); `SmbDecoder`
   golden-frame tests over the synthetic PPU fixtures (background + attribute colors, sprite
   priority, palette lookup, scroll including the status-bar split); the projector driven
   end-to-end against the mock server (a scripted 3-frame clip round-trips to 3 correct
   frames). Standard suite green.
2. **Box gate (one, thin, foreman-handled):** film a ≥300-frame clip from task 86's deep
   reproducer end-to-end on real KVM. Assert (a) the frames decode and a contact sheet is
   produced (committed as the report artifact), and (b) **hash-neutrality**: the filmed
   replay's terminal `state_hash` equals the unfilmed replay's, same seed, **25/25** — the
   one-timeline claim, proven, not asserted.

## Non-goals

- **Live/paced viewing, streaming, or any viewer UI** — a later tier; the natural embedding is
  task 83's findings page (a clip beside a `MomentRef`), not built here.
- **Video encoding in-repo** — PPM out, ffmpeg outside.
- **PPU completeness** — no mid-frame raster effects beyond the sanctioned status-bar split,
  no emphasis bits/grayscale edge cases beyond what SMB uses; the decoder seam exists so a
  later task can raise fidelity (or add the SNES Mode-1 decoder for Super Mario World)
  without touching the projector.
- **Other consoles/workloads** — `FrameDecoder` is the seam; `SmbDecoder` is the only impl.
  Metroid (same console) reuses it as-is; Super Mario World (SNES) is a new impl, its own
  task.
- **Any guest-side change** — the billboard is task 86's surface; if its layout needs
  amending, that is a task-86 follow-up ruled by the integrator, not a patch here.
- **REPL/transcript work** — task 82 owns the interactive surface; film is a batch query.
