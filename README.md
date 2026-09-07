# Harmony

Harmony is a test environment for exploring controlled executions and replaying
an interesting execution exactly.

Its components have distinct roles:

- consonance is the deterministic machine. It runs a controlled workload, owns
  its time and environmental inputs, captures complete machine state, and can
  branch or replay from that state.
- dissonance is the explorer. It schedules campaigns, executes rollouts, and
  retains useful paths for further search.
- workload packages supply programs, actions, observations, evaluation, and
  runtime preparation. The NES package supports direct emulator execution and
  Consonance. The faults package supplies guest fault injection for systems
  running together in one VM on one virtual CPU.

Harmony is under active development. The repository contains x86-64 and arm64
virtualization paths, a controlled Linux guest environment, deterministic
machine and protocol models, acceptance workloads, and search targets backed by
both an emulator and consonance. The supported determinism claim is narrower
than arbitrary software on arbitrary hardware; [Determinism](docs/DETERMINISM.md)
defines its scope.

## Try the CLI

```sh
cargo build --release -p harmony-cli
./target/release/harmony preflight
```

Search selects a workload package and, when needed, its execution backend:

```sh
harmony search --package nes smb.nes
harmony search --package nes --backend consonance smb.nes
harmony search --package faults foo.oci
```

NES defaults to the native QuickNES backend. Packages resolve their input and
record workload, execution, and search identities in campaign artifacts.

The CLI also runs OCI workloads with `harmony oci run IMAGE -- COMMAND`.
See [CLI documentation](cli/README.md) for prerequisites and run artifacts.

## Documentation

- [Architecture](docs/ARCHITECTURE.md) describes the system and its ownership
  boundaries.
- [Determinism](docs/DETERMINISM.md) defines exact replay, its argument, and its
  limits.
- [Exploration](docs/EXPLORATION.md) describes campaigns, rollouts, branching,
  and search replay.
- [Control protocol](docs/PROTOCOL.md) defines the operations between an
  explorer and a machine.
- [Testing](docs/TESTING.md) describes the oracles and corpus used to test the
  determinism claim.

Component details live in READMEs beside their code. Development setup and
repository checks live in [CONTRIBUTING.md](CONTRIBUTING.md).

## Repository map

- `consonance/` contains the deterministic VMM, machine models, snapshots,
  guest protocols, Linux guest environment, and acceptance suite.
- `dissonance/` contains the campaign engine, search archive, and target
  interfaces.
- `workloads/` contains NES adapters, guest payloads, fault tooling, and package
  preparation.
- `scripts/` contains repository-level development and validation helpers.

Consonance and Dissonance build independently. Standalone workload crates consume
their interfaces; the CLI composes them. The [Cargo dependency policy](docs/dependency-boundaries.toml)
and its required CI check enforce the direction of dependencies, including
optional, target-specific, build, and development dependencies. Consonance owns
opaque input transport and deterministic state. Fault definitions and policies
are supplied by workload tooling.

## License

Harmony is free software licensed under the GNU Affero General Public License
v3.0 or later (`AGPL-3.0-or-later`). See [LICENSE](LICENSE).
