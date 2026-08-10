# AGENTS.md — harmony

A deterministic, Antithesis-style KVM hypervisor: the same seed yields **bit-identical**
execution, so any run can be recorded and replayed exactly. Determinism is the entire
reason this project exists; every other property is subordinate to it.

This file is standing context for AI agents working in or reviewing this repo (including
`codex review`). It defines what "correct" means here so a review is project-aware, not
generic.

## What correct means

- **Determinism is the bar.** Anything that can make two same-seed runs diverge is a
  defect: wall-clock time, real frequency/TSC, host entropy (`RDRAND`/`RDSEED` not routed
  to the seeded stream), unseeded RNG, `HashMap`/`HashSet` iteration order reaching
  output/hashes/encoded bytes, floating point in state-affecting code, host identity
  (CPU/microcode/topology), async host events (interrupts, PMU) leaking into guest-visible
  state. The V-time clock is **retired branches** (count-based, frequency-independent) — not
  wall time.
- **Library code must never panic on untrusted input.** Every length, index, or enum that
  arrives from the transport, the host, or a decoded frame is untrusted: unchecked slicing
  or arithmetic on it is a panic reachable from untrusted input, and a bug, even when the
  happy-path tests pass.
- **Frozen surfaces are contracts.** The `cargo-public-api` snapshots and the determinism
  contract's normative tables bind the implementation; the implementation conforms, it does
  not negotiate. Cross-check shared constants against `docs/INTEGRATION.md`.
- **Single-tenant, pinned, homogeneous host.** The determinism foundation is an identical,
  pinned-core, single-tenant host (see `docs/CPU-MSR-CONTRACT.md`). The guest is
  **cooperative** (a CPUID-respecting Linux payload); an adversarial guest executing
  hidden/un-trappable opcodes is a documented residual risk, not a guaranteed closure —
  unless a hard mechanism (CPUID + CR4 ownership + VMX control + MSR filter) actually makes
  the op unreachable.

## When reviewing changes (highest-value findings first)

1. **Determinism leaks** — the list above. A single un-closed leak vector is blocking.
2. **Contract / frozen-surface conformance** — public API drift, or a determinism-contract
   table whose three representations (prose spine, per-class fragments, machine-readable
   TOML) disagree, or a disposition that doesn't actually close the leak it claims.
3. **Panics reachable from untrusted input** — follow every host/transport/decoded value to
   its use.
4. **Gate vacuity** — a green gate is the floor, not the bar. Does a test/proof/CI job
   actually *catch* the regression it claims, or can it pass vacuously (a test that always
   holds, a coverage/mutation/proof config weaker than it looks, a measurement that counts
   unverified samples, a CI job that skips silently)? Quality must **ratchet up**, never
   drift down — a lowered floor, relaxed lint, or skipped tool the code plainly calls for is
   a finding, not a nit.
5. **`unsafe` ⇒ Miri.** **Any crate containing `unsafe` must run clean under Miri.**
   Behavioral tests cannot see undefined behavior that does not surface as a wrong value or
   panic — out-of-bounds reads that return plausible bytes, pointer-provenance violations,
   aliasing. Run `cargo +nightly miri test -p <crate>` as part of the review and treat a Miri
   error as blocking; a crate that adds `unsafe` without a Miri-exercisable test path (the
   privileged/asm bits behind a seam so the unsafe logic runs under the interpreter) is itself
   a finding. The quality.yml `miri` job records the toolchain pin and `MIRIFLAGS`; new
   `unsafe` crates are added to that job's `-p` list.
6. **Enforcement implementability** — when the design says it "traps" or "pins" something,
   check the named mechanism actually exists on the assumed backend (e.g. stock Linux/KVM
   exposes a userspace exit for MSRs via the MSR filter, but **not** for `RDTSC`/`RDRAND`/
   `RDSEED`). An unimplementable enforcement assumption is blocking or a `[question]` for the
   integrator.

