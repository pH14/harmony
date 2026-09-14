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
| Super Tilt Bro/native | `search-eval.yml` (bounded checks) and the scheduled/manual `nova-nightly.yml` capability panel build the pinned ROM and evaluate Easy/Fair/Hard AI through the common `nes-eval` runner. Hard retains its victory requirement in the public panel. | Host QuickNES core and the pinned source-built offline UNROM game. |

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
Nova, and STB in one common roster. It intentionally does not include licensed
SMB, Mega Man 2, or Metroid. Run the full private evaluation and the separate
SMB reference manifest with `benchmarks/search/run-private.sh` on a Linux host
that already has the caller's asset inventory; those reports remain local.

Nova's `NovaGame::with_whole_game()` changes the recorded terminal predicate to
all 40 cleared-level flags, continues execution through intermediate clears, and
permits an 8,192-action horizon. The default remains the isolated-level workload.
Whole-game runs must begin at level 1; isolated level setups are never scored as
whole-game completion.

### Prepared-execution admission evidence

The `prepare-admission` example dumps the actual Rust preparation API outputs
for the controlled NES and bare-PostgreSQL workloads. It does not launch a VM
or create an approval baseline:

```sh
cargo run --locked --manifest-path workloads/nes/Cargo.toml --example prepare-admission -- \
  nes KERNEL PLATFORM.cpio.gz NES.oci NEW_OUTPUT_DIR ROM.nes
cargo run --locked --manifest-path workloads/nes/Cargo.toml --example prepare-admission -- \
  postgres KERNEL PLATFORM.cpio.gz POSTGRES.oci NEW_OUTPUT_DIR
```

The helper supports only native Linux x86_64 default KVM sessions and rejects
other hosts. It serializes the real `SessionConfig::default()` as
`session-config.json`; the checker requires its pre-PID1 eager-binding and xstate
boot flags. Custom session configurations, including campaign and fault adapters,
are outside this scope. Inputs must be local OCI layouts. The dump includes exact platform, rootfs and
control archives, serialized execution and image configuration, input hashes,
prepared identity, kernel bytes, and ordered concatenated initramfs bytes. NES
uses `prepare::prepare_oci`; PostgreSQL uses the image's existing command through
`oci_support::bundle::prepare`. ROM bytes and PostgreSQL's `/workload.sql` and
entrypoint script receive explicit hashes; all workload files remain bound by
the rootfs segment. The Nova ROM pin is in `nova-versions.env`.

Run `workloads/guest-images/verify-prepared-admission.py inventory DUMP
--output NEW_REPORT_DIR` from the repository root for candidate evidence. GNU
objdump is required (`--objdump` selects it). The checker parses the three
archives separately and compares their exact concatenation; it does not claim
the scanner accepts general concatenated archives. It rejects file collisions,
platform writes into the workload namespace, unexpected control inputs, loader
overrides and noncanonical writable ROM mounts. A deterministic workload-root
CPIO view removes only the actual `harmony-oci/rootfs` prefix, preserving entry
metadata/content so absolute ELF dependency paths resolve in their guest root.

`verify DUMP --baseline REVIEWED.json --output NEW_REPORT_DIR` additionally
requires `version: 1`, a named `reviewed_by`, the reviewed `manifest_sha256`, `composition_evidence`,
`platform_baseline`, and `workload_baseline`. Each evidence/baseline reference
contains a relative `file` and its `sha256`; both component baselines are checked
by the existing x86 scanner. Candidate generation never produces these reviews.
The evidence must cover the actual runtime configuration/mounts, loader binding
before every exec, trusted SQL/ROM/code inputs, no JIT/generated code, and no
code mutation. The OCI root remains writable: input digest binding is not
runtime filesystem immutability enforcement. Unsupported overlay or workload
semantics fail closed rather than broadening this controlled scope.

Run `python3 workloads/guest-images/test_verify_prepared_admission.py
-v` for composition mutation checks. Build/check the example with the package's
locked Cargo dependencies before using its dumps for review.

The quality workflow runs the Python composition mutation tests. Candidate
reports still require explicit reviewer evidence before admission verification.

The `admission` module writes candidate manifests from the same OCI preparation
API used for execution. The `prepare-admission` example retains its exact default
Linux x86 session format. The tools probe reuses the writer for a separately
named Nova A–E scope; see `workloads/tools/README.md`. Neither writer approves
its output or substitutes for component and composition review.
