# NES workload package

This standalone Rust workspace adapts SMB, Nova, Mega Man 2, Metroid, and Super
Tilt Bro to Dissonance.
It owns game interpretation, controller policies, campaign binaries, and workload
reporting. Generic archive and campaign mechanisms come from `searcher`; emulator
and guest execution support comes from `../nes-machine`. Consonance stages an
OCI image through `oci-support`, injects the caller's ROM at `/game.nes`, and
delegates process startup to the platform supervisor.

The `smb-*`, `nova-*`, `mm2-*`, `metroid-*`, and `stb-*` binaries provide campaign and replay entry
points. Set `HARMONY_QUICKNES_CORE` to the pinned QuickNES shared library for
native execution. SMB and Nova support native QuickNES and whole-VM Consonance execution.
Mega Man 2, Metroid, and Super Tilt Bro currently use their native campaigns or
the common `nes-eval` runner; shared CLI dispatch and Consonance execution are
not implemented for them.
The Consonance backend uses the `consonance` feature and requires Linux/KVM
and matching guest artifacts; `harmony search --package nes --backend
consonance ROM` selects it through the shared CLI.

`src/film.rs` renders a recorded tape to an H.264 MP4 with game audio: it
streams RGB frames into FFmpeg, muxes the raw PCM track back in, and writes a
four-times-faster copy beside the film. `smb-film`, `metroid-film` and
`mm2-film` drive the same target the searcher drives, so the film is the
recorded run rather than a re-derivation of it, and each needs `ffmpeg` on the
path. `metroid-map-probe` and `mm2-energy-probe` replay a tape and print one
line per action endpoint, for state the campaign report sums away: the map cell
a Metroid route crossed, and the per-weapon Mega Man 2 meters behind the decoded
sum.

Campaign recordings use the current Dissonance schedule policy version 3 and
bounded progress policy. Replay rejects recordings from superseded policy
namespaces before constructing a replay target.

## Backend acceptance matrix

The native/Consonance backend oracle covers SMB and Nova, but the available
artifact and platform evidence is not uniform:

Nova has two separate Consonance evidence paths. The package acceptance lane
below builds the generic NES OCI image and runs the shared backend oracle and
CLI entry point. The larger Nova experiment in
`.github/workflows/nova-consonance-experiment.yml` uses the same platform
runtime and NES OCI image through its specialized campaign/oracle binaries; it
does not provide SMB acceptance evidence.

| Workload/backend | Current evidence | Required artifacts and platform |
| --- | --- | --- |
| Nova/native | CI package acceptance and the Nova Consonance experiment exercise the pinned ROM and QuickNES core. | Host QuickNES core; the CI ROM is built from the pinned source recipe. |
| Nova/Consonance | Real VM campaign and backend checks run in `.github/workflows/nova-consonance-experiment.yml`. | Linux/KVM, pinned kernel/runtime, NES OCI image, and the pinned Nova ROM/core. |
| SMB/native | Adapter and loopback tests are checked in; no current real-ROM CI lane is claimed here. | Pinned QuickNES core and a licensed SMB ROM supplied by the caller. |
| SMB/Consonance | `nes-backend-oracle` supports the path; no repository CI VM result is claimed here. | Linux/KVM, the platform runtime, NES OCI image, and a caller-supplied licensed SMB ROM. |
| Mega Man 2/native | All eight independent stage origins pass local full-campaign replay qualification through `nes-eval`; commercial ROMs are excluded from CI. | Pinned QuickNES core and a caller-supplied licensed MM2 ROM. |
| Metroid/native | New-game origin passes local full-campaign replay qualification through `nes-eval`; this is not an ending claim. Commercial ROMs are excluded from CI. | Pinned QuickNES core and a caller-supplied licensed Metroid ROM. |
| Super Tilt Bro/native | `product-smoke.yml` selects a bounded PR smoke; `search-eval.yml` provides manual qualification. The scheduled/manual `nova-nightly.yml` capability panel evaluates Easy/Fair/Hard AI in independent case jobs through `nes-eval`. Hard retains its victory requirement in the public panel. | Host QuickNES core and the pinned source-built offline UNROM game. |

On Linux/KVM, the shared oracle is invoked as:

```sh
cargo run --release --manifest-path workloads/nes/Cargo.toml \
  --features consonance --bin nes-backend-oracle -- \
  ROM QUICKNES_CORE KERNEL PLATFORM_INITRAMFS NES_OCI_IMAGE
```

The oracle identifies SMB and Nova from the ROM and compares native and
whole-VM observations, restored continuations, terminal outcomes, and probes.
ROMs are not redistributed by this package. On aarch64, the caller must supply
a kernel, platform runtime, and NES OCI image built for that architecture; no
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

The scheduled/manual public capability panel is registered in
`benchmarks/search/nightly.json` and reports isolated Nova levels, whole-game
Nova, and STB in independent case jobs with one combined roster. It intentionally does not include licensed
SMB, Mega Man 2, or Metroid. Run the full private evaluation and the separate
SMB reference manifest with `benchmarks/search/run-private.sh` on a Linux host
that already has the caller's asset inventory; those reports remain local.

Nova's `NovaGame::with_whole_game()` changes the recorded terminal predicate to
all 40 cleared-level flags, continues execution through intermediate clears, and
permits an 8,192-action horizon. The default remains the isolated-level workload.
Whole-game runs must begin at level 1; isolated level setups are never scored as
whole-game completion.
