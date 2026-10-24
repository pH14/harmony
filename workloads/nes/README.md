# NES workload package

This standalone Rust workspace adapts SMB, Mega Man 2, Metroid, and Super Tilt Bro
to Dissonance.
It owns campaign drivers, binaries, films, evaluation and workload reporting.
Game interpretation and controller policies come from
[`../nes-games`](../nes-games/README.md). This package re-exports those modules
under the same paths, so `nes_workload::mm2::target` names the Mega Man 2
target, and its `target` modules make `QuickNesMachine` the default backend of
each target type. Generic archive and campaign mechanisms come from `searcher`;
emulator and guest execution support comes from `../nes-machine`. Consonance stages an
OCI image through `oci-support`, injects the caller's ROM at `/game.nes`, and
delegates process startup to the platform supervisor.

The `smb-*`, `mm2-*`, `metroid-*`, and `stb-*` binaries provide campaign and replay entry
points. Set `runner.options.core` in the CLI recipe or `HARMONY_QUICKNES_CORE` in the campaign tools to the pinned QuickNES shared library for
native execution. SMB supports native QuickNES and whole-VM Consonance execution.
The SMB campaign binary creates an `ArchiveSource` by parsing and hashing one
open file, so replacing its path cannot attach one archive's digest to another's
entries. The generic campaign recorder and replay verifier require that sealed
source, including through the public NES workload wrappers.
Run and replay require a UTF-8 origin argument so its recorded path names the
same file the command opens; an ambiguous byte path is rejected before use.
Checkpoint paths also require UTF-8, so the recorded source path names the file
opened.
Saved campaign replay rejects a source archive whose exact bytes differ from the
archive digest recorded in the stream, even if its parsed report is unchanged.
It also decodes and hashes snapshot checkpoints from the same read bytes, so a
source replacement cannot label one snapshot set with another file's digest.
SMB fixture extraction likewise pins its source archive digest to the entries it
parses into a challenge.
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
MP4. `nes-film` renders the evaluation matrices. It reads the input and `result.json` a campaign already
recorded and writes `film.json` beside `witness.mp4`, so the film is that run's
own input rather than a second search.

A witness's `snapshot_sha256` hashes snapshots after `SnapshotState::logical_identity`. The Super Tilt Bro
campaign result digest covers the complete job result, including preparation
failures and retained QuickNES snapshots. Replay therefore detects a latent
endpoint-state change even when its decoded game observations and archive key
agree. The games in `nes-games` digest only the decoded result so a browser
campaign and a native one produce the same stream. Replay binds each archive
checkpoint snapshot to the digest its source archive entry records. SMB entries
record the logical snapshot identity; the other games record the serialized
snapshot. The checkpoint file hash continues to cover its exact serialized
bytes. On Intel KVM hosts a whole-VM endpoint's raw
x87 presence bit changes between runs of the same input with equal guest RAM,
so the raw bytes cannot serve as a replay identity.

`nes-film --recorded-backend native` requires the replayed witness to equal the
one the run recorded. `--recorded-backend consonance` compares the recorded
semantic endpoint instead, because a whole-VM run's snapshot digest differs from
a native one by construction. Such a film is a native QuickNES replay of a tape
the VM search recorded: `film.json` sets `endpoint_bridged` and leaves
`evidence_verified` false, and nothing is captured inside the guest.

`--max-frames` bounds the render. A whole-game input has no length bound and
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

