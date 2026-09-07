<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Super Tilt Bro workload adapter

This module adapts the source-built [Super Tilt Bro](https://github.com/sgadrat/super-tilt-bro)
NES game to Dissonance's target-neutral search interfaces. It owns the
source-labelled memory decoder, ordinary menu setup, controller encoding,
mechanical observations, archive identity, progress milestones, terminal
conditions, and QuickNES replay/rendering seam. The generic coordinator sees
only those interfaces and does not receive a combat strategy.

## Declared workload

The primary workload is a normal local match from power-on setup: stage 0,
four stocks, and the game's built-in Player-B Easy AI (`config_ai_level = 1`).
The setup tape leaves controller B untouched during character selection, so
the opponent remains autonomous. The source's default local mode is recorded
as `mode=local;stocks=4;ai=1;stage=0` in the campaign identity. A match ending
with `game_state = 2` is terminal; `game_winner = 0` is a Player-A victory and
`game_winner = 1` is a defeat. A passive opponent or disabled hazards would be
a reduced fixture and is not used by the primary pilot.

The game starts at a sealed match genesis after title, mode, configuration,
character, and stage screens. Menu traversal is fixed setup; gameplay actions
are the searched input. No save edit, password, imported input, or scripted
combat sequence is used.

## Identity and build

The source is pinned to `sgadrat/super-tilt-bro` commit
`b132fd25add46f816e04be64c434386743b84b8b` (2026-01-31). The selected normal
emulator image is `tilt_no_network_unrom_(E).nes`, built with `-DNO_NETWORK`
and `-DMAPPER_UNROM` (mapper 2), SHA-256
`6f80d56ce0b242a4faceafafea321feb1c364ab8e7937646e8580ae9289a4ec3`.

The reproducible source inputs are the following tool revisions (the
upstream `deps/build-deps.sh` clones moving tips, so it is not sufficient for a
recorded build):

| Tool | Pinned input | Trial verification |
| --- | --- | --- |
| XA fork | `sgadrat/xa65-stb` commit `a75f76dc9aee5b892facecb0436725d368f41102` | `deps/xa65-stb/xa/xa` SHA-256 `14fe3d67ba9e91928eca317823abc991a20ffe9b38511cac40d28fa6262eb6fb` |
| 6502 GCC | `sgadrat/gcc-6502-bits` release asset `v8.4.1-2/gcc-6502.zip` | Linux x86-64 `prefix/bin/6502-gcc` SHA-256 `60e799f2ba4e2f9d0d77f9c2c5044312a1f42ee3be0f02a9a140269c43580114` |
| Huffmunch | `bbbradsmith/huffmunch` commit `dfc0804925f6a3a0309440ec5531e966ef8d7f2c` | `deps/huffmunch/huffmunch` SHA-256 `3f20e675f3b9f7efb9662124e60fd3d0ebb3afb8caa8e91f486390e9e0176682` |

The compiler archive is a Linux x86-64 release, so the trial source build ran
inside an `ubuntu:24.04` container with the source checkout mounted at `/stb`.
The literal trial command (on a Linux arm64/v8 host, Docker selected its
linux/amd64 image) was:

```sh
docker run --rm -v /private/tmp/stb-luna-v1-upstream:/stb ubuntu:24.04 \
  bash -lc 'set -eu
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq >/tmp/apt.log 2>&1
    apt-get install -y -qq build-essential python3 python3-pil python-is-python3 >>/tmp/apt.log 2>&1
    cd /stb
    (cd deps/huffmunch && make clean && make CXX="g++ -Wno-c++11-narrowing")
    (cd deps/xa65-stb/xa && test -x xa)
    deps/gcc-6502-bits/prefix/bin/6502-gcc --version | head -n 1
    XA_BIN=/stb/deps/xa65-stb/xa/xa \
    CC_BIN=/stb/deps/gcc-6502-bits/prefix/bin/6502-gcc \
    HUFFMUNCH_BIN=/stb/deps/huffmunch/huffmunch ./build.sh'
```

For a fresh tool checkout, XA was built in the same image with
`cd /stb/deps/xa65-stb/xa && make clean && make -j2`; the trial then verified
the resulting executable hash above. An explicit `--platform linux/amd64`
can be added when the host architecture should not determine Docker's
selection.

The pinned build inputs can be checked out before entering the container with
these commands (and the compiler release asset downloaded from its exact
release URL):

```sh
git clone https://github.com/sgadrat/super-tilt-bro.git /private/tmp/stb-luna-v1-upstream
git -C /private/tmp/stb-luna-v1-upstream checkout b132fd25add46f816e04be64c434386743b84b8b
cd /private/tmp/stb-luna-v1-upstream
git clone https://github.com/sgadrat/xa65-stb.git deps/xa65-stb
git -C deps/xa65-stb checkout a75f76dc9aee5b892facecb0436725d368f41102
git clone https://github.com/bbbradsmith/huffmunch.git deps/huffmunch
git -C deps/huffmunch checkout dfc0804925f6a3a0309440ec5531e966ef8d7f2c
mkdir -p deps/gcc-6502-bits
curl -L https://github.com/sgadrat/gcc-6502-bits/releases/download/v8.4.1-2/gcc-6502.zip \
  -o deps/gcc-6502-bits/gcc-6502.zip
```

After extracting the release's `prefix/` under `deps/gcc-6502-bits/`, the
build command from a checkout of the pinned source is:

```sh
XA_BIN="$PWD/deps/xa65-stb/xa/xa" \
CC_BIN="$PWD/deps/gcc-6502-bits/prefix/bin/6502-gcc" \
HUFFMUNCH_BIN="$PWD/deps/huffmunch/huffmunch" \
./build.sh
```

The local trial artifact records the source checkout, build log, ROM header,
and ROM digest under `/private/tmp/stb-luna-v1-artifacts/`. The ROM and core
are local inputs and are intentionally not committed to this repository.
The trial uses the pinned QuickNES revision `26bb785c9deddb66a17717b21bb4e328f03ade32`,
with core SHA-256
`47cb5d0872e4a293c81de5dc0b5c975bf389514f4fc15075fdba4d172eff83b4`.

## Observation contract

All addresses below are offsets in QuickNES's 2 KiB system-RAM window and are
source labels from `game/mem_labels.asm` at the pinned revision. Whole-pixel
world coordinates are `pixel + signed(screen/page) * 256`; the separate page
bytes remain observable so a wrapped pixel byte is not mistaken for a fall or
camera transition.

| Field | Address | Meaning and role | Why it is retained |
| --- | ---: | --- | --- |
| `game_state` | `$00d4` | Global state (`0` in-game, `2` game-over) | Terminal/report |
| `game_mode` | `$00e2` | Local/online/arcade/server mode (`0` local) | Genesis identity |
| `ai_level` | `$00da` | Configured opponent level (`1` Easy) | Genesis identity |
| `stage` | `$00db` | Selected versus stage | Location identity |
| `gameplay.fighter states` | `$0000/$0001` | Player-A/Player-B state-machine values | Mechanical observation while gameplay is valid |
| `gameplay.world X/Y` | `$0004..$0007`, `$000e..$0011` | Pixel bytes and signed page components | Paired location identity while gameplay is valid |
| `gameplay.directions` | `$0008/$0009` | Facing direction | Mechanical observation while gameplay is valid |
| `gameplay.damage` | `$0048/$0049` | Player-A/Player-B damage percentage | Progress and preference while gameplay is valid |
| `gameplay.stocks` | `$0054/$0055` | Remaining stocks | Durable progress and preference while gameplay is valid |
| `gameplay.state clocks` | `$0012/$0013` | State-machine clocks | Mechanical observation and event diagnostics while gameplay is valid |
| `gameplay.hitstun` | `$0002/$0003` | Hitstun counters | Mechanical observation and event diagnostics while gameplay is valid |
| `gameplay.grounded/walled` | `$0062/$0063`, `$0064/$0065` | Contact flags | Mechanical observation and event diagnostics while gameplay is valid |
| `game_winner` | `$05db` | Winner byte once game-over is reached | Terminal condition |

The decoder exposes fighter fields as an optional `gameplay` payload. It is
present only when `game_state = 0` and both player state machines are active;
menus and the game-over screen therefore cannot turn reused UI bytes into
resources, locations, or damage. The terminal observation keeps the current
`game_state` and `game_winner`, while its gameplay payload is absent. Each
observation also carries cumulative validated Player-A/Player-B stock-loss
counts. In pinned `game_logic.asm`, a death decrements the counter and checks
for a negative result before the terminal path resets it to zero. A live
zero-stock frame therefore still respawns; the terminal underflow is the
additional final loss. With the declared initial count of four, a completed
match has five observed losses for the player who reaches game-over.

The decoded observation also records the changed RAM indices for an action's
interior boundaries. Progress watermarks fold every decoded boundary so a hit
inside a held chord is retained as evidence; the input tape remains the
reproducible witness for that event.

## Archive and input policy

The archive key (`stb_local_ai_spatial_16_preference_v2`) uses one
representative per location slot. Paired Player-A and
Player-B signed world-coordinate buckets (16 pixels) plus stage and opponent
knockout count provide identity; wider groups pool those locations and then
retain stage/knockout identity. Opponent damage, opponent knockouts, and
Player-A stocks are the objective progress prefix. Player-B stocks and Player-A
damage are same-location capability preferences. Player state-machine numbers,
coordinates outside their documented units, and arbitrary RAM IDs do not rank
progress. Terminal observations have no archive key because their gameplay
payload is phase-invalid; terminal knockout counts come from the validated
observation event instead.

This checkout's generic archive interface currently uses `Ord` for both map
identity and progress walks. `StbArchiveKey::Ord` therefore compares the
objective progress prefix first and uses identity fields only as a deterministic
tie-break. Coordinates consequently retain a residual positional tie bias when
two endpoints have equal objective progress, even though they are not intended
as progress measures. The adapter documents this coupling rather than
pretending that coordinates are progress; a future generic `progress_cmp` hook
would change search semantics and needs a separately versioned fixed-policy
comparison before it can replace this baseline.

`ButtonChord` uses the QuickNES/NES serial layout: A `0x01`, B `0x02`, Select
`0x04`, Start `0x08`, Up `0x10`, Down `0x20`, Left `0x40`, and Right `0x80`.
Super Tilt Bro reverses these bits when polling its source controller byte;
the adapter supplies the generic layout and leaves the source conversion to
the ROM. Search chords combine nine non-conflicting direction states with the
four A/B states. Select is excluded because it has no gameplay action in this
mode; Start is excluded because it pauses the match. Durations are sampled as
short holds of 2--12 frames or long holds of 48--120 frames. The primary
campaign uses ordinary `AdmitAlive` admission, `OneToSix` suffixes, and the
game-neutral `AlphabetOnly` draw mixture. The repaired survival helper is
standalone probe code; `ProbeAtAdmission45` is explicitly rejected because
the primary mode has no demonstrated admission problem.

## Local probe and campaign

Build the binaries from the Dissonance workspace:

```sh
cargo build --locked --manifest-path dissonance/Cargo.toml \
  --bin stb-probe --bin stb-campaign
```

The setup and control probe records the menu-to-genesis trace, positive and
negative controller examples, autonomous opponent movement, snapshot restore,
and the 45-frame standalone survival helper:

```sh
HARMONY_QUICKNES_CORE=/private/tmp/harmony-quicknes-test.dylib \
HARMONY_STB_CORRECTNESS=1 \
dissonance/target/debug/stb-probe \
  /private/tmp/stb-luna-v1-artifacts/tilt_no_network_unrom_E.nes \
  > /private/tmp/stb-luna-v1-artifacts/control-snapshot-probe.jsonl
```

The binary defaults to a 2,000-execution, two-worker pilot; explicit larger
limits remain available for sustained evaluation under the generic campaign
limits. `--fixed-execution-soak` keeps issuing reservations through the
requested cap after a victory; it is useful when measuring the exact budget.
This trial uses only the default worker and execution bounds. All outputs
below belong in an ignored or temporary directory:

```sh
HARMONY_QUICKNES_CORE=/private/tmp/harmony-quicknes-test.dylib \
dissonance/target/debug/stb-campaign \
  --core /private/tmp/harmony-quicknes-test.dylib \
  --rom /private/tmp/stb-luna-v1-artifacts/tilt_no_network_unrom_E.nes \
  --output /private/tmp/stb-luna-v1-artifacts/pilot \
  --seed 1 --executions 2000 --workers 2 --action-limit 512 \
  --fixed-execution-soak
```

The campaign writes `stream.jsonl`, `progress.jsonl`, `campaign-report.json`,
`replay-report.json`, `archive.json`, and `snapshots.bin`; it replays the
stream and whole-tree checkpoint before reporting success. It records the
actual champion (or first verified victory) in `champion-input.json` and
`champion-observation.json`, then renders that same input when it fits the
600-frame excerpt bound. Longer champions are fully replayed headlessly and
rendered from an explicitly recorded prefix in `render-input.json`, with the
rendered endpoint compared to `render-observation.json`. The bounded excerpt
is written to `witness.rgb24`, `witness.s16le`, and `witness.mp4`.
`run-summary.json` records seeds, workers, limits, frames, resource counters,
digests, progress, milestones, full-champion versus rendered-excerpt lengths,
and all artifact paths.

## Qualification status

Execution qualification passed with
`/private/tmp/stb-luna-v1-artifacts/control-snapshot-probe-v2.jsonl`: the
decoder reaches live local-AI genesis, the native opponent advances during
no-op input, all searched controls have positive traces, and snapshot/restore
returns the decoded observation and fingerprint exactly. The retained
transition regression at
`/private/tmp/stb-luna-v1-victory-v4-restore.jsonl` also covers live gameplay,
the phase-invalid interval, and game-over; its terminal frame is 3468 with
`game_state=2`, `game_winner=0`, `gameplay=null`, and cumulative losses
`player_a=4`, `player_b=5`. Reapplying the final action after restoration
reports `observation_exact=true`.

Search qualification passed with the fixed-policy pilot at
`/private/tmp/stb-luna-v1-artifacts/pilot-2000-v2/`: seed 1 completed exactly
2,000 executions on two workers, emulated 296,557 frames, reached the first
victory at execution 803, and recorded 38 victories, both victory and defeat
milestones, and `replay_verified=true`. The same-policy post-cleanup repeat at
`pilot-2000-v3/` produced byte-identical campaign/replay reports, stream,
archive, checkpoint, champion input/observation, and rendered audio/video
artifacts. The verified champion has 72 actions and ends at frame 3630 with
`game_state=2`, `game_winner=0`, no phase-invalid gameplay payload, and
cumulative losses `player_a=4`, `player_b=5`. Its full input is replayed
headlessly; the witness render is an explicitly bounded 600-input-frame prefix
plus a 180-frame terminal tail, and its rendered endpoint matches the
headless endpoint. The earlier `pilot-2000/` directory is retained as
pre-fix evidence and is not the qualified result.

The Dissonance format, build, clippy, test, and dependency checks are covered
by the standalone workspace commands in `CONTRIBUTING.md`; the ROM, source
checkout, and QuickNES core remain local artifacts, so CI does not run this
ROM campaign. Qualification is therefore for the pinned QuickNES backend and
the declared local-AI mode. This bounded trial does not establish
many-core, hours-long, alternate-backend, or host-resource-sweep behavior;
the runner accepts explicit larger limits for later evaluations.
