# Task 86 — the real-game workload: Super Mario Bros. under the fault-free exploration gate

> **STRATEGY AMENDMENT (2026-07-12).** M0 remains live and deliberately narrow: guest/core
> provisioning, billboard, film, boot determinism, and workload evidence. It makes no search-policy
> or cell-quality claim and must not patch `LinkSensor` to manufacture one. M1's `LinkSensor` /
> `LINK_STATE_CHANNEL` cell key and task-70 selector referendum are superseded. The held-out SMB
> evaluation resumes only after `hm-bbx` and rewritten `hm-cs5`, using normalized SDK evidence,
> Differential-materialized cells at actual seals, and the simple archive-guided configuration
> versus pure random. Any later selector or Portfolio becomes an additional frozen comparison, not
> a prerequisite for M0 or the first held-out gate.

## Live contract

M0 is owned by Bead `hm-ahb` and contains only:

- the pinned libretro core and SMB guest-image/workload provisioning;
- the deterministic play agent, RAM-map decoding, unconditional billboard, and legibility events;
- boot and same-seed execution determinism on the box;
- the re-homed film gate: load one real billboard savestate into the same pinned core, run/render a
  real clip of at least 300 frames, prove same-input render identity, and prove film observation is
  hash-neutral 25/25;
- evidence capture under the existing baseline solely to inspect the workload, with no claim about
  archive cells, selector quality, or search advantage.

M0 retains these hard safety and portability requirements from the original specification:

- The copyrighted ROM is never committed, vendored, or fetched. The image reads the user-supplied
  `HARMONY_SMB_ROM`; its SHA-256 is recorded. Without it, ROM-independent builds/tests run and every
  ROM gate reports **SKIP loudly**—skip is not green.
- The libretro core is fetched from a documented commit-pinned upstream under a compatible license;
  it is not vendored. Guest and film renderer use the same pinned core commit.
- `unsafe` is limited to the named libretro C-ABI and billboard `pagemap`/`mlock` seams, each block
  has a `// SAFETY:` justification, privileged/FFI calls sit behind a safe mock seam, and the crate
  runs clean under the repository's pinned Miri command while exercising its unsafe-adjacent logic.
- Portable macOS/Linux tests cover fixed-entropy chord decoding, every RAM-map field on synthetic
  fixtures, billboard round-trip, and deterministic benchmark bookkeeping without a ROM.
- With a ROM on the box, same campaign seed reproduces the per-branch `state_hash` sequence 25/25
  and one recorded deep reproducer's terminal hash 25/25. Solo/co-tenant divergence is a P0 stop.
- Box work follows `docs/BOX-PINNING.md`. After any patched-KVM run, terminate campaign processes,
  wait for zero `kvm_intel` users, restore stock `kvm`/`kvm_intel`, and verify fresh-session module
  size **1396736**. Gates run in the foreground and their results are read before reporting.

The billboard's per-frame libretro savestate is observation data used only for host-side film
rendering. It is never an Explorer restoration state and never competes with consonance snapshots
or a `Reproducer` as search currency.

M1 is a separate held-out evaluation owned by `hm-2su`, blocked by `hm-bbx`, M0, and the explicit
mechanism-GO decision `hm-yjf` after `hm-cs5`. It compares the frozen simple cooperative
configuration with pure random. SMB does not tune that configuration; a post-inspection retune is
diagnostic or starts a new evaluation split.

## Historical specification — non-normative

**Everything below this line, through end of file, is the superseded combined M0/M1 task-86
specification.** It remains as workload and film rationale. Its LinkSensor, task-70, cell,
measurement, dispatch, acceptance, savestate, and non-goal statements are not implementation
authority.