`nes-games/src/chord.rs` draws the controller chords for every game. An action is a chord
held for a short hold of 2–12 frames or a long hold, with equal odds. The long
hold is 96–120 frames for SMB and 48–120 frames for the other games. Each game
names its held chords and at most one tap button: Select for Metroid, Start for
Mega Man 2, none for SMB and Super Tilt Bro.

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
`nes-games/src/nes_backend.rs`. A stored snapshot is the QuickNES state as an LZ4 block
with its length in front. A restore checks the declared length against the
core's state size before it decompresses. The archive charges the compressed
length plus the snapshot record, so a memory budget keeps more snapshots
resident and replays fewer evicted parents. The SMB state of 12,912 bytes
compresses about seven times. The Metroid state of 21,118 bytes, which includes
8 KiB of cartridge RAM, compresses to about 9 KB. Compression takes about 10 µs
and decompression about 6 µs, against about 105 µs for one emulated frame.
Super Tilt Bro keeps a live handle for the current state and exports
from it. SMB, Metroid and Mega Man 2 capture and restore through
`NesBackend::capture_nes` and `NesBackend::restore_nes`. On QuickNES each worker
thread serializes, compresses and decompresses in buffers it reuses, so a
capture allocates only the stored block and a restore allocates nothing. With
one malloc arena, every allocation takes a lock that all workers share. The
Consonance backend stores its own portable state through the same trait.
Mega Man 2 and Metroid also read work RAM and cartridge RAM through
`NesMemory` in the same file. Metroid also writes them there. QuickNES
implements `NesMemory`, and the Consonance backend does not.

A snapshot keeps the decoded observation it was taken at. QuickNES serves
reads right after a restore, so a game that needs work RAM after a restore
reads it from the machine. The Consonance backend serves reads only after a
run. `SnapshotState::WorkRam` is the copy of work RAM that a snapshot of that
state keeps: none for QuickNES and 2 KiB for Consonance. SMB stores its work
RAM there. Observations carry no list of changed RAM
addresses and no log line, because nothing reads them.
Checkpoint freezing accounts for owned emulator/work-RAM capacities before
cloning. Portable Consonance state exposes its immutable shared page/sidecar
allocations for the image's private ledger; retaining them does not keep an
archive entry active or exempt imports from verification. Evidence checkpoint
writers stream borrowed fields and preserve the existing postcard/JSON bytes,
including on the synchronous staging-pressure path.

Campaign recordings use the current Dissonance schedule policy version 6 and
bounded progress policy. Replay rejects recordings from superseded policy
namespaces before constructing a replay target.

## Backend evidence matrix

The native/Consonance backend oracle covers SMB, but the available artifact
and platform evidence is not uniform:

| Workload/backend | Current evidence | Required artifacts and platform |
| --- | --- | --- |
| SMB/native | Adapter and loopback tests are checked in; no current real-ROM CI lane is claimed here. | Pinned QuickNES core and a licensed SMB ROM supplied by the caller. |
| SMB/Consonance | `nes-backend-oracle` supports the path; no repository CI VM result is claimed here. | Linux/KVM, the platform runtime, NES OCI image, and a caller-supplied licensed SMB ROM. |
| Mega Man 2/native | All eight independent stage origins and the whole-game origin pass local full-campaign replay qualification through `nes-eval`; commercial ROMs are excluded from CI. | Pinned QuickNES core and a caller-supplied licensed MM2 ROM. |
| Metroid/native | New-game origin passes local full-campaign replay qualification through `nes-eval`; this is not an ending claim. Commercial ROMs are excluded from CI. | Pinned QuickNES core and a caller-supplied licensed Metroid ROM. |
| Twin Dragons/native | Unit tests are checked in and `twin-dragons-campaign` passes local replay qualification; no CI lane runs it yet. | Host QuickNES core and the pinned silent-effects release ROM. |
| Zelda/native | Unit tests are checked in and `nes-eval` runs a cold campaign from the first overworld screen; commercial ROMs are excluded from CI. | Pinned QuickNES core and a caller-supplied licensed The Legend of Zelda (USA) (Rev 1) ROM. |
| Castlevania/native | Unit tests are checked in and a short cold `nes-eval` probe replays its witness; commercial ROMs are excluded from CI. | Pinned QuickNES core and a caller-supplied licensed Castlevania (USA) (Rev 1) ROM. |
| Famidash/native | Unit tests are checked in, and `nes-eval` clears Stereo Madness with a replayed victory witness on seeds 1 to 5; no CI lane runs it yet. | Host QuickNES core with the four-screen state patch and the four official 1.2.8 release ROMs. |
| Punch-Out!!/native | Unit tests are checked in and `nes-eval` runs a cold campaign from the first round against Glass Joe; commercial ROMs are excluded from CI. | Pinned QuickNES core and a caller-supplied licensed Punch-Out!! (USA) ROM. |
| Super Tilt Bro/native | The `STB` job in `Checks / Dissonance Workloads / NES` runs a bounded search on affected pull requests. The `Benchmarks / Dissonance Workloads / NES` capability panel evaluates Easy/Fair/Hard AI in independent case jobs through `nes-eval`. Hard retains its victory requirement in the public panel. | Host QuickNES core and the pinned source-built offline UNROM game. |

