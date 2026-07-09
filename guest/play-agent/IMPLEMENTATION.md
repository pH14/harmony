# play-agent — implementation notes (task 86 M0)

The in-guest SMB workload agent: a minimal headless libretro frontend run as
the game image's single supervised process. The portable brain (`src/lib.rs`)
is unsafe-free and fully tested against the mock core; the binary
(`src/main.rs`) carries the named-`unsafe` Linux glue. See the root
`IMPLEMENTATION-task86.md` for the whole-task record and the box handoff.

## Core choice: QuickNES (pinned in `guest/linux/versions.lock`)

QuickNES over FCEUmm / Mesen for **throughput** — the box runs `retro_run`
under a single-stepping deterministic VMM, so emulation cost multiplies into
every branch; QuickNES is the fastest of the three and SMB (mapper 0 / NROM)
is the most-exercised title in its test suite. Accuracy is a soft concern by
spec: determinism comes from the VM below, not the core. **License note:** the
spec's table says LGPL-2.1; the pinned `libretro/QuickNES_Core` repo's
top-level `LICENSE` is **GPL-2.0** (the embedded Nes_Emu core sources carry
LGPL-2.1 headers; the libretro packaging is GPL-2). Either way the discipline
is the same as bedrock/the kernel: fetched at image-build time from the
sha256-pinned tarball, built, **never vendored, never copied from** — the
repo's own code links nothing of it. No build-time patch was needed at this
pin (the ≤100-line patch allowance is unused).

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
