# NES workload package

This standalone Rust workspace adapts SMB, Nova, Mega Man 2, Metroid, and Super
Tilt Bro to Dissonance.
It owns game interpretation, controller policies, campaign binaries, and workload
reporting. Generic archive and campaign mechanisms come from `searcher`; emulator
and guest execution support comes from `../nes-machine`.

The `smb-*`, `nova-*`, `mm2-*`, `metroid-*`, and `stb-*` binaries provide campaign and replay entry
points. Set `HARMONY_QUICKNES_CORE` to the pinned QuickNES shared library for
native execution. SMB and Nova support native QuickNES and whole-VM Consonance execution.
Mega Man 2, Metroid, and Super Tilt Bro currently use their native campaigns or
the common `nes-eval` runner; shared CLI dispatch and Consonance execution are
not implemented for them.
The Consonance backend uses the `consonance` feature and requires Linux/KVM
and matching guest artifacts; `harmony search --package nes --backend
consonance ROM` selects it through the shared CLI.

## Backend acceptance matrix

The native/Consonance backend oracle covers SMB and Nova, but the available
artifact and platform evidence is not uniform:

Nova has two separate Consonance evidence paths. The package acceptance lane
below builds the ROM-free generic NES base image and runs the shared backend
oracle and CLI entry point. The larger Nova experiment in
`.github/workflows/nova-consonance-experiment.yml` uses the legacy
`initramfs-nova.cpio.gz` image and its specialized campaign/oracle binaries;
that experiment does not provide SMB acceptance evidence.

| Game/backend | Current evidence | Required artifacts and platform |
| --- | --- | --- |
| Nova/native | CI package acceptance and the Nova Consonance experiment exercise the pinned ROM and QuickNES core. | Host QuickNES core; the CI ROM is built from the pinned source recipe. |
| Nova/Consonance | Real VM campaign and backend checks run in `.github/workflows/nova-consonance-experiment.yml`. | Linux/KVM, pinned kernel, generic NES base image, and the pinned Nova ROM/core. |
| SMB/native | Adapter and loopback tests are checked in; no current real-ROM CI lane is claimed here. | Pinned QuickNES core and a licensed SMB ROM supplied by the caller. |
| SMB/Consonance | `nes-backend-oracle` supports the path; no repository CI VM result is claimed here. | Linux/KVM, a capable NES base image, pinned core, and a caller-supplied licensed SMB ROM. |
| Mega Man 2/native | All eight independent stage origins pass local full-campaign replay qualification through `nes-eval`; commercial ROMs are excluded from CI. | Pinned QuickNES core and a caller-supplied licensed MM2 ROM. |
| Metroid/native | New-game origin passes local full-campaign replay qualification through `nes-eval`; this is not an ending claim. Commercial ROMs are excluded from CI. | Pinned QuickNES core and a caller-supplied licensed Metroid ROM. |
| Super Tilt Bro/native | `search-eval.yml` (bounded checks) and `nova-nightly.yml` (nightly NES benchmark) build the pinned ROM, probes controls/restoration, evaluates Easy/Fair/Hard AI and verifies replay/full-champion video; Hard must win within its execution ceiling. | Host QuickNES core and the pinned source-built offline UNROM game. |

On Linux/KVM, the shared oracle is invoked as:

```sh
cargo run --release --manifest-path workloads/nes/Cargo.toml \
  --features consonance --bin nes-backend-oracle -- \
  ROM QUICKNES_CORE KERNEL NES_BASE_INITRAMFS
```

The oracle identifies SMB and Nova from the ROM and compares native and
whole-VM observations, restored continuations, terminal outcomes, and probes.
ROMs are not redistributed by this package. On aarch64, the caller must supply
a kernel, base image, and QuickNES artifact built for that architecture; no
current ARM package acceptance result is claimed by this matrix.

```sh
cargo test --manifest-path workloads/nes/Cargo.toml
cargo clippy --manifest-path workloads/nes/Cargo.toml --all-features --all-targets -- -D warnings
```

The [Nova](src/nova/README.md), [Mega Man 2](src/mm2/README.md), and
[Metroid](src/metroid/README.md) READMEs document their input and observation maps. Campaign streams
and checkpoints retain their versioned workload identities across crate moves.

The package also owns the pinned Nova source recipe and ROM revision in
`scripts/build-nova-rom.sh` and `nova-versions.env`. Its default output is
`build/nova`. The `tools` directory contains NES movie conversion and trace
utilities; artifact redistribution terms are recorded in
`NOVA-ARTIFACT-LICENSE.md`.

[Super Tilt Bro](src/stb/README.md) has a separate pinned recipe in
`scripts/build-stb-rom.sh`, `stb-versions.env`, and `STB-ARTIFACT-LICENSE.md`.
New workloads use the execution, observation, input, and qualification contracts
described above, with minimal game guidance.

