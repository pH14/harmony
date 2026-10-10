# Harmony

Harmony is a runtime environment that runs your code deterministically while sussing out
interesting bugs along the way. This makes it particularly useful for finding and resolving [Heisenbugs](https://en.wikipedia.org/wiki/Heisenbug). It’s
built with testing databases and distributed systems in mind (i.e. what I’m most familiar with),
though will likely be valuable for many systems that can express their invariants or
correctness properties as assertions.

Harmony is composed of two halves that work together to reach resolution on your trickiest bugs:

* `consonance`: a deterministic Linux environment that runs your code reproducibly every time. It is built with
hardware portability in mind, allowing Harmony to run on Intel, AMD, and ARM chips across Linux (KVM) and macOS (HVF),
including within nested virtualization, or even as a user-mode Linux process.

* `dissonance`: a chaotic exploration tool that takes your code through adversarial conditions trying to find bugs.

The components are designed to be independent. For instance, if you wanted to use `consonance` as an off-the-shelf deterministic
hypervisor — there aren’t many of them! — you are welcome to.

Harmony is inspired by [Antithesis](https://antithesis.com/), whose engineering team I greatly admire. Their pioneering work has
made me want such testing capability for my own side projects, and since I cannot personally afford an enterprise SaaS contract, I started
to build Harmony instead as an experiment. It’s been a fascinating way to explore the problem space and future of software testing &
quality more deeply.

> [!IMPORTANT]
> As you will likely be able to tell, outside of this intro, Harmony is heavily AI-authored. This is rather fundamental, as Harmony both takes on massive
technical scope and is built as a passion project that fits within the quiet gaps of my life outside of family, friends, and work. While I cannot yet vouch
> for the state of the repo at any given moment in time, what I can say of the project, is that a lot of thinking, design, effort, hardware, and tokens have
> gone into, and will continue to go into, making Harmony something of value, both for myself, and perhaps, for you too.

---

> [!WARNING]
> Everything you read after this point, including any linked docs, has been written by an LLM. Apologies in advance.

## User documentation

The [Harmony documentation site](https://ph14.github.io/harmony/) covers installation,
OCI workloads, platform compatibility, SDK integration, fault search, and replay.
Start with [installation](docs/user/how-to/install.md) or the
[first-run tutorial](docs/user/tutorials/first-run.md).

## Try the CLI

```sh
cargo build --release -p harmony-cli
./target/release/harmony check
```

Prepare a language project or point directly at an OCI image:

```sh
harmony init --language rust
harmony prepare
harmony search --name baseline --for 10m
harmony show baseline
harmony branch baseline --finding 1 --rewind 10 --shell --name debugging
harmony search --from debugging --name neighborhood
harmony branch baseline --finding 1 --rewind 1 --stop --name earlier
harmony search --from earlier --name neighborhood
```

`harmony.toml` describes services, readiness, test traffic, assertions and control
hooks. Inline TOML and direct image inputs work too. The CLI chooses KVM, HVF or
UML and keeps prepared artifacts for exact replay. NES files select the native
game package: `harmony search game.nes`, with the runner library configured in
`runner.options.core`. Workload inputs, runner options and search budgets have
separate TOML tables.
See [CLI documentation](cli/README.md) for preparation, runtime provisioning,
guest commands, interactive debugging and whole-search continuation.

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
- [Performance](docs/PERFORMANCE.md) estimates how fast a search could run on
  a chip and explains where measured performance falls short.

Component details live in READMEs beside their code. Development setup and
repository checks live in [CONTRIBUTING.md](CONTRIBUTING.md).

Before filing a GitHub issue, ask: **“If we discarded this branch today,
would this still need attention?”** If not, keep the finding in the current task.

## Repository map

- [`consonance/`](consonance/README.md) contains the deterministic VMM, machine
  models, snapshots, guest protocols, Linux guest environment, and
  snapshot/control runtime.
- [`dissonance/`](dissonance/README.md) contains the campaign engine, search
  archive, and target interfaces.
- `workloads/` contains NES adapters, guest payloads, fault tooling, and package
  preparation.
- `scripts/` contains repository-level development and validation helpers.

Repository lint ownership and scope, including Cargo binary entrypoints and
their helper modules, are specified in the [CI contract](docs/WORKFLOWS.md).

Consonance and Dissonance build independently. Standalone workload crates consume
their interfaces; the CLI composes them. The [Cargo dependency policy](scripts/dependency-boundaries.toml)
and its required CI check enforce the direction of dependencies, including
optional, target-specific, build, and development dependencies. Consonance owns
opaque input transport and deterministic state. Fault definitions and policies
are supplied by workload tooling.

## License

Harmony is free software licensed under the GNU Affero General Public License
v3.0 or later (`AGPL-3.0-or-later`). See [LICENSE](LICENSE).