> **FRONTIER · the held-out generalization test for the search seam (the Metroid discipline,
> on a real game).** Task 84 proves signal-beats-random on a maze whose depth, size, and
> branching are manifest knobs — a workload *designed* to be beatable, and the fixture task
> 70's selectors are tuned against. This task re-runs the same fault-free gate on a workload
> nobody can tune: a commercial NES game (Super Mario Bros., mapper-0/NROM — the simplest
> real-game start) running under a libretro core as an ordinary supervised Linux guest
> process. If task 70's selector only beats baseline on the maze it was tuned on, we measured
> overfitting, not search — this task is where we find out. It also lands the **billboard** (a
> per-frame, always-on core-state export in guest memory) that task 87 (`film`) consumes to
> make replays visible. Named successors ride the same seams (see Non-goals): NES **Metroid**
> — the direct replication of Antithesis's published experiment, same console and core, only
> a new RAM table and cell key — then **Super Mario World** (SNES: only a new core pin —
> task 87's core-replay renderer is console-agnostic).
>
> **Queued: do not dispatch until task 84 PASSes and task 70 is merged.** Depends on **task 84**
> (the gate definition, the campaign harness through `explorer::Explorer` + `SocketMachine`, and
> the branch-rate numbers that size this workload's budgets), **task 70** (Selector v2/v3 — the
> subject under test), **task 73** (the guest SDK — `state_set`/`entropy_fill`), **task 68**
> (materialization), **task 67/link** (`LinkSensor`), **tasks 58/60** (control server + the
> campaign/guest-workload-init pattern). The spine (`explorer`/`link`/`harmony-sdk`) is
> **read-only**, exactly as in task 84.
>
> **AMENDED — integrator directive (Paul, 2026-07-09): split the dispatch.** Task 69 M2 ruled
> NO-GO, so task 70 does not exist to test, and the standing directive is now *maximize testing
> on actual workloads, with visual inspection of what works*. Therefore:
> - **M0 (workload bring-up) dispatches IMMEDIATELY:** the guest image + core provisioning +
>   billboard + workload-init, the boot/determinism gates, and campaign runs under the
>   *existing* default/baseline search (no selector claim made or needed). M0 additionally
>   absorbs the **film crate's re-homed live gate** (integrator ruling on the film PR #87
>   merge, same date): libretro core loads in the box guest, one real `unserialize` +
>   `retro_run` validates film's `env_cb` assumption, a ≥300-frame clip renders from a real
>   recorded campaign, render-determinism (same input twice → bit-identical frames), and
>   25/25 hash-neutrality (film observation on/off → identical `state_hash`). Film (merged
>   portable-approved) is M0's visualization consumer — campaign runs become visually
>   inspectable contact sheets.
> - **M1 (the selector referendum — everything below inheriting task 84's gate definitions)
>   stays queued** behind a selector artifact (task 70's successor under the post-NO-GO
>   sequencing; per the E-fails doctrine the signal/CellFn iterates first). The gate
>   definitions in this spec are unchanged — only their dispatch timing moved.
> - The ROM discipline below is unchanged: no ROM in the repo; `HARMONY_SMB_ROM` is
>   user-supplied; absent ROM ⇒ gates SKIP loudly, and M0's ROM-independent surface (core
>   provisioning, billboard plumbing, image scaffolding) still builds and gates.

