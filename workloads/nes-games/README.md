<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# NES game logic

`nes-games` holds the game logic of the NES workloads: Super Mario Bros., Mega
Man 2, Metroid, Super Tilt Bro, Twin Dragons and Famidash. It builds for the
host and for `wasm32-unknown-unknown`, so the browser demo can run the same
game logic as the native workload.

Each game module has a `target` with the RAM decoder, observations, snapshots,
setup tapes and the `Target` implementation, and an `archive` with the
controller vocabulary, archive keys and milestones. Mega Man 2, Metroid and
Famidash also have a `progress` module for named progress. `chord` holds the
controller chord draw. `nes_backend` holds the `NesBackend`, `NesMemory` and
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
`FamidashGame<M>`, `SmbGame<M, P>`, `Mm2Game<M>` and `MetroidGame<M>` are
generic over the backend and build each target from a boxed factory, so the
native workload and the browser demo share every trait implementation: the
native one passes a QuickNES or Consonance factory, and `demos/twin-dragons`
passes one over the browser core. LZ4 snapshot packing is available on every
target for that reason. Job results are digested with
`postcard_semantic_result_sha256` from the searcher, which leaves out emulator
state, so a browser campaign and a native one produce the same decision
stream.

[`../nes`](../nes/README.md) re-exports these modules and keeps everything
else: campaign drivers and configuration, Super Tilt Bro's `Workload`
implementation, streams, films, evaluation, packaging, guest preparation and
binaries. The game READMEs under `../nes/src` document the RAM maps and
policies of each game.

```sh
cargo test --manifest-path workloads/nes-games/Cargo.toml --all-features
cargo check --manifest-path workloads/nes-games/Cargo.toml --target wasm32-unknown-unknown
```
