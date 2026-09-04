# Contributing to Harmony

Harmony is a Rust project with a root workspace for consonance and a separate
workspace in `dissonance/`. The checked-in toolchain and CI configuration are
the authorities for supported versions and validation commands.

## Setup

Install Rust with [rustup](https://rustup.rs/). The repository's
`rust-toolchain.toml` selects the project toolchain automatically.

Install the external Cargo tools used by the quality gates:

```sh
scripts/install-quality-tools.sh
```

The bare-metal acceptance payloads additionally use the
`x86_64-unknown-none` target:

```sh
rustup target add x86_64-unknown-none
```

Component READMEs describe any platform-specific prerequisites and commands.

## Root workspace

Run the standard checks from the repository root:

```sh
cargo build --all-features
cargo nextest run --all-features
cargo clippy --all-features --all-targets -- -D warnings
cargo fmt --all -- --check
cargo deny check
```

The pre-push hook runs the fast subset. `.github/workflows/quality.yml` defines
the complete portable gate, including coverage, mutation tests, formal checks,
public-API snapshots, cross-architecture checks, and standalone guest crates.

## dissonance workspace

Run dissonance's checks against its independent manifest:

```sh
cargo fmt --manifest-path dissonance/Cargo.toml --all -- --check
cargo clippy --locked --manifest-path dissonance/Cargo.toml \
  --release --all-features --all-targets -- -D warnings
cargo test --locked --release --manifest-path dissonance/Cargo.toml --all-features
cargo deny --manifest-path dissonance/Cargo.toml check
```

## Documentation and follow-up work

Keep project concepts in `docs/` and component details in the nearest README.
Describe the current system rather than recording the sequence used to build
it. Git commits and pull requests preserve implementation history, while
GitHub issues hold future work.
