# Nova the Squirrel benchmark

Nova the Squirrel is Dissonance's second real NES workload and its first
source-buildable game benchmark. It runs on the current `searcher`/`machine`
architecture; no LibAFL code or vocabulary is involved.

## Boundary and state preference

The coordinator in `searcher/src/search/` knows only controller actions,
opaque ordered archive groups, an opaque same-location comparison, snapshots,
and serialized observations. Every Nova address, menu input, and
interpretation lives under `searcher/src/nova/`.

Nova keeps one scheduled representative per 16-pixel location. At an existing
location the adapter's opaque comparison prefers, in order: more cleared
levels, more collectibles, more available levels, carrying an ability, more
health, and more puzzle chips. This follows the orthogonal state-preference
pattern suggested by Antithesis's
[Metroid write-up](https://antithesis.com/blog/2025/metroid/): spatial
diversity remains bounded while useful non-spatial state decides which
representative receives future work. Generic search sees only `Ordering` and
never those fields.

Coarser groups expose durable progress and campaign/internal level identity to
the generic frontier selector. Reports keep an action-interior mechanical
watermark, while the film champion is selected at an action endpoint so its
state is exactly reproducible by the serialized input.

## Reproducible inputs

`nova-versions.env` pins Nova commit
`e9e79ae59b188348bd6a87117a2d5c86a34ba433`, the GitHub archive SHA-256, and
the expected source-built ROM SHA-256. `scripts/build-nova-rom.sh` verifies the
archive before extraction, builds the checked-in assets with cc65, verifies
the ROM, and checks every observed linker symbol against the newly generated
debug symbols. QuickNES uses the existing pinned
`scripts/build-quicknes-core.sh` recipe.

## Observation map

The addresses are linker symbols at the pinned Nova revision. Coordinates use
the engine's 12.4 fixed-point representation: `high * 16 + low / 16`.

| Observation | CPU address | Region | Upstream source |
|---|---:|---|---|
| Player X low/high | `$0025/$0026` | system RAM | [`memory.s`](https://github.com/NovaSquirrel/NovaTheSquirrel/blob/e9e79ae59b188348bd6a87117a2d5c86a34ba433/src/memory.s#L46-L54) |
| Player Y high/low | `$0027/$0028` | system RAM | [`memory.s`](https://github.com/NovaSquirrel/NovaTheSquirrel/blob/e9e79ae59b188348bd6a87117a2d5c86a34ba433/src/memory.s#L46-L54) |
| Health | `$004B` | system RAM | [`memory.s`](https://github.com/NovaSquirrel/NovaTheSquirrel/blob/e9e79ae59b188348bd6a87117a2d5c86a34ba433/src/memory.s#L67-L78) |
| Internal/selected level | `$00A7/$00A8` | system RAM | [`memory.s`](https://github.com/NovaSquirrel/NovaTheSquirrel/blob/e9e79ae59b188348bd6a87117a2d5c86a34ba433/src/memory.s#L120-L133) |
| Reload pending | `$00A9` | system RAM | [`memory.s`](https://github.com/NovaSquirrel/NovaTheSquirrel/blob/e9e79ae59b188348bd6a87117a2d5c86a34ba433/src/memory.s#L120-L133) |
| Puzzle chips/required | `$0508/$0509` | system RAM | [`memory.s`](https://github.com/NovaSquirrel/NovaTheSquirrel/blob/e9e79ae59b188348bd6a87117a2d5c86a34ba433/src/memory.s#L295-L311) |
| Copied ability | `$7200` | save RAM `$1200` | [`memory.s`](https://github.com/NovaSquirrel/NovaTheSquirrel/blob/e9e79ae59b188348bd6a87117a2d5c86a34ba433/src/memory.s#L341-L359) |
| Cleared levels | `$7F1F..$7F26` | save RAM `$1F1F` | [`memory.s`](https://github.com/NovaSquirrel/NovaTheSquirrel/blob/e9e79ae59b188348bd6a87117a2d5c86a34ba433/src/memory.s#L435-L444) |
| Available levels | `$7F27..$7F2E` | save RAM `$1F27` | [`memory.s`](https://github.com/NovaSquirrel/NovaTheSquirrel/blob/e9e79ae59b188348bd6a87117a2d5c86a34ba433/src/memory.s#L435-L444) |
| Collectibles | `$7F2F..$7F36` | save RAM `$1F2F` | [`memory.s`](https://github.com/NovaSquirrel/NovaTheSquirrel/blob/e9e79ae59b188348bd6a87117a2d5c86a34ba433/src/memory.s#L435-L444) |

The exit-door routine writes the clear bit, unlocks the next level, advances
the level numbers, and saves inventory in
[`TouchedDoorBottom`](https://github.com/NovaSquirrel/NovaTheSquirrel/blob/e9e79ae59b188348bd6a87117a2d5c86a34ba433/src/metaplayer.s#L628-L661).

## Setup, replay, film, and licensing

The adapter owns a bounded title → main menu → level select → pre-level →
gameplay walk with release frames between edge-triggered presses. Genesis is
sealed only when health and coordinates confirm gameplay. Search actions
exclude Start and Select and draw nine non-conflicting D-pad directions
crossed with all four A/B combinations.

`nova-campaign` normally records the job stream, immediately replays it, and
requires the report and whole-tree checkpoint to be byte-identical. The
marketing nightly selects `--marketing-soak` instead: it runs headlessly until
victory or a 500,000-execution cap, retains only a compact progress summary, and
skips the full-campaign replay and multi-gigabyte checkpoint artifact. It replays
only the winning or champion input with video and native 48 kHz stereo audio
enabled on the same QuickNES build, muxing both into the uploaded MP4. Search
remains on QuickNES's hard audio/video-off path, so audiovisual capture consumes
resources only for that final film. The film must still reach the champion's
headless decoded endpoint, and the PCM and MP4 hashes are recorded with it.

Upstream describes code as GPL-3.0-or-later and graphics, levels, and other
assets as CC BY-NC-SA 4.0 with additional restrictions in its
[README](https://github.com/NovaSquirrel/NovaTheSquirrel/blob/e9e79ae59b188348bd6a87117a2d5c86a34ba433/README.md#L36-L40).
The uploaded film is a noncommercial benchmark demonstration and includes
`NOVA-ARTIFACT-LICENSE.md`; the ROM and emulator library are never uploaded.

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
  --seed 1 --executions 500000 --workers 4 --action-limit 512 \
  --marketing-soak
```
