# Conventions for all tasks

You are implementing one component of a deterministic hypervisor (in the mold of
Antithesis's "Determinator"): a KVM-based VMM that runs a single-vCPU Linux guest fully
deterministically — same seed ⇒ bit-identical execution — with virtual time derived from
performance counters, hypercall-only I/O, and copy-on-write snapshot/branching. You are
building **one crate (or one directory) in isolation**. Other components are built by other
workers in parallel; integration happens later and is not your concern.

## Task classes

Most tasks below are **delegable**: one crate/directory, gates run laptop-side, no box needed.
A second class exists and covers roughly two-thirds of the recent queue (tasks 41–57, and any
future KVM bring-up / integration work):

- **Frontier tasks** — box-only (need `/dev/kvm`, a real bare-metal box; see `docs/BOX-PINNING.md`
  and `docs/BUILDING.md`'s capability matrix). A frontier task's spec names an explicit **surface
  list** of the crates/dirs it may touch — that list is the boundary in place of hard rule 1
  below, since integration work legitimately spans `consonance/vmm-core` plus whichever crates it
  wires together. Frontier gates are the **box gates** (real KVM run, live boot, `state_hash`
  determinism check, etc., run over SSH per `docs/box-access` conventions) **plus** the
  **portable-logic gates** (the standard suite below) for any pure-logic code the task also
  touches. A frontier spec must still be **runnable from the repo**: box paths, SSH hosts, and
  environment tags belong in the spec's **Environment** section, not scattered through the prose
  as ad hoc asides — a reader should be able to tell, from one place, exactly what needs the box
  and what doesn't.

Everything else in this document (hard rules, gates, style, deliverable) applies to both classes;
"your directory" for a frontier task means its spec's surface list, not a single crate dir.

## Hard rules

1. **Touch only your directory** (or, for a **frontier task**, your spec's named surface list —
   see Task classes above). Your task file names it — `consonance/<crate>/` for the
   deterministic hypervisor (the substrate / engine), `dissonance/<crate>/` for the bug finder
   built on it (e.g. `consonance/snapshot-store/`, `dissonance/explorer/`). The root `Cargo.toml`
   already globs both `consonance/*` and `dissonance/*`, so your crate joins the workspace just by
   existing under its dir — you never edit root files.
2. **Define interfaces locally.** If your spec includes traits that other components will
   implement later, define them in *your* crate exactly as specified. Do not invent a shared
   "interfaces" crate or depend on any sibling crate.
3. **Public API is a contract.** Implement the signatures in your spec's "Public API" section
   exactly (names, types, semantics). You may add private items and additional helper methods,
   but do not remove, rename, or change the meaning of anything specified.
4. **Determinism discipline.** This project exists to eliminate nondeterminism; do not
   introduce any. Concretely: never iterate a `HashMap`/`HashSet` where order can reach an
   output, a hash, or an encoded byte (use `BTreeMap` or sort); no floating point in anything
   that affects state (use integer/fixed-point math as specified); no wall-clock time, no
   `rand` without a caller-provided seed; library code must never panic on untrusted input.
5. **Dependency whitelist** (ask-by-comment in your PR description if you truly need more):
   `thiserror`, `zerocopy`, `proptest`, `sha2`, `blake3`, `serde`+`serde_json` (std crates
   only), `clap` (bins only), `memmap2`, `tempfile`, `rustix`, `libc`. Pin nothing; use caret
   defaults. Dev-dependency additions: `proptest-state-machine` and `arbitrary`
   (quality-e and future fuzzing use them).
   The external quality *binaries* (`cargo-nextest`, `cargo-llvm-cov`,
   `cargo-mutants`, `cargo-deny`, `cargo-public-api`; installed via
   `scripts/install-quality-tools.sh`) are tools, NOT crate dependencies, and are
   exempt from this whitelist.
6. **Portability.** Delegated crates must build and pass all gates on **both macOS and
   Linux** — development happens on a Mac. No Linux-only syscalls/APIs (`memfd_create`,
   `userfaultfd`, `io_uring`, `/proc`, …); use `tempfile` + `memmap2` for mapped/file-backed
   memory. No `#[cfg(target_os)]` logic forks. See `docs/BUILDING.md` for setup and
   per-platform commands.
7. **No `unsafe`** unless your task file explicitly grants it for a named purpose (e.g. mmap);
   every `unsafe` block gets a `// SAFETY:` comment.

## Workflow: one worktree per task

Never work in the main checkout; one worktree per agent, never shared. Start:

```sh
git worktree add ../harmony-task-<crate> -b task/<crate-name>
cd ../harmony-task-<crate>
```

All commits go on `task/<crate-name>` and touch only your directory (rule 1). Hand off by
pushing the branch / opening a PR. Cleanup after merge is the integrator's job, not yours:

```sh
git worktree remove ../harmony-task-<crate>
git branch -d task/<crate-name>
```

## Gates (all must pass before you are done)

```sh
cargo build -p <your-crate> --all-features
cargo nextest run -p <your-crate> --all-features   # subsumes `cargo test`
cargo clippy -p <your-crate> --all-features --all-targets -- -D warnings
cargo fmt -p <your-crate> -- --check
cargo deny check                                   # advisories, bans, licenses, sources
```

Clippy now enforces the workspace `clippy.toml` (the determinism lints of rule #4:
disallowed `Instant::now`/`SystemTime::now`/`thread_rng`/`random` and
`HashMap`/`HashSet` types). A legitimate lookup-only use is allowed with
`#[allow(clippy::disallowed_{types,methods})]` plus a `// not order-observable:`
justification; an order-into-output use is a bug, not a thing to silence.

plus the task-specific gates in your spec. Task 04 (guest image) has its own non-cargo gates.
Property tests use `proptest` with at least 256 cases; keep total `cargo test` runtime under
~3 minutes.

**Any crate containing `unsafe` must also run clean under Miri** (the unsafe⇒Miri review-bar
rule, `AGENTS.md`):

```sh
cargo +nightly miri test -p <your-crate>    # pinned nightly + MIRIFLAGS per quality.yml's `miri` job
```

Miri catches undefined behavior that value/panic assertions cannot (out-of-bounds reads that
return plausible bytes, pointer-provenance violations, aliasing). Structure the crate so the
unsafe pointer logic is reachable under the interpreter — privileged/`asm!` paths (which Miri
cannot execute) sit behind a seam and are `#[cfg(not(miri))]`-excluded, with the unsafe logic
driven by an in-process loopback. Reduce proptest cases under `cfg!(miri)` so the (10–100×
slower) interpreted suite stays quick. Add the crate to the `miri` job's `-p` list in
`.github/workflows/quality.yml`.

## Style

- Rust edition 2024, stable toolchain (from `rust-toolchain.toml`).
- Errors: `thiserror` enums, no `anyhow` in library code, no `.unwrap()`/`.expect()` outside
  tests except for statically-infallible cases (commented).
- Document every public item; crate-level doc comment explains the component's role in one
  paragraph.
- Tests live next to the code (`#[cfg(test)]`) plus `tests/` for integration/property tests.

## Deliverable

A branch named `task/<crate-name>` containing only your directory, all gates green, and a
short `IMPLEMENTATION.md` in your directory noting: deviations considered and rejected, known
limitations, and anything the integrator must know. Do not open follow-on work; stop when the
gates pass.

**Where the write-up lives.** For a single-crate task that is the crate's `IMPLEMENTATION.md`,
nothing else. A frontier/multi-crate task has no single directory: its write-up goes in the
**PR description** (runbooks, evidence, judgment calls — the review record). Only when a
durable in-repo copy is genuinely needed (a runbook a later task must re-run, box evidence a
spec cites) does it become a file — at `docs/history/IMPLEMENTATION-task<NN>.md`, **never the
repo root** (cleanup ruling, 2026-07-15).
