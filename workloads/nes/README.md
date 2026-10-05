# NES workload package

This standalone Rust workspace adapts SMB, Nova, Mega Man 2, Metroid, and Super
Tilt Bro to Dissonance.
It owns game interpretation, controller policies, campaign binaries, and workload
reporting. Generic archive and campaign mechanisms come from `searcher`; emulator
and guest execution support comes from `../nes-machine`. Consonance stages an
OCI image through `oci-support`, injects the caller's ROM at `/game.nes`, and
delegates process startup to the platform supervisor.

The `smb-*`, `nova-*`, `mm2-*`, `metroid-*`, and `stb-*` binaries provide campaign and replay entry
points. Set `runner.options.core` in the CLI recipe or `HARMONY_QUICKNES_CORE` in the campaign tools to the pinned QuickNES shared library for
native execution. SMB and Nova support native QuickNES and whole-VM Consonance execution.
Mega Man 2, Metroid, and Super Tilt Bro currently use their native campaigns or
the common `nes-eval` runner; shared CLI dispatch and Consonance execution are
not implemented for them.
The Consonance backend uses the `consonance` feature and requires Linux/KVM
and matching guest artifacts; `harmony search --backend
kvm ROM` selects it through the shared CLI.

The campaign binaries, `nes-eval` and `harmony search` limit glibc to one
malloc arena on Linux. They call `mallopt(M_ARENA_MAX, 1)` before NES execution,
which has the same effect as `MALLOC_ARENA_MAX=1` and does not depend on the
environment. A search resumed from a checkpoint restores its snapshots on the
main thread before the workers start. With several arenas, the memory those
snapshots free stays in an arena the workers never reuse, and resident memory
grows past the archive's memory budget. A binary exits at startup if the call
fails.

`src/film.rs` renders a recorded tape to an H.264 MP4 with game audio: it
streams RGB frames into FFmpeg, muxes the raw PCM track back in, and writes a
four-times-faster copy beside the film. `smb-film`, `metroid-film` and
`mm2-film` drive the same target the searcher drives, so the film is the
recorded run rather than a re-derivation of it, and each needs `ffmpeg` on the
path. `metroid-map-probe` and `mm2-energy-probe` replay a tape and print one
line per action endpoint, for state the campaign report sums away: the map cell
a Metroid route crossed, and the per-weapon Mega Man 2 meters behind the decoded
sum.

`src/film.rs` also holds the benchmark capture contract. `Endpointed` returns a
game's decoded endpoint and frame count for a recorded input, `Filmable` adds
rendering that input to raw video and audio, and `Reel` owns the FFmpeg
pipeline: it rejects an empty or wrong-geometry render and a PCM track too short
to cover the video, muxes the track back in, and hashes both the track and the
MP4. `nes-film` renders the evaluation matrices of both compositions. It reads the input and `result.json` a campaign already
recorded and writes `film.json` beside `witness.mp4`, so the film is that run's
own input rather than a second search.

`nes-film --recorded-backend native` requires the replayed witness to equal the
one the run recorded. `--recorded-backend consonance` compares the recorded
semantic endpoint instead, because a whole-VM run's snapshot digest differs from
a native one by construction. Such a film is a native QuickNES replay of a tape
the VM search recorded: `film.json` sets `endpoint_bridged` and leaves
`evidence_verified` false, and nothing is captured inside the guest.

`--max-frames` bounds the render. A `nova-full` input has no length bound and
each action can hold up to 120 frames, so an unbounded film can run for hours. Frames past the
ceiling are still emulated and are left out of the video, making the film a
trailing window ending at the recorded endpoint plus `--tail-frames`. The
ceiling, the clip policy and the dropped frame count are recorded in `film.json`
under `clip`. `scripts/verify-nes-films.py` checks the MP4 against those
numbers: digest, frame count, a real audio stream, audible volume, audio that
covers the video, and a duration floor. Its `--media` mode applies the same
checks to an MP4 a workload wrote without a `film.json`, which is how the Super
Tilt Bro campaign's own witness is checked.