## Local evaluation matrix

[`benchmarks/search`](../../benchmarks/search/README.md) supplies one compact
native `nes-eval` runner for SMB, Nova, Mega Man 2, Metroid, and Super Tilt Bro.
ROM/core hashes, origin definitions, complete adapter policy identities,
resource budgets, verification, and immutable export are shared across games.
Licensed ROMs stay in a private host inventory. Source-built games and generic
runner/engine tests can run in ordinary CI.

Nova's `NovaGame::with_whole_game()` changes the recorded terminal predicate to
all 40 cleared-level flags, continues execution through intermediate clears, and
permits an 8,192-action horizon. The default remains the isolated-level workload.
Whole-game runs must begin at level 1; isolated level setups are never scored as
whole-game completion.

`nes-eval` optionally accepts `slot_retention: "resource_extremes_2_v1"`.
Metroid supplies health/missiles and MM2 health/total weapon energy; other
workloads keep ordinary retention. This bounded research policy leaves the
controller vocabulary and selector unchanged. Its identity is recorded in
streams and evaluation provenance.

The separate `resource_coverage_2_v1` option keeps up to two states maximizing
joint resource-threshold coverage, allowing a useful intermediate tradeoff to
survive. It uses the same axes and archive budget. Total weapon energy remains
a scalar proxy; it does not encode weapon-specific future requirements. Both
policies require matched fresh-search evaluation before promotion.

`representative_job_sample_2_v1` instead keeps the ordinary best representative
and the best state from the lowest-ranked creation job. Its fixed job ranking
uses no search RNG and needs no resource axes. It retains at most two states
under the same archive byte budget, including when resources tie. This is a
research option; the ranking gives no guarantee of useful future behavior.

`quality_representatives_2_v1` supplies a plain top-two-quality control.
`context_representatives_2_v1` keeps the top two representatives from distinct
opaque contexts. Metroid supplies a facing/speed-sign context only in builds
with `--features metroid-motion-context`; other builds/workloads have no such
context and use ordinary retention for that policy. Motion metadata changes
neither retention-slot geometry nor selector groups. Both comparison arms must
use the same feature build, resource limits and recorded terminal semantics.
No experimental policy or feature is a production default.

`--features metroid-boss-context-audit` adds a reporting-only Metroid encounter
sample at each completed live action endpoint and saves the first producing
input for two replays. It uses separate artifact identities and adds metadata
and verification costs; both comparison arms must use the same feature build.
See the [Metroid observation and cost contract](src/metroid/README.md#experimental-endpoint-encounters).

Build `nes-eval` with `--features selector-cost-audit` to forward the generic
searcher's observation-only cost-rank diagnostic into progress sidecars. This
can be combined with `metroid-motion-context`; comparison arms must use the same
feature build. The separate selector identifier
`room_cell_uniform_128_energy_progress_no_cost_v1:3,6,12,2` removes selection-side
historical group-time ranks. Its use does not change workload retention or the
controller vocabulary. See the [searcher policy contract](../../dissonance/searcher/README.md)
and [registered research](../../benchmarks/search/continuation-yield/README.md).


## Experimental action correlation

For Metroid and MM2, `nes-eval` optionally accepts `chord_correlation` as
`component_refresh_half_v1` or `whole_repeat_37_of_210_v1`. Both require
`alphabet_only`. The shared controller transform operates only within each
newly drawn suffix; it never reads a restored state's action history or game
observations. It preserves suffix length, all hold frames and special-tap
positions. A special tap clears the previous-command context. No persistent
draw state or snapshot field is added.

The component policy draws a complete fresh command half the time and refreshes
one of the three direction/A/B components otherwise. The whole-command control
has a derived repeat probability that matches complete-command run lengths.
The optional `action_correlation` entry in game policies records the exact law;
a different policy context rejects replay. Defaults and explicit
`independent_v1` preserve the existing policy map and independent sampler.
These are research choices, not demonstrated game improvements. The
[finite model and counterexample](../../benchmarks/search/action-correlation/README.md)
explain what is proved and what remains empirical.

## Exact local-retention diagnostics

`metroid-retention-replay extract REQUEST OUT` exports a pinned competition
from a complete capture without constructing an emulator. It checks the full
footer and both snapshot/input hashes; the exported inputs are checkpoint-local.
`metroid-retention-pair draws SEEDS BANK` freezes ordinary shared continuations,
and `metroid-retention-pair run REQUEST OUT` compares exact captured states with
per-frame boss diagnostics, verified restores and held-command boundary replay.
Use the same `metroid-motion-context,metroid-boss-context-audit` feature identity
as the capture. The finite diagnostic changes no archive policy. Its bounds and
limitations are documented in
[PC01](../../benchmarks/search/continuation-reassessment/pc01-design.md).
