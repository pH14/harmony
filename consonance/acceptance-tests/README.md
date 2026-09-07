<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Acceptance integration tests

`acceptance-tests` is the composition crate for tests that combine the
`acceptance-suite` oracle runner or the `telemetry` observer with the real
`vmm-core` bridge. Keeping these targets outside `vmm-core` lets the execution
core remain independent of application and test harness crates.

Portable bridge coverage runs with:

```sh
cargo test -p acceptance-tests --test corpus_oracle_mock
```

The patched-KVM corpus and k3s telemetry gates remain ignored and require their
documented host artifacts:

```sh
cargo test -p acceptance-tests --test box_corpus -- --ignored --nocapture
cargo test -p acceptance-tests --test live_k3s_postgres -- --ignored --nocapture
```
