<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Super Tilt Bro workload adapter

This module adapts the source-built [Super Tilt Bro](https://github.com/sgadrat/super-tilt-bro)
NES game to Dissonance's target-neutral search interfaces. It owns the
source-labelled memory decoder, ordinary menu setup, controller encoding,
mechanical observations, archive identity, progress milestones, terminal
conditions, and QuickNES replay and rendering interface. The generic coordinator
sees only those interfaces and does not receive a combat strategy.

The adapter implements `CampaignTypes`, `TargetExecution`, `InputPolicy`,
`Evaluation`, and `Reporting` in the NES workload package. It retains an
execution override because the shared rollout currently records full requested
holds and requires a candidate at every nonterminal endpoint. STB instead
records a shortened hold at an interior ending and excludes phase-invalid
endpoints from admission while preserving their observations. Scheduling,
archive maintenance, recording, and replay remain owned by Dissonance.

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
workloads/nes/scripts/build-stb-rom.sh workloads/nes/build/stb
scripts/build-quicknes-core.sh workloads/nes/build/stb/quicknes_libretro.so
```

[`stb-versions.env`](../../stb-versions.env) pins the game and XA source
commits, the 6502 GCC release archive and its checksum, and the expected ROM
checksum. The build fetches those exact inputs and fails if the output ROM
changes. It selects upstream's `SKIP_RESCUE_IMG=2` option: the offline UNROM
image is assembled normally, while unused Rainbow rescue compression is
skipped. Huffmunch is therefore unnecessary for this recipe. No gameplay
source is patched. Upstream's generic `python` interpreter spelling is bound
to Python 3 inside the temporary build directory.

The compiler release is Linux x86-64. On another host use a `linux/amd64`
container with the repository mounted and the same packages/commands. QuickNES
revision
`26bb785c9deddb66a17717b21bb4e328f03ade32` is pinned by the shared core script;
the core binary digest is measured on each host and included in campaign
identity.

Build outputs are ignored under `workloads/nes/build/stb/`. ROMs and cores are
local/build inputs; the CI upload includes compact evidence and the
[artifact notice](../../STB-ARTIFACT-LICENSE.md), upstream license, and
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

The archive key (`stb_local_ai_spatial_16_preference_v3`) uses one
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

## Evidence and compatibility

Campaigns start from a validated stage-0 local-AI genesis, with initial raw
stock counters and configuration all equal to four. Zero is the last live
stock, so the terminal loss is number five. A different setup is rejected.

Champion selection reuses the archive's progress and resource preference;
higher player damage, hitstun, grounded flags, and coordinates are not extra
champion rewards. Run-wide progress reports only peak opponent knockout and
damage values. Those are independent maxima across observed branches, not a
single achieved endpoint; use the champion observation for that endpoint's
resources. Player stock losses remain in milestones and observations.

Policy `stb_local_ai_spatial_16_preference_v3` fixes the unsolved-champion
ordering and uses floor division at every pooling depth, including negative
coordinates. Stream/checkpoint formats are v3 because the progress report
schema also changed. Recordings from the earlier v2 policy require the previous
implementation; the PR preserves that history and its qualification evidence.
Compare searcher changes only with the same recorded adapter policy.

`frames_emulated` measures actual emulator work, including the short terminal
prefix re-execution needed to align the saved endpoint. It is not the number
of unique game frames explored. The champion tape and observation carry the
separate logical frame count.

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

Public CI builds and runs the author's WTFPL source release; the author also
[explicitly permits redistribution](https://itch.io/post/11601778). The root
software license does not replace separate asset notices. The default film
shows Sinbad (Zi Ye, CC BY-SA 3.0) and Kiki (Tyson Tan, CC BY-SA 4.0 option),
with Tui's CC-BY "Kiki theme". The uploaded
[artifact notice](../../STB-ARTIFACT-LICENSE.md) provides attribution, source
and license links, identifies the recording transformation, and distributes
the video under CC BY-SA 4.0. Recheck the media notice when changing the pinned
game, setup, characters, music or recording scope.

To reproduce one CI campaign locally after the source build, from the repository
root:

```sh
cargo build --locked --release --manifest-path workloads/nes/Cargo.toml \
  --bin stb-probe --bin stb-campaign
HARMONY_QUICKNES_CORE=workloads/nes/build/stb/quicknes_libretro.so \
HARMONY_STB_CORRECTNESS=1 \
  workloads/nes/target/release/stb-probe workloads/nes/build/stb/stb.nes
workloads/nes/target/release/stb-campaign \
  --core workloads/nes/build/stb/quicknes_libretro.so \
  --rom workloads/nes/build/stb/stb.nes --output workloads/nes/build/stb-artifact \
  --seed 1 --executions 2000 --workers 2 --action-limit 512 \
  --fixed-execution-soak
```
