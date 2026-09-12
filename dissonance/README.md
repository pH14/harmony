# Dissonance

Dissonance is a standalone workspace for deterministic exploration. Its `searcher`
crate owns archive retention, deterministic campaign scheduling, rollout mechanics,
input mutation machinery, stream recording, and replay.

## Design tenets

These tenets define the direction for new work and the criteria for evaluating
changes.

- Provide one state-exploration algorithm that works across workloads. Do not
  add configuration knobs for tuning its search behavior; make the algorithm
  adapt from the evidence and resources available during a campaign.
- Use CPU and memory efficiently.
- Make search throughput scale efficiently as more CPU cores are available.
- Remain workload-agnostic. Dissonance provides interfaces through which
  clients connect workloads; its internals never encode the concepts or
  vocabulary of a particular workload.

Workload packages implement the search interfaces with typed actions, observations,
keys, and snapshots. Workload adapters and execution drivers live under
`../workloads`.

Evaluation separates worker candidate keys from completed archive keys. The
rollout records `rollout_key`, then the coordinator completes it against the
candidate snapshot before admission; workloads can preserve existing job
digests while assigning ancestry-dependent fields during completion.

```sh
cargo test --manifest-path dissonance/Cargo.toml
cargo clippy --manifest-path dissonance/Cargo.toml --all-targets -- -D warnings
```
