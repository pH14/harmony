# film — implementation notes (task 87)

`dissonance/film` films a discovered game reproducer: `(reproducer, Moment) →
what the screen showed`, as a **pure observation query over the one timeline**.
Four passes, one seam:

- **`plan`** — `FilmPlan::derive` turns a recorded trace's `REG_FRAME` frame
  clock + billboard window into a serializable shot list (chunked to the task-80
  read cap).
- **`projector`** — `film()` drives the task-82 `Session`: materialize once,
  then per frame `read` → verify header → `run` to the next frame. Observation
  and navigation verbs only (hash-neutral).
- **`render`** — the `FrameRenderer` seam; `StampRenderer` (pure test fake) by
  default, `CoreReplay` (libretro FFI, `core-replay` feature) on the box.
- **`output`** — PPM (P6) writer + contact sheet; `blake3_hex` is the committed
  artifact.

## Gate results (portable, laptop, no ROM/core)

All green on macOS (this worktree):

- `cargo build -p film --all-features` ✓
- `cargo nextest run -p film --all-features` — **45 tests** (28 unit + 4 plan
  proptests ≥256 cases + 11 projector-mock + 2 artifact-roundtrip) ✓
- `cargo clippy -p film --all-features --all-targets -- -D warnings` ✓ (the
  `rand::*` "does not refer to a reachable function" lines are workspace
  `clippy.toml` config notes for a crate that does not depend on `rand`, not lint
  failures — clippy exits 0)
- `cargo fmt -p film -- --check` ✓
- `cargo deny check` ✓
- `cargo +nightly-2026-06-16 miri test -p film` ✓ — run with **default Miri
  isolation on** (no `-Zmiri-disable-isolation`), matching the reviewer/CI
  invocation. The default build has **zero `unsafe`**, so Miri validates the pure
  logic; proptest cases drop to 16 under `cfg!(miri)`, and the proptest config
  sets `failure_persistence = None` under `cfg!(miri)` (its default regressions
  file needs `getcwd`, which Miri isolation blocks — leaving it on made
  `plan_proptest` red, the round-1 finding). The one test that *executes*
  `blake3` carries `#[cfg_attr(miri, ignore)]` because blake3's SIMD/FFI path is
  not Miri-interpretable on aarch64 — an implementation detail of the hash, not
  of film's logic; everything else (lib unit + `plan_proptest` + `projector_mock`
  + `artifact_roundtrip`) runs and passes under Miri.

The `film demo` binary was driven end-to-end and is byte-for-byte deterministic
across runs (same contact-sheet blake3 on repeat).

## Gate 1 mapping (task 87 §Acceptance gates)

| Gate-1 requirement | Where |
|---|---|
| `FilmPlan` derivation proptested (≥256), gaps/chunks/stride | `tests/plan_proptest.rs` |
| header-verification rejects every corruption class | `billboard.rs` unit tests + `tests/projector_mock.rs` (`WrongFrame`, `BadMagic` → hard error) |
| projector end-to-end vs mock + fake renderer, 3-frame clip → 3 correct frames | `tests/projector_mock.rs::three_frame_clip_round_trips_to_three_correct_frames` |
| PPM + contact-sheet writers golden-tested | `output.rs` unit tests (byte-exact) |
| all with no ROM/core | entire default + `--all-features` laptop path |

Extra coverage beyond the letter of the gate: drop-recovery (`with_read_drops`),
short-landing → `ShortRun` (`with_stop_short_at`), chunked-read reassembly
through the projector, hash-neutrality of billboard reads, a 300-frame clip
(box-gate shape), plan/bundle JSON round-trips, and invalid-plan rejection.

## Round-1 PR review fixes (applied)

The foreman's round-1 read + blind cross-model pass raised four [blocking]
findings, all fixed:

1. **`--core-replay` must not fall back to fake frames** (gate-integrity). An
   explicit `--core-replay` that cannot be honored (binary built without the
   `core-replay` feature, or `HARMONY_SMB_CORE`/`HARMONY_SMB_ROM` unset) is now a
   hard **error** (non-zero exit), never a silent `StampRenderer` producing
   plausible-but-wrong committed hashes. The stamp renderer stays the default only
   when the flag is absent. Locked by a bin test.
2. **Rule-4 gpa overflow.** `validate_plan` now `checked_add`-guards
   `billboard.gpa + len` (`FilmError::InvalidPlan`), and `BillboardWindow::chunks`
   is total on any untrusted window (an overflowing `gpa + len` yields no chunks,
   like a zero cap) so `read_chunks()` never panics on the inner `gpa + done`.
   Regression: a proptest that `read_chunks()` is total on an arbitrary hand-built
   plan, plus unit/projector cases.