## Chord draw

`src/chord.rs` draws the controller chords for every game. An action is a chord
held for a short hold of 2–12 frames or a long hold, with equal odds. The long
hold is 96–120 frames for SMB and 48–120 frames for the other games. Each game
names its held chords and at most one tap button: Select for Metroid, Start for
Mega Man 2, none for SMB, Nova and Super Tilt Bro.

A new chord changes one control of the chord before it. The controls are the
D-pad, with nine positions, and the A and B buttons. The draw picks the D-pad,
A, B or no change with equal odds. A D-pad change moves to another position,
and an A or B change toggles that button. A change that would leave the game's
held chords keeps that control as it was. One draw in twelve is a tap instead:
the tap button joins the chord before it for 2–7 frames, and the next chord
drops the tap button and keeps the rest. The first chord of a game has nothing
before it and is drawn whole: a tap alone, or a held chord at random.

## Snapshots

Every game stores its QuickNES snapshots through `NesBackend` in
`src/nes_backend.rs`. A stored snapshot is the QuickNES state as an LZ4 block
with its length in front. A restore checks the declared length against the
core's state size before it decompresses. The archive charges the compressed
length plus the snapshot record, so a memory budget keeps more snapshots
resident and replays fewer evicted parents. The SMB state of 12,912 bytes
compresses about seven times. The Metroid state of 21,118 bytes, which includes
8 KiB of cartridge RAM, compresses to about 9 KB. Compression takes about 10 µs
and decompression about 6 µs, against about 105 µs for one emulated frame.
Nova and Super Tilt Bro keep a live handle for the current state and export
from it. SMB, Metroid and Mega Man 2 capture and restore through
`NesBackend::capture_nes` and `NesBackend::restore_nes`. On QuickNES each worker
thread serializes, compresses and decompresses in buffers it reuses, so a
capture allocates only the stored block and a restore allocates nothing. With
one malloc arena, every allocation takes a lock that all workers share. The
Consonance backend stores its own portable state through the same trait.

A snapshot keeps the decoded observation it was taken at. QuickNES serves
reads right after a restore, so a game that needs work RAM after a restore
reads it from the machine. The Consonance backend serves reads only after a
run. `SnapshotState::WorkRam` is the copy of work RAM that a snapshot of that
state keeps: none for QuickNES and 2 KiB for Consonance. SMB stores its work
RAM there. Nova needs no work RAM after a restore. Observations carry no list of changed RAM
addresses and no log line, because nothing reads them.

Campaign recordings use the current Dissonance schedule policy version 3 and
bounded progress policy. Replay rejects recordings from superseded policy
namespaces before constructing a replay target.

## Backend evidence matrix

The native/Consonance backend oracle covers SMB and Nova, but the available
artifact and platform evidence is not uniform:

Nova has two separate Consonance evidence paths. The `Backend Equivalence` job
in `Checks / Harmony Workloads / NES` builds the generic NES OCI image and runs
the shared backend oracle and CLI entry point. `Benchmarks / Harmony Workloads /
NES` uses the same platform runtime and NES OCI image through the whole-VM
campaign binary; it produces no SMB evidence.

