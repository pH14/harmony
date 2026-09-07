# NES workload package

This standalone Rust workspace adapts SMB, Nova, and Super Tilt Bro to Dissonance.
It owns game interpretation, controller policies, campaign binaries, and workload
reporting. Generic archive and campaign mechanisms come from `searcher`; emulator
and guest execution support comes from `../nes-machine`.

The `smb-*`, `nova-*`, and `stb-*` binaries provide campaign and replay entry
points. Set `HARMONY_QUICKNES_CORE` to the pinned QuickNES shared library for
native execution. SMB and Nova support native QuickNES and whole-VM Consonance execution.
Super Tilt Bro currently uses its standalone native campaign; shared CLI dispatch
and Consonance execution are not implemented for it.
The Consonance backend uses the `consonance` feature and requires Linux/KVM
and matching guest artifacts; `harmony search --package nes --backend
consonance ROM` selects it through the shared CLI.

## Backend acceptance matrix

The adapter and backend oracle cover both ROM kinds, but the available
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
| Super Tilt Bro/native | `.github/workflows/stb.yml` builds the pinned ROM, probes controls/restoration, runs fixed-budget search against Easy/Fair/Hard AI and verifies replay/full-champion video. | Host QuickNES core and the pinned source-built offline UNROM game. |

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

`src/nova/README.md` documents Nova's input and observation map. Campaign streams
and checkpoints retain their versioned workload identities across crate moves.

The package also owns the pinned Nova source recipe and ROM revision in
`scripts/build-nova-rom.sh` and `nova-versions.env`. Its default output is
`build/nova`. The `tools` directory contains NES movie conversion and trace
utilities; artifact redistribution terms are recorded in
`NOVA-ARTIFACT-LICENSE.md`.

[Super Tilt Bro](src/stb/README.md) has a separate pinned recipe in
`scripts/build-stb-rom.sh`, `stb-versions.env`, and `STB-ARTIFACT-LICENSE.md`.
The [NES integration skill](../../.agents/skills/nes-game-integration/SKILL.md)
describes how to add workloads with minimal game guidance.