3. **`GET_CAN_DUPE` answers `false`.** `render()` clears the captured frame before
   each `retro_run`, so a legal dupe frame (`video_refresh(NULL, …)`) would become
   a spurious "no frame" error. A screenshot frontend gains nothing from duping, so
   the core is now obliged to hand real pixels every run.
4. **Red Miri gate fixed.** `plan_proptest` failed under the *documented* plain
   invocation (`cargo +nightly-2026-06-16 miri test -p film`, isolation on) with
   `getcwd` unavailable — proptest's default file failure-persistence needs it.
   Fixed by `failure_persistence = None` under `cfg!(miri)`; the IMPLEMENTATION
   Miri claim is corrected to the plain invocation and re-verified (the earlier
   green run had used `-Zmiri-disable-isolation`, which the doc didn't state).

## Adversarial-review hardening (applied earlier, pre-PR)

A cross-context review pass surfaced four findings, all addressed:

1. **rule-4 hang** — `FilmPlan::read_chunks()` looped forever on `read_cap == 0`
   for a plan reached outside `derive` (deserialized / hand-built, all fields
   `pub`). Fixed two ways: `chunks()` returns no chunks on a zero cap, and
   `film()` re-validates the plan at entry (`FilmError::InvalidPlan`). Both tested.
2. **untested `ShortRun`** — added `MockBillboardServer::with_stop_short_at` (a
   scripted crash before the requested frame) and a test asserting the projector
   surfaces `FilmError::ShortRun` rather than fabricating a frame.
3. **minimal `env_cb`** — flagged to the foreman below (the most likely box-gate
   friction point); documented, not a code change.
4. **overlapping regions** — `BillboardHeader::parse` now rejects a savestate /
   work-RAM overlap (`HeaderError::RegionOverlap`), closing the "reject
   corruption" gap; the mock `read` window-end is `checked_add` (total on any
   `gpa`).

## Deviations considered and rejected

- **Hand-written PPU compositor** (reconstruct frames from raw VRAM/OAM/palette).
  Rejected by the integrator (2026-07-07): a reconstruction is approximate by
  nature (mid-frame raster effects, palette interpretation), and an investigator
  shown an approximated frame can reach a wrong conclusion. `CoreReplay` renders
  with **zero interpretation of pixels** — 1:1 by construction. This also deleted
  the largest would-be block of code in the crate. A measured-fidelity
  cross-check was considered and is strictly worse than 1:1-by-construction.
- **Re-running the reproducer with the emulator's rendering on.** Forbidden by
  the one-reproducer rule (`docs/LAYERS.md` constraint 1): extra render
  instructions change the guest stream, so `Moment`s and every `state_hash`
  diverge and the filmed replay is no longer the found one. Film reads the
  always-on billboard instead.
- **`zerocopy` for the billboard header.** Chose a hand-rolled little-endian
  encode/parse: byte-exact control, no alignment/padding surprises, panic-free
  bounds checks proven in one place, one fewer dependency in play.
- **`libloading` for the core.** Not on the rule-5 whitelist; used
  `libc::dlopen`/`dlsym` (whitelisted), which also matches the play-agent's own
  FFI posture.
- **A separate mock in `resolution`.** `resolution::MockServer` serves scripted
  *pure-function* memory; film needs *structured billboard bytes*, so it owns a
  `MockBillboardServer` implementing the same public `Server` seam.

## The `unsafe` / Miri story (the named grant)

The only `unsafe` in the crate is the libretro C-ABI FFI in `core_replay.rs`,
behind the `core-replay` feature and the `FrameRenderer` seam. Consequences:

- The **default build carries no `unsafe`** — Miri runs the whole default surface
  clean (the FFI cannot be interpreted; it is feature-excluded, matching the
  conventions' Miri carve-out for privileged/FFI paths behind a seam).
- The **pure** parts of the renderer — the pixel-format conversions
  (`0RGB1555`/`RGB565`/`XRGB8888` → RGB24) and the joypad→libretro-input mapping
  — are `unsafe`-free functions with unit tests that run under `--all-features`.
- Every `unsafe` block has a `// SAFETY:` note; the raw work is confined to
  `dlopen`/`dlsym`/`transmute`-to-fn-pointer and the `retro_*` calls.

Per the `unsafe`⇒Miri review bar: run `cargo +nightly-2026-06-16 miri test -p
film` (default features) — done here, clean. Adding `film` to the nightly Miri
job's `-p` list is a one-line CI edit for the integrator (I do not edit workflow
files; conventions rule 1).

## What the integrator must reconcile when 80 / 82 / 86 merge

Film codes against three sibling wire contracts that are **unmerged on this
branch**; it models them locally (rule 2), exactly as task 82 does. When they
land:

1. **The billboard header layout** (`billboard.rs`) is film's *local definition*
   of task 86's producer format. The byte layout is documented at the top of
   `billboard.rs` (32-byte header: magic `HBBD`, u16 version, u16 flags, u32
   frame, u8 joypad, 3 reserved, then two `(offset,len)` region pairs for
   savestate + work RAM). **Task 86's play-agent must emit this exact layout**,
   or the integrator reconciles the two onto one definition. Nothing observable
   in film depends on which side owns the constant.
