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
`b132fd25add46f816e04be64c434386743b84b8b` (2026-01-31). The offline emulator
image is `tilt_no_network_unrom_(E).nes`, built with `-DNO_NETWORK` and
`-DMAPPER_UNROM` (mapper 2), SHA-256
`6f80d56ce0b242a4faceafafea321feb1c364ab8e7937646e8580ae9289a4ec3`.

From the repository root on Linux x86-64:

```sh
sudo apt-get install --yes build-essential curl git unzip python3 python3-pil ffmpeg jq
dissonance/scripts/build-stb-rom.sh dissonance/stb-build
scripts/build-quicknes-core.sh dissonance/stb-build/quicknes_libretro.so
```

[`stb-versions.env`](../../../stb-versions.env) pins the game and XA source
commits, the 6502 GCC release archive and its checksum, and the expected ROM
checksum. The build fetches those exact inputs and fails if the output ROM
changes. It selects upstream's `SKIP_RESCUE_IMG=2` option: the offline UNROM
image is assembled normally, while unused Rainbow rescue compression is
skipped. Huffmunch is therefore unnecessary for this recipe. No gameplay
source is patched. Upstream's generic `python` interpreter spelling is bound
to Python 3 inside the temporary build directory.

The compiler release is Linux x86-64. On another host use a `linux/amd64`
container with the repository mounted and the same packages/commands. The
initial macOS trial used an Ubuntu 24.04 container to build the same ROM and
a native QuickNES core to execute it. QuickNES revision
`26bb785c9deddb66a17717b21bb4e328f03ade32` is pinned by the shared core script;
the core binary digest is measured on each host and included in campaign
identity. The initial macOS core digest was
`47cb5d0872e4a293c81de5dc0b5c975bf389514f4fc15075fdba4d172eff83b4`.

Build outputs are ignored under `dissonance/stb-build/`. ROMs and cores are
local/build inputs; the CI upload includes compact evidence and the
[artifact notice](../../../STB-ARTIFACT-LICENSE.md), upstream license, and
in-game credits, without binaries or snapshots.

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
plus a 180-frame neutral input tail, and its rendered endpoint matches the
headless endpoint. The earlier `pilot-2000/` directory is retained as
pre-fix evidence and is not the qualified result.

The Dissonance format, build, clippy, test, and dependency checks use the
standalone workspace commands in `CONTRIBUTING.md`. The trial establishes the
pinned direct QuickNES backend and declared local-AI mode; it does not establish
many-core, hours-long, alternate-backend, or host-resource-sweep behavior.

## Continuous evaluation

[The Super Tilt Bro workflow](../../../../.github/workflows/stb.yml) follows
Nova's source-build/search/film pattern and additionally gates full recorded
campaign replay. Relevant pull requests run seed 1; scheduled and manual runs
use the registered seeds 1, 2, and 3. Each runs 2,000 executions on two workers,
with ordinary admission, the existing fixed input/key policy, and a 30-minute
job timeout. The correctness probe checks controls, autonomous opposition,
and restored continuations before search. The campaign compares live/replayed
reports and checkpoint bytes and verifies the headless/rendered endpoint.

CI gates execution count, replay consistency, and usable evidence. Victory
counts and first-win executions are reported without a minimum or fixed target;
an unsolved run remains useful search evaluation. CI does not change difficulty,
seed selection, or policy to recover a win. These small runs qualify an adapter
and expose search behavior; they are not a scalability benchmark.

Artifacts are retained for 30 days: summary and progress, full champion input
and observation, campaign/replay reports, control probe, the explicitly labeled
600-frame film prefix (plus neutral tail), and source/credit notices with
checksums. ROMs, core/compiler binaries, snapshots and raw media are excluded.
Scheduled runs become active after the workflow reaches the default branch.

To reproduce one CI campaign locally after the source build, from the repository
root:

```sh
cargo build --locked --release --manifest-path dissonance/Cargo.toml \
  --bin stb-probe --bin stb-campaign
HARMONY_QUICKNES_CORE=dissonance/stb-build/quicknes_libretro.so \
HARMONY_STB_CORRECTNESS=1 \
  dissonance/target/release/stb-probe dissonance/stb-build/stb.nes
dissonance/target/release/stb-campaign \
  --core dissonance/stb-build/quicknes_libretro.so \
  --rom dissonance/stb-build/stb.nes --output dissonance/stb-artifact \
  --seed 1 --executions 2000 --workers 2 --action-limit 512 \
  --fixed-execution-soak
```
