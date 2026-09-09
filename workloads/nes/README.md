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

Development-only observation audits are available through `metroid-local-search`
(request-bounded search from an input discovered by a prior campaign, with
composed ordinary-genesis witness verification) and `nes-input-inspect`
(raw-machine replay, endpoint RAM/image, and bounded terminal-state probes).
The raw inspector checks a combined declared prefix/suffix ceiling of 20,000
actions and 2M normalized hold frames before creating an emulator target, then
checks the resolved physical tape again after adapter-owned setup/award frames.
These are tape bounds; repeated verification work is accounted separately.
Their diagnostic origins never qualify fresh search. Requests, bounds, exact
provenance and experimental policy comparisons are documented in the
[alternative-futures research ledger](../../benchmarks/search/alternative-futures/README.md).

`nes-eval` accepts `nes_duration` for Metroid and MM2. The default
`stratified_short_or_long_v1` preserves historical holds of 2–12 or 48–120
frames. Experimental `stratified_short_middle_long_v2` samples three equal
bands (2–12, 13–47, 48–120), keeping button draws and special menu taps
unchanged. The duration identity is part of strict campaign replay context.
The local Metroid diagnostic uses the same option named `duration`.
The ablation `stratified_two_band_mean_matched_v1` retains the two historical
bands but chooses short holds with probability 131/231, matching the three-band
mean of 121/3 frames. It tests average duration separately from middle-band support.
