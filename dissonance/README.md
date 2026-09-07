# Dissonance

Dissonance is a standalone workspace for deterministic exploration. Its `searcher`
crate owns archive retention, deterministic campaign scheduling, rollout mechanics,
input mutation machinery, stream recording, and replay.

Workload packages implement the search interfaces with typed actions, observations,
keys, and snapshots. The NES package and machine drivers live under `../workloads`.

```sh
cargo test --manifest-path dissonance/Cargo.toml
cargo clippy --manifest-path dissonance/Cargo.toml --all-targets -- -D warnings
```