On Linux/KVM, the shared oracle is invoked as:

```sh
cargo run --release --manifest-path workloads/nes/Cargo.toml \
  --features consonance --bin nes-backend-oracle -- \
  ROM QUICKNES_CORE KERNEL PLATFORM_INITRAMFS NES_OCI_IMAGE
```

The oracle identifies SMB from the ROM and compares native and
whole-VM observations, restored continuations, terminal outcomes, and probes.
ROMs are not redistributed by this package. On aarch64, the caller must supply
a kernel, platform runtime, and NES OCI image built for that architecture; no
current ARM result is claimed by this matrix.

`nes-core-oracle` writes the native QuickNES reference for the browser demo's
WebAssembly core. It runs a game's setup tape followed by seeded chords from
that game's vocabulary, with occasional Start and Select taps, and writes the
tape, system RAM after every frame, and system RAM, cartridge RAM and the
canonical core state at every action boundary. `--play` turns on video and
audio capture. `demos/twin-dragons/tests/core-oracle.mjs` runs it and
compares the browser core frame by frame.

```sh
cargo test --manifest-path workloads/nes/Cargo.toml
cargo clippy --manifest-path workloads/nes/Cargo.toml --all-features --all-targets -- -D warnings
```

The [Mega Man 2](src/mm2/README.md),
[Metroid](src/metroid/README.md), [Castlevania](src/castlevania/README.md), [Punch-Out!!](src/punchout/README.md) and [Zelda](src/zelda/README.md) READMEs document their input and observation maps. Campaign streams
and checkpoints retain their versioned workload identities across crate moves.

The `tools` directory contains NES movie conversion and trace utilities.

[Super Tilt Bro](src/stb/README.md) has a separate pinned recipe in
`scripts/build-stb-rom.sh`, `stb-versions.env`, and `STB-ARTIFACT-LICENSE.md`.
[Twin Dragons](src/twin_dragons/README.md) has a pinned recipe in
`scripts/build-twin-dragons-rom.sh`, `twin-dragons-versions.env`, and
`TWIN-DRAGONS-ARTIFACT-LICENSE.md`.
[Famidash](src/famidash/README.md) has a pinned recipe in
`scripts/build-famidash-rom.sh` and `famidash-versions.env`.
New workloads use the execution, observation, input, and qualification contracts
described above, with minimal game guidance.

## Local evaluation matrix

[`benchmarks/search`](../../benchmarks/search/README.md) supplies one compact
native `nes-eval` runner for SMB, Mega Man 2, Metroid, and Super Tilt Bro.
ROM/core hashes, origin definitions, complete adapter policy identities,
resource budgets, verification, and immutable export are shared across games.
Licensed ROMs stay in a private host inventory. Source-built games and generic
runner/engine tests can run in ordinary CI.