**Settled rulings (cite, don't re-litigate).** Some findings have already been ruled by the
integrator; a review that re-raises one should cite the ruling rather than re-open it.
Currently settled:

- **arm64 interrupt delivery is deferred by design.** The stock `Arm64KvmBackend` wiring
  **no** delivery fabric — `set_pending_irq`/inject are `Unsupported`, it never creates an
  in-kernel `KVM_DEV_TYPE_ARM_VGIC_V3`, and the DTB advertises the GICv3 while the skeleton
  claims **no** interrupt-driven guest boot — is the ruled design, not a defect.

Report each finding as `file:line` + severity (blocking/suggestion/question/nit) + the
concrete input or scenario that triggers it. If nothing is real, say so — don't pad.

## Build & gates

Rust workspace (edition 2024, stable toolchain from `rust-toolchain.toml`). All of these
must pass before work is done:

```sh
cargo build --all-features
cargo nextest run --all-features                       # subsumes `cargo test`
cargo clippy --all-features --all-targets -- -D warnings
cargo fmt --all -- --check
cargo deny check                                       # advisories, bans, licenses, sources
```

plus the quality toolchain in `docs/CODE-QUALITY.md` (coverage floor,
`cargo-mutants --in-diff`, proptest ≥256 cases, `proptest-state-machine`, Kani proofs,
`cargo-public-api` snapshots). Both macOS and Linux must pass (portability). The external
quality binaries (`cargo-nextest`, `cargo-llvm-cov`, `cargo-mutants`, `cargo-deny`,
`cargo-public-api`) install via `scripts/install-quality-tools.sh`; they are tools, not
crate dependencies. The gate of record is `.github/workflows/quality.yml`; the `.githooks`
pre-push hook runs the fast subset locally.

Clippy enforces the workspace `clippy.toml` determinism lints (disallowed
`Instant::now`/`SystemTime::now`/`thread_rng`/`random` and `HashMap`/`HashSet` types). A
legitimate lookup-only use is allowed with `#[allow(clippy::disallowed_{types,methods})]`
plus a `// not order-observable:` justification; an order-into-output use is a bug, not a
thing to silence.

**Miri** (the `unsafe` ⇒ Miri rule above): structure crates so the unsafe pointer logic is
reachable under the interpreter — privileged/`asm!` paths sit behind a seam and are
`#[cfg(not(miri))]`-excluded, with the unsafe logic driven by an in-process loopback. Reduce
proptest cases under `cfg!(miri)` so the interpreted suite stays quick.

**Cross-arch discipline:** cfg-gate x86 substrate code on the **arch**, not just the OS
(`all(target_os = "linux", target_arch = "x86_64")`) — `kvm_bindings` exposes different
types per arch, and the aarch64 cross-clippy job in quality.yml exists to catch exactly
this. A Mac-only run cannot see `cfg(linux)` breakage; run the Linux gates before calling
shared-enum or cfg(linux) work done.

## Conventions

- **Determinism discipline** (the reason the project exists — never introduce any): never
  iterate a `HashMap`/`HashSet` where order can reach an output, a hash, or an encoded byte
  (use `BTreeMap` or sort); no floating point in anything that affects state; no wall-clock
  time; no `rand` without a caller-provided seed.
- **Dependency whitelist** (ask in the PR description if you truly need more): `thiserror`,
  `zerocopy`, `proptest`, `sha2`, `blake3`, `serde`+`serde_json` (std crates only), `clap`
  (bins only), `memmap2`, `tempfile`, `rustix`, `libc`; dev-deps also
  `proptest-state-machine` and `arbitrary`. Pin nothing; use caret defaults.
- **Portability.** Portable crates build and pass all gates on both macOS and Linux. No
  Linux-only syscalls/APIs outside the explicitly Linux-gated backend code; use `tempfile` +
  `memmap2` for mapped/file-backed memory. See `docs/BUILDING.md`.
- **No `unsafe`** without a named purpose; every `unsafe` block gets a `// SAFETY:` comment,
  and the crate joins the Miri gate.
- Errors: `thiserror` enums, no `anyhow` in library code, no `.unwrap()`/`.expect()` outside
  tests except statically-infallible cases (commented).
- Document every public item; crate-level doc comment explains the component's role in one
  paragraph.
- **Spikes are worktrees, not directories.** Exploratory work happens on a `spike/<name>`
  branch in its own `git worktree`, and never merges to `main` as a `spikes/` folder; it
  either graduates into a real crate/module or the branch dies. (The existing `spikes/`
  entries predate this rule and leave with the code that depends on them.)

## Issue tracking

Bugs and follow-up work live in **GitHub issues** (`gh issue list`). Project coordination
happens outside this repo. Historical note: through 2026-08 the repo carried a task-spec
system (`tasks/`), a beads issue database (`.beads/`), and agent-coordination
configs/skills; `tasks/NN` and `hm-*`/bead references in old comments and commit messages
resolve via git history (removed at the `restructure:` commits of 2026-08-10).

## License

Harmony is licensed **AGPL-3.0-or-later** (see `LICENSE`); every crate carries
`license = "AGPL-3.0-or-later"` and every first-party source file carries an
`SPDX-License-Identifier: AGPL-3.0-or-later` header — `//` for Rust, `#` (after the
shebang) for shell and Python. New first-party files must carry it. The lone exception
is `harmony-linux/linux/init.sh`, which is baked verbatim into the determinism-hashed initramfs
(`harmony-linux/linux/MANIFEST.sha256`); a header line would change that golden, so it carries
no inline header and is covered by the repo `LICENSE`. The patch series under
`consonance/vmm-backend/kvm-patches/patches/` are GPLv2 Linux-kernel diffs and keep their
own headers. `cargo deny check licenses` gates dependency compatibility (only
AGPL-compatible licenses are allowed) — for the root workspace and, via the
`cargo deny (guest + fuzz manifests)` CI step, the out-of-workspace manifests too. The
AGPL §13 network-use obligation applies to anyone hosting a modified version.