| Workload/backend | Current evidence | Required artifacts and platform |
| --- | --- | --- |
| Nova/native | `Checks / Dissonance Workloads / NES` and `Benchmarks / Dissonance Workloads / NES` exercise the pinned ROM and QuickNES core. | Host QuickNES core; the CI ROM is built from the pinned source recipe. |
| Nova/Consonance | The whole-VM campaign runs in `Benchmarks / Harmony Workloads / NES` and the backend oracle in `Checks / Harmony Workloads / NES`. | Linux/KVM, pinned kernel/runtime, NES OCI image, and the pinned Nova ROM/core. |
| SMB/native | Adapter and loopback tests are checked in; no current real-ROM CI lane is claimed here. | Pinned QuickNES core and a licensed SMB ROM supplied by the caller. |
| SMB/Consonance | `nes-backend-oracle` supports the path; no repository CI VM result is claimed here. | Linux/KVM, the platform runtime, NES OCI image, and a caller-supplied licensed SMB ROM. |
| Mega Man 2/native | All eight independent stage origins and the whole-game origin pass local full-campaign replay qualification through `nes-eval`; commercial ROMs are excluded from CI. | Pinned QuickNES core and a caller-supplied licensed MM2 ROM. |
| Metroid/native | New-game origin passes local full-campaign replay qualification through `nes-eval`; this is not an ending claim. Commercial ROMs are excluded from CI. | Pinned QuickNES core and a caller-supplied licensed Metroid ROM. |
| Super Tilt Bro/native | The `STB` job in `Checks / Dissonance Workloads / NES` runs a bounded search on affected pull requests. The `Benchmarks / Dissonance Workloads / NES` capability panel evaluates Easy/Fair/Hard AI in independent case jobs through `nes-eval`. Hard retains its victory requirement in the public panel. | Host QuickNES core and the pinned source-built offline UNROM game. |

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
current ARM result is claimed by this matrix.

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

### Prepared-execution admission

The `prepare-admission` example dumps the actual Rust preparation API outputs
for the NES and bare-PostgreSQL workloads. It does not launch a VM or admit
the dump it writes:

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
--output NEW_REPORT_DIR` from the repository root for a candidate inventory. GNU
objdump is required (`--objdump` selects it). The checker parses the three
archives separately and compares their exact concatenation; it does not claim
the scanner accepts general concatenated archives. It rejects file collisions,
platform writes into the workload namespace, unexpected control inputs, loader
overrides and noncanonical writable ROM mounts. A deterministic workload-root
CPIO view removes only the actual `harmony-oci/rootfs` prefix, preserving entry
metadata/content so absolute ELF dependency paths resolve in their guest root.

`verify DUMP --output NEW_REPORT_DIR` additionally runs the executable property
scan over the platform and workload archives: no W+X segments, no executable
stack, no text relocations, no TLSdesc relocations, a canonical glibc loader and
a proven ECX-zero sequence at every XGETBV site. It compares nothing against a
stored digest, so rebuilding a guest does not require a new approval.

The execution scope requires eager binding before every exec, fixed trusted
SQL/ROM/code inputs, no JIT/generated code and no code mutation. The OCI root
remains writable: input digests do not enforce runtime filesystem immutability.
Unsupported overlay or workload semantics fail closed.

```sh
python3 workloads/guest-images/test_verify_prepared_admission.py -v
```

The quality workflow runs these composition mutation checks. The `admission`
module writes candidate manifests from the actual OCI preparation API. The
example describes default Linux x86 sessions; the tools probe uses the same
writer for its separately named Nova A–E scope, described in
`workloads/tools/README.md`. Neither mode broadens the controlled workload scope.

## Reusing native evaluation

`eval::run_cli` reads the common request, checks the external ROM/core hashes,
and prepares the output directory. A game-specific evaluator supplies the
workload and run policy to `eval::evaluate`. That module owns search limits,
checkpoint/resume configuration, progress and resource phases, witness and
milestone verification, and result export. `nes-eval` keeps the existing game
dispatch and option validation. Dedicated workload binaries can reuse the same
protocol and validation machinery without copying the evaluation loop.

## Product CLI adapter

The CLI's NES adapter owns ROM identification and guest-image preparation. The
shared CLI sees a workload input, package identity and runner, not ROM/core/image
flags. QuickNES is a runner; Consonance supplies a separate runner with KVM for
this adapter. Runner libraries live in `runner.options.core`, and the guest OCI
image in `workload.options.guest_image`.

`package` exposes typed search and replay for native and prepared guest execution.
Search origins include genesis, a recorded input prefix and a retained whole-search
checkpoint. The package uses existing searcher snapshot-root and checkpoint APIs,
retains final checkpoints and preserves native/controller action types internally.
See the [CLI README](../../cli/README.md) for named runs and investigation.
