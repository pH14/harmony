# NES workload package

This standalone Rust workspace adapts SMB, Nova, Mega Man 2, Metroid, Super
Tilt Bro, and Thwaite to Dissonance.
It owns game interpretation, controller policies, campaign binaries, and workload
reporting. Generic archive and campaign mechanisms come from `searcher`; emulator
and guest execution support comes from `../nes-machine`.

The `smb-*`, `nova-*`, `mm2-*`, `metroid-*`, `stb-*`, and `thwaite-*` binaries provide campaign
and replay entry points. Set `HARMONY_QUICKNES_CORE` to the pinned QuickNES shared library for
native execution. SMB and Nova support native QuickNES and whole-VM Consonance execution.
Mega Man 2, Metroid, Super Tilt Bro, and Thwaite currently use their native campaigns or
the common `nes-eval` runner; shared CLI dispatch and Consonance execution are
not implemented for them.
The Consonance backend uses the `consonance` feature and requires Linux/KVM
and matching guest artifacts; `harmony search --package nes --backend
consonance ROM` selects it through the shared CLI.

Campaign recordings use the current Dissonance schedule policy version 3 and
bounded progress policy. Replay rejects recordings from superseded policy
namespaces before constructing a replay target.

## Backend acceptance matrix

The native/Consonance backend oracle covers SMB and Nova, but the available
artifact and platform evidence is not uniform:

Nova has two separate Consonance evidence paths. The package acceptance lane
below builds the ROM-free generic NES base image and runs the shared backend
oracle and CLI entry point. The larger Nova experiment in
`.github/workflows/nova-consonance-experiment.yml` uses the legacy
`initramfs-nova.cpio.gz` image and its specialized campaign/oracle binaries;
that experiment does not provide SMB acceptance evidence.

| Workload/backend | Current evidence | Required artifacts and platform |
| --- | --- | --- |
| Nova/native | CI package acceptance and the Nova Consonance experiment exercise the pinned ROM and QuickNES core. | Host QuickNES core; the CI ROM is built from the pinned source recipe. |
| Nova/Consonance | Real VM campaign and backend checks run in `.github/workflows/nova-consonance-experiment.yml`. | Linux/KVM, pinned kernel, generic NES base image, and the pinned Nova ROM/core. |
| SMB/native | Adapter and loopback tests are checked in; no current real-ROM CI lane is claimed here. | Pinned QuickNES core and a licensed SMB ROM supplied by the caller. |
| SMB/Consonance | `nes-backend-oracle` supports the path; no repository CI VM result is claimed here. | Linux/KVM, a capable NES base image, pinned core, and a caller-supplied licensed SMB ROM. |
| Mega Man 2/native | All eight independent stage origins pass local full-campaign replay qualification through `nes-eval`; commercial ROMs are excluded from CI. | Pinned QuickNES core and a caller-supplied licensed MM2 ROM. |
| Metroid/native | New-game origin passes local full-campaign replay qualification through `nes-eval`; this is not an ending claim. Commercial ROMs are excluded from CI. | Pinned QuickNES core and a caller-supplied licensed Metroid ROM. |
| Super Tilt Bro/native | `search-eval.yml` (bounded checks) and the scheduled/manual `nova-nightly.yml` capability panel build the pinned ROM and evaluate Easy/Fair/Hard AI through the common `nes-eval` runner. Hard retains its victory requirement in the public panel. | Host QuickNES core and the pinned source-built offline UNROM game. |
| Thwaite/native | `search-eval.yml` builds the pinned ROM and runs the control probe, search, recorded replay, and film through the dedicated `thwaite-campaign` binary: a bounded PR smoke and a manual-dispatch soak. It is not in the `nes-eval` roster or the nightly panel. | Host QuickNES core and the pinned source-built NROM game. |

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

The [Nova](src/nova/README.md), [Mega Man 2](src/mm2/README.md),
[Metroid](src/metroid/README.md), and [Thwaite](src/thwaite/README.md) READMEs
document their input and observation maps. Campaign streams
and checkpoints retain their versioned workload identities across crate moves.

The package also owns the pinned Nova source recipe and ROM revision in
`scripts/build-nova-rom.sh` and `nova-versions.env`. Its default output is
`build/nova`. The `tools` directory contains NES movie conversion and trace
utilities; artifact redistribution terms are recorded in
`NOVA-ARTIFACT-LICENSE.md`.

[Super Tilt Bro](src/stb/README.md) has a separate pinned recipe in
`scripts/build-stb-rom.sh`, `stb-versions.env`, and `STB-ARTIFACT-LICENSE.md`.
[Thwaite](src/thwaite/README.md) has one in `scripts/build-thwaite-rom.sh`,
`thwaite-versions.env`, and `THWAITE-ARTIFACT-LICENSE.md`; its build verifies
every observed address against the linker debug file upstream already emits.
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
Nova, and STB in one common roster. It intentionally does not include licensed
SMB, Mega Man 2, or Metroid. Thwaite is not in that roster either: it runs
through its own campaign binary and CI action, and joining the common runner
would need a `nes-eval` case of its own. Run the full private evaluation and the separate
SMB reference manifest with `benchmarks/search/run-private.sh` on a Linux host
that already has the caller's asset inventory; those reports remain local.

Nova's `NovaGame::with_whole_game()` changes the recorded terminal predicate to
all 40 cleared-level flags, continues execution through intermediate clears, and
permits an 8,192-action horizon. The default remains the isolated-level workload.
Whole-game runs must begin at level 1; isolated level setups are never scored as
whole-game completion.
