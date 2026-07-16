# play-agent — implementation notes (task 86 M0)

The in-guest SMB workload agent: a minimal headless libretro frontend run as
the game image's single supervised process. The portable brain (`src/lib.rs`)
is unsafe-free and fully tested against the mock core; the binary
(`src/main.rs`) carries the named-`unsafe` Linux glue. See the root
`docs/history/IMPLEMENTATION-task86.md` for the whole-task record and the box handoff.

## Core choice: FCEUmm, GPL-2.0-or-later (pinned in `guest/linux/versions.lock`)

**Round-3 P1 re-pin.** The first pin was QuickNES (the spec table's "fastest"
row, labeled LGPL-2.1 there), but the pinned `libretro/QuickNES_Core` release
is a licensing mix — an LGPL-2.1+ Nes_Emu core under a **bare GPL-2 top-level
LICENSE with an unheadered `libretro.cpp` glue** — whose safe reading is
GPL-2.0-only, which is **incompatible with AGPL-3**: the AGPL-3 play-agent
dlopens the core and both ship inside one initramfs artifact (a combined work
cargo-deny cannot see). Re-pinned to **FCEUmm** (`libretro/libretro-fceumm`),
audited per-file at the pinned commit: 490 of 601 `src/` files carry explicit
headers and **every one is or-later** (488 GPL-2.0-or-later + 2
LGPL-2.1-or-later, Blargg's `nes_ntsc`); zero only-versioned files; the
unheadered remainder falls under the repo `Copying` serving those grants. So
the built core is GPL-2.0-or-later, upgradeable to GPL-3, and **AGPL-3 §13
permits conveying the combined work** — the compatibility rationale recorded
beside the sha in `versions.lock` (`FCEUMM_LICENSE=`). Mesen (GPL-3, cleanly
licensed) was the runner-up, rejected on throughput: it is the heaviest of
the three and emulation cost multiplies into every branch under the
single-stepping VMM; FCEUmm is the middle ground and SMB (mapper 0/NROM) is
core-agnostic territory. Accuracy stays a soft concern — determinism comes
from the VM below. The fetch discipline is unchanged (sha256-pinned tarball,
built at image time, **never vendored, never copied from** — the bedrock
rule); no build-time patch was needed at this pin. One FCEUmm-specific note:
its `retro_serialize` requires `size == retro_serialize_size()` exactly,
which is precisely how the agent calls it (the layout freezes that size at
init).

## Billboard gpa publication: one hugetlb page (contiguous by construction)

The spec offered hugetlb (one contiguous gpa) or a page-0 gpa table for a
scattered buffer. Chosen: **one anonymous 2 MiB `MAP_HUGETLB` mapping** —
the ~35 KiB billboard fits with room to spare, the physical extent is
contiguous by construction (film's `BillboardWindow` models one `(gpa, len)`
window), and it removes the gpa-table indirection entirely. The mapping is
faulted in, `mlock`ed, translated once via `/proc/self/pagemap` (root/
`CAP_SYS_ADMIN`, the campaign-super precedent), and published once at init via
`state_set(REG_BILLBOARD_GPA/LEN)` before `setup_complete`, so every branch
inherits the published window. `game-init.sh` reserves the hugepage
(`nr_hugepages=2`).

## Billboard layout: byte-matched to film's merged definition

`src/billboard.rs` is a local mirror of `dissonance/film/src/billboard.rs`
(guest crates cannot depend on `dissonance/`): magic `HBBD`, layout v1, the
32-byte LE header, savestate at offset 32, the 2 KiB work RAM contiguously
after. Two tests pin it: a byte-for-byte comparison against a reproduction of
film's canonical `encode_billboard`, and a hard golden of the 32 header bytes.
The joypad byte is NES hardware shift order (bit 0 = A … bit 7 = Right),
exactly the mapping film's `joypad_pressed` replays. `REG_FRAME` is emitted
**after** the billboard bytes are written and **before** `retro_run`, so the
billboard at that Moment always describes the stamped frame.

## RAM map: verified against SMBDIS.ASM

Every decoded address was verified against the doppelganger disassembly
(quoted in `src/ram.rs`): `OperMode $0770`, `PlayerStatus $0756`,
`NumberofLives $075A`, `LevelNumber $075C` ("the actual dash number" — chosen
over `AreaNumber $0760` so pipe rooms don't perturb the cell key),
`CoinTally $075E`, `WorldNumber $075F`, `Player_PageLoc $6D`,
`Player_X_Position $86`. Synthetic-RAM fixtures unit-test every register.

## Build shape: dynamic glibc (a deliberate divergence from flow-agent)

flow-agent ships fully-static musl; play-agent **cannot** — it `dlopen`s the
core, and static musl binaries do not support `dlopen`. So `build.sh` produces
a dynamic native (glibc) binary and `build-game-image.sh` copies the ldd
closure of **both** the agent and the core `.so` into the rootfs (the
build-postgres-image.sh pattern). The box builds guest + host renderer from
the same pin on the same platform, which is also what makes savestate loading
a non-issue there (cross-platform savestates stay best-effort, ungated).

## Known limitations / notes for the integrator

- **`env_cb` is minimal** (SET_PIXEL_FORMAT accepted, GET_CAN_DUPE=true,
  everything else `false`) — the same shape as film's, and the same expected
  box-bring-up friction: if the pinned QuickNES demands a directory or
  core-variable service at `retro_load_game` time, extend `env_cb` in the
  `retro` module (a mechanical, guest-side-only change).
- **Savestate size stability** is assumed (`retro_serialize_size` queried once
  at init to freeze the billboard layout). QuickNES's savestate is
  fixed-size; a core whose size drifts mid-run would fail the serialize
  (returns `false` → the agent dies loudly, never a torn billboard).
- **The scripted start (round-4 P1)**: from power-on SMB sits on the title
  screen, and the campaign alphabet deliberately excludes `START` (branches
  must explore gameplay inputs, not menu resets) — so `src/start.rs` presses
  `START` on a fixed press/release cadence until the RAM shows gameplay
  (`OperMode == 1`), settles, re-verifies, and only then does the binary
  publish the billboard and signal `setup_complete`: the base seal lands at
  gameplay start. The script draws no entropy (a pure function of power-on;
  the portable test pins that two runs execute identical frames), and failing
  to reach gameplay is a loud error — never a silently-vacuous campaign. The
  box smoke's mandatory vacuity check (root `docs/history/IMPLEMENTATION-task86.md` step
  1) verifies the billboard shows in-gameplay state at the seal point.
- **Depth/marker semantics**: `REG_DEPTH` is the instantaneous
  `world*4+level` ordinal via `state_max` (the host keeps the max);
  `smb_level_cleared` fires once when the ordinal first rises above its value
  at the first gameplay observation; `smb_world_two` when `world >= 1`
  (castle or warp — both real). After a game over SMB resets to 1-1; the
  host-side max keeps the high-water mark.
- **Miri**: the crate's only `unsafe` is the binary's cfg(linux) FFI
  (dlopen/hugetlb/pagemap/doorbell — real syscalls Miri cannot execute,
  cfg-gated off the Miri host). Per the flow-agent precedent (round-2 P1),
  every **decision** those edges depend on is hoisted into `src/glue.rs` —
  libretro callback responses, the joypad-bit mapping (asserted against the
  chord masks), the work-RAM copy/clamp/zero-fill bounds, the hugetlb length
  bound `from_raw_parts_mut` relies on, and the pagemap offset/entry decode —
  all Miri-covered; `main.rs`'s `unsafe` blocks are thin FFI edges whose
  `// SAFETY:` comments cite the glue invariant holding at the call site.
  Proptest failure-persistence is disabled under Miri (its `getcwd` is
  unsupported in isolation).
- The `--smoke` mode (mock core + seeded local xorshift, no hypervisor) is the
  off-box bring-up check and runs anywhere.

## task 103 — the settle loop is bounded by `max_frames` (bead `hm-9wa`, finding 3)

PR #93's round-9 pass: the scripted start could **exceed its own frame bound while settling**.
`run_start_script` ran `settle_frames` unconditionally once gameplay was first observed, so an
observation late in the budget overran `max_frames` — and `frames += 1` past `u32::MAX` was a
theoretical wrap on top of it. `max_frames` is the loud-failure bound on the whole script; a
silent overrun of it is the same class as the round-5 cadence-overflow panic, and the frames a
rollout spends are budget a box gate paid for.

The settle is now spent from the same budget, checked at both ends:

- **Before the first frame** — a script whose `settle_frames + 1` cannot fit under `max_frames`
  can never succeed, so it is `BadScript` up front rather than discovered mid-settle (the box
  would otherwise burn the full press cadence to find out).
- **At the observation** — gameplay observed too late to settle inside the bound is
  `StartError::SettleExceedsBudget { observed_at, settle_frames, max_frames }`, naming the three
  numbers an operator needs to fix it.

`frames <= max_frames` is now an invariant of the loop, so the overflow is gone **by
construction** rather than by checked arithmetic, and `max_frames - frames` cannot wrap.

Both call sites use `StartScript::default()` (`settle_frames: 16`, `max_frames: 1800`), which is
unaffected — the defaults sit far inside the bound, and the real start reaches gameplay in a few
hundred frames. The existing `never_reaching_gameplay_is_loud` fixture (`max_frames: 2`) needed a
1-frame settle to stay a *bound-exhaustion* test rather than an unusable script; its assertion is
unchanged.

Gates: `cargo test` (46 lib + 7 agent) and `cargo clippy --all-targets -D warnings` green on the
host and for `x86_64-unknown-linux-gnu`; `cargo fmt --check`; Miri `--lib` green (46 tests). No
new `unsafe`, no guest behavior change — the start script draws no entropy and presses the same
frames; this only bounds what it is allowed to spend.