2. **The joypad byte bit layout** (`core_replay.rs`, `joypad_pressed`): film
   assumes NES hardware order — bit0=A, 1=B, 2=Select, 3=Start, 4=Up, 5=Down,
   6=Left, 7=Right — and maps each bit to the corresponding libretro
   `RETRO_DEVICE_ID_JOYPAD_*`. If task 86 captures the joypad byte in a different
   order, either align the producer or adjust this one mapping function (it is
   the whole of film's input interpretation).
3. **`read`/`regs` verbs + `RegsView`/`Server`**: film rides `resolution`'s
   client, which already models these locally and flags its own collapse onto the
   real `control-proto` surface (task 82's IMPLEMENTATION). Film needs no extra
   work beyond resolution's reconciliation; the `film` crate re-exports
   `resolution`/`environment` types for consumer convenience.

## The box gate (handed to the foreman — task 87 §gate 2)

One live scenario on real KVM, pinned per `docs/BOX-PINNING.md`, box-safety per
task 86's section (leave KVM on stock **1396736** + verify after any patched
run). The box builds guest and renderer from the **identical core pin on the
identical x86_64 Linux platform**, which is what makes savestate loading a
non-issue there.

Shape:

- **Capture** — the foreman's real socket `Server` adapter (post-80/82) + a
  ≥300-frame `FilmPlan` over task 86's deep reproducer, run through
  `film::film()` → a `CaptureBundle` (JSON).
- **Render** — `HARMONY_SMB_CORE=<pinned core.so> HARMONY_SMB_ROM=<dump> film
  render --bundle bundle.json --out-dir frames/ --core-replay --contact-cols N`.
- Assert (a) every frame renders through `CoreReplay` (zero header mismatches,
  zero unserialize failures) and the contact sheet is produced (commit the blake3
  hashes; attach the sheet to the report); (b) **render determinism** — the same
  bundle rendered twice is byte-identical (`film render` twice → identical
  `blake3`); (c) **hash-neutrality** — the filmed replay's terminal `state_hash`
  equals the unfilmed replay's, same seed, **25/25** (the projector sends only
  observation/navigation verbs, so this is the one-timeline claim proven, not
  asserted).

### CoreReplay caveats for the box run

- **Env services.** The frontend `env_cb` services `SET_PIXEL_FORMAT` and
  `GET_CAN_DUPE` and refuses everything else — sufficient for a simple NROM game.
  If the pinned core demands more (system/save directory, core variables), extend
  `env_cb`; a rejected required service surfaces as a load-time
  `RenderError::Unavailable`, not a silent wrong frame.
- **Geometry.** `CoreReplay` pins the frame size to the core's `av_info` base
  geometry and rejects any `retro_run` frame that differs
  (`RenderError::CoreGeometry`). An overscan-cropping core may want the max
  geometry instead — a one-line change if the box run trips it.
- **Threading.** The libretro callbacks reach a per-thread context (libretro
  passes no user-data pointer); render a clip on one thread (film does).
- **SKIP discipline.** `CoreReplay::from_env` returns `Ok(None)` (a loud SKIP)
  when `HARMONY_SMB_CORE`/`HARMONY_SMB_ROM` is unset — a skipped gate is not a
  green gate.

## Known limitations

- **CoreReplay is untested on the laptop** (no core, no ROM): its FFI compiles
  and its pure helpers are tested, but the live path is entirely box-verified. It
  is written to the libretro ABI from the spec, not exercised end-to-end here.
- **Cross-platform savestate portability is not gated** (non-goal): same-pin
  same-platform (the box) is the gated path; a Mac loading box-produced
  savestates is best-effort and undocumented-by-design.
- **No viewer UI / no video encoding in-repo** (non-goals): PPM out, ffmpeg
  outside (README one-liner). The natural embedding is task 83's findings page.

## Dependencies (rule 5 + the task's explicit grant)

`control-proto`, `environment`, `resolution` (as the client library,
`default-features = false` so the lib pulls no clap) — the three hard-rule-2
exceptions the task grants; **no dependency on `explorer`**. `thiserror`, `serde`
+ `serde_json`, `blake3` (whitelist); `clap` (bins-only, `cli` feature); `libc`
(`core-replay` feature only, for `dlopen`). Dev: `proptest`, `tempfile`.

## Cross-directory edit (declared)

Per the task's explicit naming instruction ("`film` enters `docs/GLOSSARY.md`
(Adopted vocabulary) in this task's implementation PR — the name-when-built
discipline"), this branch adds one row to `docs/GLOSSARY.md`'s Adopted
vocabulary table. Everything else is under `dissonance/film/`.
