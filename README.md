# Harmony

Harmony is a test environment for exploring controlled executions and replaying
an interesting execution exactly.

It has two parts:

- consonance is the deterministic machine. It runs a controlled workload, owns
  its time and environmental inputs, captures complete machine state, and can
  branch or replay from that state.
- dissonance is the explorer. It chooses inputs and faults, evaluates the
  resulting states, and retains useful paths for further search.

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

The CLI runs OCI workloads with `harmony oci run IMAGE -- COMMAND`.
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
- `dissonance/` contains the machine abstraction, campaign engine, search
  archive, and workload adapters.
- `scripts/` contains repository-level development and validation helpers.

consonance and dissonance are separate Rust workspaces. consonance also owns the
shared environment and control vocabulary because those contracts form part of
the deterministic machine boundary. dissonance depends on the meaning of that
boundary without depending on a particular hypervisor implementation.

## License

Harmony is free software licensed under the GNU Affero General Public License
v3.0 or later (`AGPL-3.0-or-later`). See [LICENSE](LICENSE).
