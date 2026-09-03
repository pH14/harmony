<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Nova workload adapter

This module adapts Nova the Squirrel to Dissonance's target-neutral search
interfaces. It owns Nova addresses, menu input, observations, archive keys,
state preference, and terminal conditions. The generic coordinator receives
only actions, ordered keys, observations, snapshots, and policy identifiers.

## Archive policy

Nova retains one scheduled representative per 16-pixel location. At the same
location, the adapter prefers states with more cleared levels, collectibles,
available levels, carried abilities, health, and puzzle chips, in that order.
Coarser archive groups represent durable progress and level identity.

Reports may record progress reached inside an action. Reproducer selection uses
action endpoints, where the serialized input identifies the complete state.

## Reproducible inputs

`../../../../nova-versions.env` pins the Nova source revision, archive SHA-256,
and built ROM SHA-256. `../../../../scripts/build-nova-rom.sh` verifies the
source archive, builds the ROM with cc65, checks its digest, and verifies the
observed linker symbols against the generated debug symbols. The QuickNES build
is pinned by the repository's `scripts/build-quicknes-core.sh`.

## Observation map

Coordinates use Nova's 12.4 fixed-point representation: `high * 16 + low / 16`.

| Observation | CPU address | Region |
|---|---:|---|
| Player X low/high | `$0025/$0026` | system RAM |
| Player Y high/low | `$0027/$0028` | system RAM |
| Health | `$004B` | system RAM |
| Internal/selected level | `$00A7/$00A8` | system RAM |
| Reload pending | `$00A9` | system RAM |
| Puzzle chips/required | `$0508/$0509` | system RAM |
| Copied ability | `$7200` | save RAM `$1200` |
| Cleared levels | `$7F1F..$7F26` | save RAM `$1F1F` |
| Available levels | `$7F27..$7F2E` | save RAM `$1F27` |
| Collectibles | `$7F2F..$7F36` | save RAM `$1F2F` |

The addresses are linker symbols from the revision pinned in
`nova-versions.env`. The build rejects a ROM whose generated symbols differ.

## Setup and actions

The adapter performs a bounded title, main-menu, level-select, pre-level, and
gameplay sequence with release frames between edge-triggered presses. Genesis
is sealed after health and coordinates confirm gameplay. Search actions exclude
Start and Select. They combine nine non-conflicting directional states with the
four A/B button states.

## Replay and media capture

`nova-campaign` records and replays the campaign stream in its standard mode.
The `--marketing-soak` mode runs to victory or its execution cap, stores a
compact progress summary, and replays only the winning or leading input with
video and 48 kHz stereo capture enabled. Headless search keeps audio and video
disabled. The captured endpoint must match the headless decoded endpoint.

The Nova source is GPL-3.0-or-later. Its graphics, levels, and other assets use
CC BY-NC-SA 4.0 with additional upstream restrictions. Published media includes
`dissonance/NOVA-ARTIFACT-LICENSE.md`; ROM and emulator binaries are excluded.

## Local Linux run

```sh
sudo apt-get install cc65 ffmpeg
dissonance/scripts/build-nova-rom.sh dissonance/nova-build
scripts/build-quicknes-core.sh dissonance/nova-build/quicknes_libretro.so
cargo run --locked --release --manifest-path dissonance/Cargo.toml \
  --bin nova-campaign -- \
  --core dissonance/nova-build/quicknes_libretro.so \
  --rom dissonance/nova-build/nova.nes \
  --output dissonance/nova-artifact \
  --seed 1 --executions 500000 --workers 4 --action-limit 512
```