Read first: `tasks/00-CONVENTIONS.md`, `docs/GLOSSARY.md` (binding register — reproducer,
rollout, campaign, `Moment`/`Span`), `docs/LAYERS.md` (R-L1, the R-L2 thin-SDK corollary, and
the **one-reproducer constraint** — it drives the billboard design), `docs/EXPLORATION.md` (the
`quiet` tactic arm, rows E/F), `tasks/84-exploration-gate.md` (the gate this re-runs — its
definitions of budget, baseline, and report are inherited, not restated), `tasks/70-selector-bandit.md`
(whose output this tests), `tasks/87-film.md` (the billboard's consumer),
`guest/flow-agent/` (the guest-agent pattern), `guest/linux/pg-init.sh` (workload-init
conventions), `dissonance/link/src/sensor.rs` (`LinkSensor`, `LINK_STATE_CHANNEL`),
`dissonance/explorer/src/stads.rs` (discovery-curve estimator).

## Environment

Portable-logic surface: the chord input policy, the RAM-map decode, the billboard layout, and
the benchmark extensions are pure and macOS+Linux-testable against a **mock core** (a fake
`retro_run` + synthetic console RAM) — no ROM and no emulator anywhere in portable tests. The
campaigns are **box-only** (patched KVM, the Linux guest image carrying the core + ROM +
play-agent). Pin per `docs/BOX-PINNING.md`; always revert KVM to stock **1396736** and verify
after any patched run (see Box-safety).

**ROM provisioning (hard requirement).** The SMB ROM is copyrighted and is **never committed,
vendored, or fetched by any script in this repo**. The image build reads it from
`HARMONY_SMB_ROM=<path>` (user-supplied dump); when unset, the image builds without the game
workload and every gate below reports **SKIP loudly** (a skipped gate is not a green gate). The
report records the ROM's sha256 so results are comparable across runs of the same dump.

**Core provisioning.** The libretro NES core is fetched at image-build time from a
**commit-pinned upstream** (like the kernel), never vendored. Core choice is the implementer's,
documented in `IMPLEMENTATION.md` with the trade-off: QuickNES (LGPL-2.1, fastest — throughput
matters at box branch rates), FCEUmm (GPL-2), Mesen (GPL-3, most accurate). Accuracy is a soft
concern — determinism comes from the VM below, not the core, and SMB on mapper 0 is the
most-exercised game in any core's test suite. A build-time patch file against the pinned core
(≤ ~100 lines) is permitted; copying core source into the repo is not. Same no-copy discipline
as bedrock.

Surface list (frontier waiver of hard rule 1):

- `guest/play-agent/` — the new workload agent (beside `guest/flow-agent/`), plus its
  `guest/linux/` init wiring per the task-60 workload-init conventions.
- `dissonance/benchmark` — extend (do **not** fork) with the SMB report configuration; the
  measures (distinct cells, depth, medians + IQR, STADS) are task 84's, reused.
- `consonance/vmm-core` — campaign manifest/config wiring only; the campaign path through the
  composed engine is task 84's, reused.
- `dissonance/explorer`, `dissonance/link`, `guest/sdk`: **read-only.** A spine defect this
  surfaces is a finding to escalate, not a change to smuggle in.

**`unsafe` grant (named):** the libretro C-ABI FFI seam in `play-agent`, and the
pagemap/`mlock` calls the billboard needs. Every block gets `// SAFETY:`; the agent's decision
and decode logic sits behind a mock-core seam so the portable tests never cross the FFI.

## Context

Task 84 imports the Metroid discipline: exploration quality is measurable, decoupled from fault
quality, faults-off (`FaultPolicy::none()`, buggify off — the `quiet` arm). But its maze
manifest is tuned until the gate is passable, and task 70 then tunes selectors against that
fixture. A held-out workload is the standard remedy, and a real game is the best available one:
its difficulty is fixed by Nintendo, its state space rewards search over luck by construction
(pits and enemies make random input plateau within screens; branching from a deep archive entry
makes progress roughly linear in depth), and progress is legible to humans — which is what
makes it the demo, not just the test.

SMB is the deliberately *simple* first game: one background layer, no mapper state (NROM),
levels that scroll one way (absolute X is monotone within a level — a clean progress signal),
and the best-commented disassembly in existence (SMBDIS.ASM) as the RAM-map ground truth. It
still has real discovery structure: pits and enemies plateau random input within the first
screens, while hidden warp zones, vines, and pipe rooms mean deep cells can be *discovered*,
not just ground toward. Metroid — the exploration-shaped game and the Antithesis replication
target — is the named successor once this pipeline is proven, not the starting point.

The whole-VM determinism makes the emulator's own behavior a non-issue: the game's frame-driven
RNG, the core's scheduling, everything is a pure function of the campaign seed because
everything in the guest is. No emulator savestates are ever used — the archive is the only
state currency, and a "checkpoint at World 1-2 with a mushroom" is just an admitted cell.

## The play-agent (`guest/play-agent/`)

A single supervised process: a minimal headless libretro frontend linking the pinned core.
Null audio/video callbacks, unthrottled — it calls `retro_run` in a loop as fast as the box
allows, and **its own `retro_run` counter is the frame clock**. Per frame (vblank) it:

1. **Draws inputs.** One byte of decision entropy via `Sdk::entropy_fill` per **input window**
   (a run of `W` frames, manifest parameter, suggested 8–24), decoded against a **weighted
   chord alphabet** (manifest): e.g. `RIGHT`, `RIGHT+B` (run), `RIGHT+A` (jump), `RIGHT+A+B`
   (run-jump), `A` (neutral jump), `LEFT`, `DOWN` (duck/pipe entry), neutral. Jump height
   rides the hold window (A held across a window = full jump). Weights bias rightward (SMB
   only scrolls right). `START`/`SELECT` are excluded (pausing burns budget). Per-frame
   uniform buttons is a known-bad policy (a random walk); the chord window is what makes the
   entropy stream mean something. Alphabet, weights, and `W` are manifest parameters — tuning
   *them* is legitimate (input shaping), tuning the game is impossible, which is the point.
2. **Emits state registers** (once per window, via `state_set` — the R-L2 thin-SDK shape; the
   host owns cell interpretation): `REG_GAME_MODE` (OperMode), `REG_WORLD`, `REG_LEVEL`,
   `REG_X_BUCKET` (absolute X = page·256 + on-screen X, bucketed ~128–256 px), `REG_POWERUP`,
   `REG_DEPTH` (furthest `(world, level)` ordinal reached — the depth metric; warp zones make
   it jump, which is legitimate discovered progress), and `REG_FRAME` every vblank (the frame
   clock task 87 addresses film frames by). Addresses come from the SMB disassembly /
   datacrystal RAM map (OperMode `$0770`, world `$075F`, player X `$0086` + page `$006D`,
   powerup `$0756`, lives `$075A`, coins `$075E`, etc. — **the implementer verifies every
   address against SMBDIS.ASM and unit-tests the decode against synthetic RAM fixtures**; the
   level-number and player-state addresses especially).
3. **Publishes the billboard** (task 87's enabling seam — built here because it must be part
   of the workload from the first recorded reproducer). Each vblank, *before* the frame's
   `retro_run`, the agent writes into one pinned guest buffer: a self-describing header
   (magic, layout version, the frame counter, **the frame's joypad byte**, region
   offsets/lengths) + the core's full savestate (`retro_serialize`, ~20–32 KiB for NES
   cores) + the 2 KiB console work RAM (cheap, and a stable window for ad-hoc RAM inspection
   by resolution clients). ~25–35 KiB per frame. The savestate is what makes task 87's
   frames **1:1 by construction** (integrator fidelity ruling, 2026-07-07): film re-renders
   each frame by loading the savestate into the *same commit-pinned core* host-side and
   running exactly one frame with the recorded joypad byte — the picture is the core's own
   rendering, not a reconstruction. The buffer's guest-physical address is published once at
   init via state registers — either a hugetlb mapping (one contiguous gpa) or a page-0 gpa
   table for a scattered buffer; implementer's choice, documented. **The billboard is
   unconditional** — always on, filmed or not — because the one-reproducer rule forbids a
   "render mode" that would fork the timeline; a ~30 KiB/frame serialize+memcpy is noise at
   emulation speeds and is simply part of the workload. (Savestate portability: the guest
   image and task 87's host renderer use the **same pinned core commit**; the box gate runs
   both as identical x86_64 Linux builds — cross-platform savestate loading is best-effort
   and documented, never gated.)
