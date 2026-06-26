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
- **Specs are contracts.** A task's Public API section (exact names, types, semantics) and
  the determinism contract's normative tables bind the implementation; the implementation
  conforms, it does not negotiate. Cross-check shared constants against `docs/INTEGRATION.md`.
- **Single-tenant, pinned, homogeneous host.** The determinism foundation is an identical,
  pinned-core, single-tenant host (see `docs/BOX-PINNING.md`, `docs/CPU-MSR-CONTRACT.md`).
  The guest is **cooperative** (a CPUID-respecting Linux payload); an adversarial guest
  executing hidden/un-trappable opcodes is a documented residual risk, not a guaranteed
  closure — unless a hard mechanism (CPUID + CR4 ownership + VMX control + MSR filter)
  actually makes the op unreachable.

## When reviewing changes (highest-value findings first)

1. **Determinism leaks** — the list above. A single un-closed leak vector is blocking.
2. **Contract / spec conformance** — public API drift, or a determinism-contract table whose
   three representations (prose spine, per-class fragments, machine-readable TOML) disagree,
   or a disposition that doesn't actually close the leak it claims.
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
   a finding. The quality.yml `miri` job and the crate's IMPLEMENTATION.md record the toolchain
   pin and `MIRIFLAGS`; new `unsafe` crates are added to that job's `-p` list.
5. **Enforcement implementability** — when the design says it "traps" or "pins" something,
   check the named mechanism actually exists on the assumed backend (e.g. stock Linux/KVM
   exposes a userspace exit for MSRs via the MSR filter, but **not** for `RDTSC`/`RDRAND`/
   `RDSEED`). An unimplementable enforcement assumption is blocking or a `[question]` for the
   integrator.

Report each finding as `file:line` + severity (blocking/suggestion/question/nit) + the
concrete input or scenario that triggers it. If nothing is real, say so — don't pad.

## Build / gates

Rust workspace. Standard gates: `cargo build --all-features`, `cargo test --all-features`,
`cargo clippy --all-features --all-targets -- -D warnings`, `cargo fmt -- --check`, plus the
quality toolchain in `docs/CODE-QUALITY.md` (coverage floor, `cargo-mutants --in-diff`,
proptest ≥256, `proptest-state-machine`, Kani proofs, `cargo-public-api` snapshots,
`cargo-deny`). Both macOS and Linux must pass (portability). Box-executed work must be
CPU-pinned (`docs/BOX-PINNING.md`).

## License

Harmony is licensed **AGPL-3.0-or-later** (see `LICENSE`); every crate carries
`license = "AGPL-3.0-or-later"` and every first-party source file carries an
`SPDX-License-Identifier: AGPL-3.0-or-later` header — `//` for Rust, `#` (after the
shebang) for shell and Python. New first-party files must carry it. The lone exception
is `guest/linux/init.sh`, which is baked verbatim into the determinism-hashed initramfs
(`guest/linux/MANIFEST.sha256`); a header line would change that golden, so it carries
no inline header and is covered by the repo `LICENSE`. The patch series under
`consonance/vmm-backend/kvm-patches/patches/` are GPLv2 Linux-kernel diffs and keep their
own headers. `cargo deny check licenses` gates dependency compatibility (only
AGPL-compatible licenses are allowed) — for the root workspace and, via the
`cargo deny (guest + fuzz manifests)` CI step, the out-of-workspace manifests too. The
AGPL §13 network-use obligation applies to anyone hosting a modified version.