The scheduled/manual public capability panel is registered in
`benchmarks/search/nightly.json` and reports STB Easy, Fair and Hard in independent
case jobs with one combined roster. It intentionally does not include licensed
SMB, Mega Man 2, or Metroid. Run the full private evaluation and the separate
SMB reference manifest with `benchmarks/search/run-private.sh` on a Linux host
that already has the caller's asset inventory; those reports remain local.

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
prepared identity, kernel bytes, and ordered concatenated initramfs bytes. Source
hashes in `image-inputs.json` cover the exact `index.json` bytes staging parsed
and the selected manifest, config, and layer blobs it verified. Unselected OCI
layout files are omitted, and changing the source layout during preparation
cannot make these hashes describe a different selection from the staged image.
The checker validates the dump's file hashes but does not reopen the source
layout; this provenance comes from staging's verified reads. NES
uses `prepare::prepare_oci`; PostgreSQL uses the image's existing command through
`oci_support::bundle::prepare`. ROM bytes and PostgreSQL's `/workload.sql` and
entrypoint script receive explicit hashes; all workload files remain bound by
the rootfs segment. PostgreSQL's post-preparation source reads temporarily
grant the staging user read/search access and restore the image modes afterward,
so their hashes do not depend on host privilege. These reads require resolved
source paths to remain inside the staged rootfs, preventing image symlinks from
hashing host files.

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
module writes candidate manifests from the actual OCI preparation API for
default Linux x86 sessions. It does not broaden the controlled workload scope.

## Reusing native evaluation

`eval::run_cli` reads the common request, checks the external ROM/core hashes,
and prepares the output directory. A game-specific evaluator supplies the
workload and run policy to `eval::evaluate`. That module owns search limits,
checkpoint/resume configuration, progress and resource phases, witness and
milestone verification, and result export. `nes-eval` keeps the existing game
dispatch and option validation. Dedicated workload binaries can reuse the same
protocol and validation machinery without copying the evaluation loop.

## Live view

