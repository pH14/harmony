<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# NES game logic

`nes-games` holds the game logic of the NES workloads: Super Mario Bros., Mega
Man 2, Metroid, Super Tilt Bro, Twin Dragons and Famidash. It builds for the
host and for `wasm32-unknown-unknown`, so the browser demo can run the same
game logic as the native workload.

Each game module has a `target` with the RAM decoder, observations, snapshots,
setup tapes and the `Target` implementation, and an `archive` with the
controller vocabulary, archive keys and milestones. Mega Man 2 and Metroid
also have a `progress` module for named progress. Famidash has a `levels`
module with each side's level names, lengths and groups. `chord` holds the
controller chord draw. Direction alternatives use bounded stack storage and preserve the vocabulary order and generator draws. Famidash generates only the suffix prefix that the coordinator keeps: its chord draws use a local mutation RNG and read-only draw tables, so discarded tail draws do not affect search or replay. `nes_backend` holds the `NesBackend`, `NesMemory` and
`SnapshotState` traits that every target uses to run, snapshot and read a
machine.

Every target type is generic over its backend. The QuickNES parts are compiled
only on Unix, because `machine::quicknes` exists only there: the `NesBackend`
and `NesMemory` implementations for `QuickNesMachine`, its LZ4 snapshot
packing, and the constructors, capture and render methods of each target that
take a ROM and a core path. The `consonance` feature adds the backend
implementation for the Consonance machine. The `test-support` feature exposes
test constructors and the scripted Super Tilt Bro machine to the tests of
`nes-workload`.

Twin Dragons, Famidash, Super Mario Bros., Mega Man 2 and Metroid also have a
`campaign` module with their `Workload` implementation. `TwinDragonsGame<M>`,
`FamidashGame<M, GROUP>`, `SmbGame<M, P>`, `Mm2Game<M>` and `MetroidGame<M>` are
generic over the backend and build each target from a boxed factory, so the
native workload and the browser demo share every trait implementation: the
native one passes a QuickNES or Consonance factory, and `demos/twin-dragons`
passes one over the browser core. LZ4 snapshot packing is available on every
target for that reason. Job results are digested with
`postcard_semantic_result_sha256` from the searcher, which leaves out emulator
state, so a browser campaign and a native one produce the same decision
stream. Archive entries record a snapshot digest from `snapshot_identity_sha256`,
and an archive checkpoint must match those digests. SMB hashes the logical
snapshot identity; the other games hash the serialized snapshot.

Each game's recorded emulator policy ends with a hash field. The native
constructors take the core hash and the prepared execution hash from
`machine::quicknes::prepared_execution_sha256`, record both, and load every
machine with `QuickNesMachine::from_rom_bytes_with_identity`, which rejects a
load whose executable code differs from the prepared one. The browser demo
passes the hash of its core WASM module instead.

`arcade` holds what the arcade page draws, for both the browser campaign and a
native live campaign. `ArcadeGame` extends `Workload` with a view of each
observation, the four-number motion sample of a frame, the cleared-level count
and the emulator state of a snapshot. The `smb`, `metroid`, `mm2`, `famidash`
and `twin_dragons` submodules implement it for each campaign game over any
backend. `ArcadeGame` decodes RAM in static functions, so `FamidashGame` takes
the index of a level group in `famidash::levels::GROUPS` as the const
parameter `GROUP`. Each group is its own game type, with that group's side,
levels and map tops. `trail::Traced` wraps a machine and records each job's motion trail
every four frames and at every chord end, and every controller action it runs. `feed::run_noted` runs a job inside
that recorder and returns a `Note` with the trail, the view of every step and
each candidate's snapshot digest. `feed::Feed` joins notes with the
coordinator's admission decisions into the `Batch` the page reads: points,
trails, finished trails and their exits. Aggregate-only notes update the full
frame and death totals without entering the pending admission map; batches
report the cumulative `sampled_jobs` count for notes containing display data.
A bounded terminal ledger keeps one actual cleared-level step per nonzero `u8`
count plus an objective endpoint after ordinary batches are drained. Internal
per-step cleared-count metadata identifies the finishing step within a job;
it is not part of the browser protocol. `take_terminal_batch` replays that sparse
summary without retaining transient motion or all historical points.
It drops ids the archive no longer
holds once it keeps more than twice the archive's entries.
`feed::archived_state` builds a state's controller history and replays it
through a fresh target with `feed::played` when the archived input misses
frames the core ran. The digest is taken over the QuickNES
core state without the native snapshot header, so a native snapshot and a
browser capture of the same state have the same digest.

[`../nes`](../nes/README.md) re-exports these modules and keeps everything
else: campaign drivers and configuration, Super Tilt Bro's `Workload`
implementation, streams, films, evaluation, packaging, guest preparation and
binaries. The game READMEs under `../nes/src` document the RAM maps and
policies of each game.

```sh
cargo test --manifest-path workloads/nes-games/Cargo.toml --all-features
cargo check --manifest-path workloads/nes-games/Cargo.toml --target wasm32-unknown-unknown
```

Snapshot checkpoint codecs live with these shared target types so native and
browser callers use the same checksummed `HMSNAP02` envelope. The standalone
formats are Famidash v2, Twin Dragons v2, Metroid v13, Mega Man 2 v4, SMB
QuickNES v4 and SMB Consonance v3; Super Tilt Bro's native campaign selects v5.
These plain snapshots use the postcard payload codec inside the framed envelope.
Previous flat envelopes are refused. Native `nes` target modules remain
re-export shims and do not supply separate checkpoint implementations.