4. **Marks legibility events**: `assert_reachable` on first flagpole (any level cleared) and
   on one named deep goal — reaching any world ≥ 2 (by castle *or* by warp zone; both are
   real). Markers, not bugs — zero fault vocabulary, exactly as task 84 rules.

## The cell key (host-side, retunable)

`LinkSensor` turns the registers into `(Moment, Feature)` on `LINK_STATE_CHANNEL`; the
campaign's `CellFn` keys cells on **(game mode, world, level, x-bucket)** — the analog of
Antithesis's discretized `(x, y)` tuple. Depth = `REG_DEPTH`. Because interpretation is
host-side, retuning the key (adding powerup, coarsening x) needs no guest rebuild — and one
documented retune round is the sanctioned first response to a FAIL (below).

If task 70 shipped the multi-objective archive preference (the "prefer more missiles" gap
R-L2 logs), run one **diagnostic** configuration preferring `REG_POWERUP` at equal cells —
reported as a column, not part of the pass condition.

## The measurement

Three configurations, identical branch budget (sized from task 84's measured branch rates),
**≥20 seeds each**, medians + IQR, STADS discovery curve + exhaustion signal for the signal
configuration — the task 84 report machinery, reused:

- **Signal** — task 70's winning selector configuration, exactly as it shipped. No retuning
  the selector against this workload (that would un-hold-out the test).
- **Pure-random baseline** (primary, the pass/fail line) — independent seeds, no archive
  branching; task 84's ruling inherited.
- **Selector v1** (attribution column) — the task-84-era default selector. Separates "the
  archive helps at all" from "task 70's improvements transfer"; a signal-beats-random win
  with v1 ≈ v2/v3 is itself a finding about 70.

Non-vacuity is documented empirically, per task 84's discipline: the random baseline's plateau
(distinct cells and max depth) goes in `IMPLEMENTATION.md`, so a win cannot be claimed against
a saturated or broken control.

## Acceptance gates

1. **Portable (macOS + Linux):** play-agent logic against the mock core — chord decode from a
   fixed entropy stream reproduces a fixed input tape; RAM-map decode against synthetic RAM
   fixtures (every register); billboard header/layout round-trips; benchmark extensions
   proptested (≥256). Standard suite green on every touched crate. All of this runs with no
   ROM present.
2. **Box gate — determinism:** with ROM present, the campaign replays bit-identically — same
   campaign seed ⇒ identical per-branch `state_hash` sequence **25/25**; one deep reproducer's
   terminal `state_hash` **25/25**. Co-tenancy discipline per `docs/BOX-PINNING.md` (solo vs
   co-tenant divergence = P0 STOP + escalate).
3. **Box gate — the measurement:** a committed `dissonance/benchmark/SMB-EXPLORATION-REPORT.md`
   over the three configurations as specified above, ROM sha256 recorded, faults-off recorded
   (`FaultPolicy::none()`, buggify off — the `quiet` arm).
4. **The verdict:** **PASS** = signal strictly beats pure-random on **both** distinct cells and
   depth (greater medians, non-overlapping IQRs), against a demonstrably live control. A
   **FAIL** routes first to one documented host-side cell-key retune (in-surface); a
   persisting FAIL is a **generalization finding about task 70** — escalate the report to the
   integrator. It is *not* a selector patch in this task, and *never* a workload nerf (no ROM
   hacks, no easier game). A FAIL here is a publishable result, not a blocked task.

## Box-safety (CRITICAL)

Stock KVM = **1396736**; the patched module is larger. ALWAYS leave the box on stock +
verified after every run: `pkill -9 -f` the campaign bin (and any `live_*`) FIRST → wait
`lsmod | grep '^kvm_intel'` users=0 → `rmmod kvm_intel kvm; modprobe kvm; modprobe kvm_intel`
→ verify size 1396736 on a FRESH ssh connection. SSH drops (exit 255) on pkill/rmmod are
normal — reconnect + verify. Pin builds/tests to a leased core (`taskset -c`,
`docs/BOX-PINNING.md`). Run gates in the foreground and READ results before reporting.

## Non-goals

- **Selector/search work** — task 70 (or its successor, if this FAILs) owns it. This task
  measures; it does not improve.
- **Any fault vocabulary** — `quiet` arm throughout, exactly as task 84.
- **Fattening the SDK** — `state_set`/`entropy_fill`/`assert_reachable` only; the host owns
  cells (R-L2).
- **In-guest rendering or frame export** — no render mode, ever (one-reproducer rule). Making
  the game *visible* is task 87's job, over the billboard, host-side.
- **Emulator savestates** — never; the archive is the only state currency.
- **Vendoring the ROM or the core**, or any PPU-accuracy work in the core.
- **Multi-objective preference implementation** — 70's design input; only its diagnostic
  column runs here, and only if 70 shipped it.
- **Other games/consoles** — per-game pieces (RAM table, chord weights, cell key) are data
  behind seams built here. Named successors, each its own task: NES **Metroid** (the
  Antithesis replication — same core, new table/key) and SNES **Super Mario World** (only a
  new core pin — task 87's core-replay renderer carries over unchanged).
