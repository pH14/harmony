<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Thwaite workload adapter

This module adapts the source-built [Thwaite](https://github.com/pinobatch/thwaite-nes)
NES game to Dissonance's target-neutral search interfaces. It owns the
symbol-checked memory decoder, ordinary title setup, controller encoding,
mechanical observations, archive identity, progress milestones, terminal
conditions, and the QuickNES replay and rendering interface. The generic
coordinator sees only those interfaces and receives no aiming strategy.

The adapter implements `CampaignTypes`, `TargetExecution`, `InputPolicy`,
`Evaluation`, and `Reporting` in the NES workload package, and retains the same
execution override as the other NES adapters: a shortened hold is recorded at an
interior ending and phase-invalid endpoints are excluded from admission while
their observations are preserved. Scheduling, archive maintenance, recording,
and replay remain owned by Dissonance.

## Why this workload

The existing NES roster searches games that reward acting: Nova advances by
moving through a level, Super Tilt Bro by landing hits. Thwaite inverts that.
Fireworks intercept incoming missiles, the town is destroyed by *omission*, and
a search that does nothing loses buildings while the wave plays out. Progress is
therefore only available by aiming a cursor and firing at the right frame, which
exercises credit assignment rather than positional novelty.

The genesis also seeds the game's own randomness. Upstream mixes the NMI frame
counter into its LFSR at the moment the title screen is dismissed, so the wave
schedule follows from the fixed setup tape; the same tape always produces the
same first hour.

## Declared workload

The primary workload is an ordinary one-player game from power-on: dismiss the
upstream notice screen, take the default `1 player` menu row, let the
introductory cut scene run, and seal the genesis at the first frame of the first
hour with all twelve buildings standing, both silos loaded with fifteen
fireworks, and no missile yet in flight. Practice mode and the two-player mode
are not used; no save edit, password, imported input, or scripted firing
sequence is used.

An hour ends when the wave is exhausted and no enemy missile remains in flight
(`gameState` moves from `2` to `3`). The declared objective is **surviving the
whole 35-hour campaign** — seven days of five hours — and `--survive-hours N`
lowers that target for a bounded run. A **perfect hour**, one that ends with
`buildingsDestroyedThisLevel` still zero, stays a reported milestone rather than
the objective: the interesting question is how far into the week a search can
keep a town alive, not how immaculately it plays one wave. The
game itself ends at `gameState = 7` when both silos are destroyed, or when an
hour ends with no building left; `gameState = 0` after genesis means the game
returned to the title, which after seven surviving days is the whole-game
ending. Both are terminal for the campaign.

## Identity and build

The source is pinned to `pinobatch/thwaite-nes` commit
`00e36745188bc165990f60eed6b093c3ce6ad0e3`. The built image is the NROM-256
(mapper 0) `thwaite.nes`, SHA-256
`ee51cd9562f28195ba015d9857c6c4fc9bf67cdfb213e95f655e586b92195173`.

From the repository root:

```sh
sudo apt-get install --yes build-essential curl git make python3 python3-pil cc65 ffmpeg jq
workloads/nes/scripts/build-thwaite-rom.sh workloads/nes/build/thwaite
scripts/build-quicknes-core.sh workloads/nes/build/thwaite/quicknes_libretro.so
```

[`thwaite-versions.env`](../../thwaite-versions.env) pins the upstream
repository, commit, and expected ROM checksum. The build fetches that exact
commit, builds with ca65/ld65 through upstream's own makefile, and fails if the
ROM changes. Upstream carries no build clock or generated timestamp, so the
pinned source assembles the same ROM on every day and from any directory; the
ROM digest does depend on the assembler, which is the distribution `cc65`
package that CI installs. No source is patched.

The build then checks every observed address against `thwaite.dbg`, the linker
debug file upstream's makefile already emits, and fails closed if a symbol
moves. QuickNES revision `26bb785c9deddb66a17717b21bb4e328f03ade32` is pinned by
the shared core script; the core binary digest is measured on each host and
included in campaign identity.

Build outputs are ignored under `workloads/nes/build/thwaite/`. ROMs, debug
symbols, and cores are build inputs; the CI upload includes compact evidence and
the [artifact notice](../../THWAITE-ARTIFACT-LICENSE.md), upstream licence, and
upstream README, without binaries or snapshots.

## Observation contract

All addresses below are offsets in QuickNES's 2 KiB system-RAM window and are
linker symbols from the pinned revision, verified by the build.

| Field | Address | Meaning and role | Why it is retained |
| --- | ---: | --- | --- |
| `game_state` | `$0038` | `0` inactive, `1` new level, `2` active, `3` level reward, `7` game over | Terminal/report |
| `num_players` | `$0039` | Configured player count (`1`) | Genesis identity |
| `practice` | `$004f` | Practice-mode flag (`0`) | Genesis identity |
| `town.buildings` | `$03cc..$03d7` | Twelve buildings: `0` destroyed, `1` standing, `2` threatened | Durable progress and preference |
| `town.buildings_destroyed_this_level` | `$03d8` | Buildings lost during the current hour | Perfect-hour condition |
| `score` | `$03d9/$03da` | Hundreds and remainder, combined as `100 * hundreds + ones` | Progress and preference |
| `game_day`/`game_hour` | `$03dd/$03de` | Campaign clock; the level index is `5 * day + hour` | Location identity |
| `game_minute`/`game_second` | `$03df/$03e0` | In-hour clock | Mechanical observation |
| `town.silo_missiles` | `$03e6/$03e7` | Left/right silo ammunition | Same-location capability preference |
| `town.enemy_missiles_left` | `$0305` | Enemy missiles not yet launched this hour | Wave phase; identity and progress |
| `town.enemy_missiles_in_flight` | `$0412..$0425` | Count of enemy missile slots with a nonzero Y | Wave phase identity |
| `town.crosshair_x/y` | `$04e0/$04e4` | Player-one crosshair position | Location identity |

The decoder exposes town fields as an optional `town` payload, present only
while `game_state` is neither `0` nor `7`, so the title and game-over screens
cannot turn reused bytes into buildings or ammunition. Slots `0..3` of the
missile arrays are the players' own fireworks and are excluded from the
in-flight count.

Each observation also carries cumulative defence evidence derived frame by
frame: buildings lost, hours cleared, and perfect hours. Because the frame scan
sees every interior boundary, an hour cleared inside a held chord is retained as
evidence; the input tape remains the reproducible witness for that event.

## Archive and input policy

The archive key (`thwaite_hours_survived_town_wave_bucket_v2`) keeps eight
elites per cell. Cell identity is the level index, the standing-building count,
the wave phase (missiles left to launch and missiles in flight), and a coarse
ammunition bucket; wider groups drop ammunition, then the in-flight count, then
the fine wave phase, leaving the level index and a coarse wave band at the
class depth. Objective progress is hours survived, then standing buildings,
then wave depth, then score.

The crosshair is deliberately **not** part of cell identity. An earlier revision
keyed it in 16-pixel cells and the archive grew to 8,228 entries inside the
first hour alone; removing it collapsed the same search to 17 entries, which
showed the novelty budget had been spent enumerating reversible cursor
positions rather than game states. Eight elites per cell restore the population
that removal costs.

`progress_cmp` orders classes by the hour reached, then by emptier wave and
flight bands, and treats everything inside a band as mutually maximal. That
relation is only consulted by the `EnergyProgress*` selectors, so the campaign
defaults to
`hierarchy_uniform_128_energy_progress_cheapest_v1:3,6,12,2` and `--selector`
takes any identifier the searcher recognises. The band has to live at the class
depth (the coarsest group) or the relation degenerates to the hour alone and
selection dilutes uniformly across every cell in that hour.

`ButtonChord` uses the QuickNES/NES serial layout: A `0x01`, B `0x02`, Select
`0x04`, Start `0x08`, Up `0x10`, Down `0x20`, Left `0x40`, Right `0x80`. In one
player mode the game fires the left silo on B and the right silo on A, both at
the crosshair, and both edge-triggered. Search chords combine nine
non-conflicting direction states with the four A/B states. Select is excluded
because it has no gameplay action; Start is excluded because it pauses.

Durations (`stratified_aim_or_watch_v2`) are aiming taps of 1--8 frames (three
times in five) or watches of 48--120 frames. The long holds matter more than
they look: the searcher extends a lineage by at most six actions per execution,
so a policy capped at short holds advances roughly ninety frames per execution
against an hour that runs two to three thousand frames, and clearing one hour
then needs about twenty-five consecutive admitted expansions on a single
lineage. Watching while missiles fall is also what a person does between shots.

The campaign uses ordinary `Unprobed` admission, `OneToSix` suffixes, and the
game-neutral `AlphabetOnly` draw mixture; `ProbeAtAdmission` is explicitly
rejected.

## Continuous evaluation

[Search evaluation checks](../../../../.github/workflows/search-eval.yml) run
the [Thwaite evaluation action](../../../../.github/actions/thwaite-evaluation/action.yml):
a four-execution smoke with a wall limit on relevant PRs and main changes, and a
20,000-execution seed-1 soak on manual dispatch. The correctness probe checks
the sealed genesis, neutral input, per-silo fire ownership, aiming directions,
restored continuations, and the admission probe before search runs. The campaign
then compares live and replayed reports and checkpoint bytes and verifies the
headless and rendered endpoints.

The registered lanes are fixed-execution soaks. Surviving all 35 hours is the
declared objective and the action can require a target
(`THWAITE_REQUIRE_SURVIVAL=true`, which also stops the campaign once the target
hour is reached), but no repository CI run has demonstrated one, so no lane
requires it.

Measured depth on one Linux box, seed 1, three workers, an 8,192-action horizon
and the policy above:

| executions | hours survived |
| ---: | ---: |
| 800 | 1 |
| 4,800 | 2 |
| 39,500 | 3 |

The superseded `..._wave_phase_v1` key policy, which keyed the crosshair and
capped holds at 48 frames, reached two hours at 163,000 executions on the same
box and seed. These are measured outcomes for one seed, not golden values, and
the cost of each additional hour grows roughly eightfold, so the full campaign
is an open problem for this policy rather than a budget away. CI does not retry,
change seed, or alter search policy.

Artifacts are retained for 30 days: summary and progress, full champion input
and observation, campaign/replay reports, control probe, the full champion film
plus 180 neutral frames, and source/licence notices with checksums. CI compares
the champion and rendered tapes/observations and counts encoded video frames so
the film includes the entire input and tail. ROMs, debug symbols, cores,
snapshots, and raw media are excluded.

To reproduce one CI campaign locally after the source build, from the repository
root:

```sh
cargo build --locked --release --manifest-path workloads/nes/Cargo.toml \
  --bin thwaite-probe --bin thwaite-campaign
HARMONY_QUICKNES_CORE=workloads/nes/build/thwaite/quicknes_libretro.so \
HARMONY_THWAITE_CORRECTNESS=1 \
  workloads/nes/target/release/thwaite-probe workloads/nes/build/thwaite/thwaite.nes
workloads/nes/target/release/thwaite-campaign \
  --core workloads/nes/build/thwaite/quicknes_libretro.so \
  --rom workloads/nes/build/thwaite/thwaite.nes \
  --output workloads/nes/build/thwaite-artifact \
  --seed 1 --executions 20000 --workers 2 --action-limit 8192 --fixed-execution-soak
```

Dropping `--fixed-execution-soak` stops the campaign once `--survive-hours`
(default 35) is reached and reports it as a qualified campaign.