Every `nes-eval` campaign of Super Mario Bros., Mega Man 2, Metroid,
Castlevania, Twin Dragons, Famidash, Punch-Out!! or Zelda serves the arcade page's view of itself over HTTP on a
loopback port. It prints `live view listening on 127.0.0.1:PORT` and writes
`live.json` with the address, the page's game id, the process id and, on Linux,
the process start time from `/proc/self/stat`, to its output directory. It
removes the file when the campaign ends. A process killed by a signal leaves the
file behind, and the start time keeps a reused process id from matching it. The
server refuses any other address, and answers 403 to a request whose `Host` or
`Origin` names another host: it has no authentication and can serve the ROM
directory. To watch from another machine, forward the port over SSH, for
example `ssh -L 8790:127.0.0.1:PORT HOST`, or use the arcade launcher in
[`demos/twin-dragons`](../../demos/twin-dragons/README.md#native-runs).

The arcade page has no client definition for Castlevania or Punch-Out!!, so
their live views are served but not drawn.

Without a connected viewer, workers skip motion trails and detailed observation
records. With viewers, the service bounds detailed trails per 100 ms tick and
retains level-completion points even when transport sampling omits the trail.
Archive sampling selects a bounded set of states without scanning the whole
archive, and decodes one selected state at a time. These are observation limits;
they do not change search selection, admission order or retained search state.

Completion sends retained progress before the terminal event, including to a
late viewer, and drains active event streams for a bounded interval. State
lookups racing shutdown receive explicit missing-state replies instead of
waiting on a coordinator that has exited. Root and lookup states retain their
actual archived identity, replay input and game-frame count.

The viewing browser renders the event stream and records the same camera and
mini-map to 1920×1080 video at display cadence. Encoding runs on the viewing
machine; the native server needs no display or video encoder. Recording controls,
file streaming limits and qualification checks are documented in the
[arcade README](../../demos/twin-dragons/README.md#record-the-search-view).

A request with `live` set to a loopback address such as `127.0.0.1:8790` runs
the arcade game from genesis with its own draw mixture and a `Traced` core, as
the browser does, and rejects `stage`, `ai`, `root_input`, `metroid_terminal`,
`resume` and `mixture`. `live_site` names a directory with the built arcade
page and its `roms` directory, which the server serves at `/`. Other campaigns
keep their own game setup and mixture, and their view has no motion trails;
its root is the campaign's first archived state.

`GET /live/events` is a Server-Sent Events stream: a `ready` message with the
root state, the archive sample, then one `batch` every 100 ms in the shape the
browser search worker posts, with at most 256 trails and 2,000 unretained points
per batch, and a final `limit` message when the campaign ends.
`GET /live/states?ids=&request=` returns the controller input and core snapshot
of up to 12 archived states. The coordinator thread reads them between
admissions. In a `live` campaign, the request thread replays a state's input
through a fresh target when the archived input does not hold every frame the
core ran, as the browser does. Requests are limited to 64 KiB with 10-second
read and write timeouts, and the server holds at most 64 connections. Open
`http://127.0.0.1:8790/?game=GAME&live` to watch. The page ids are
`super-mario-bros`, `mega-man-2`, `metroid`, `castlevania`, `zelda`, `twin-dragons` and the five
Famidash groups `famidash`, `famidash-community`, `famidash-b`, `famidash-c` and
`famidash-d`; a Famidash campaign reports the group that holds its stage.

## Product CLI adapter

The CLI's NES adapter owns ROM identification and guest-image preparation. The
shared CLI sees a workload input, package identity and runner, not ROM/core/image
flags. QuickNES is a runner; Consonance supplies a separate runner with KVM for
this adapter. Runner libraries live in `runner.options.core`, and the guest OCI
image in `workload.options.guest_image`.

`package` exposes typed search and replay for native and prepared guest execution.
Native search and replay require the saved core digest from the run manifest;
the package checks the on-disk core before constructing a target, and the
QuickNES loader checks its private copy against the same digest on every load.
Before a native campaign creates its identity, it prepares a digest of the
executable QuickNES load, including its loaded dependency closure. SMB, Mega Man
2, Metroid, Super Tilt Bro, Famidash, Twin Dragons, Punch-Out!! and Zelda bind this prepared
execution digest in the recorded emulator policy, and the native live view loads
its machines against the same digest. Each campaign worker checks that digest while loading
its private core and before running the ROM. Native prepared.json, evaluation
identity.json, power-on fixture descriptors, progress replay records and STB
film evidence record the same digest, so replacing a dependency while keeping
the core file unchanged cannot reuse a saved campaign identity.
The standalone film and probe commands are diagnostic runs: they open the
selected private core and report observations, but their transient outputs do
not carry a campaign replay identity. The identity-bearing `nes-film` command
records the prepared execution digest in film.json.
For a saved search origin, the CLI checks the ROM and, for Consonance, the
kernel and initramfs against the inherited manifest at their final read before
passing those bytes to the package.
The Consonance SMB campaign's emulator policy and prepared execution identity
also bind the loaded Harmony executable through the exact client session
identity, so a saved stream or checkpoint from another build is refused before
the guest is replayed.
Search returns the exact serialized witness input and result that it wrote, so
the CLI can reject replacements before recording a completed search.
Search origins include genesis, a recorded input prefix and a retained whole-search
checkpoint. The package uses existing searcher snapshot-root and checkpoint APIs,
retains final checkpoints and preserves native/controller action types internally.
A CLI-selected checkpoint carries its journal digest into the searcher so a
file changed after selection cannot resume under the earlier identity. The
checkpoint's origin input comes from a saved, verified run artifact and is
passed to the package as pinned data; the package does not reopen the mutable
parent path to reconstruct that input. A rooted search stores the same typed
controller input that the package writes, even when the user's JSON contains
accepted fields outside that type.
See the [CLI README](../../cli/README.md) for named runs and investigation.

Full campaign exports write `checkpoint.bin` using the generic framed snapshot
codec. Snapshot artifacts use the same codec through fixture tools, package exports,
and SMB stream readers/writers; flat JSON checkpoint files are no longer written.
