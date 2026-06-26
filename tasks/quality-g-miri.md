# Task quality-g — Miri UB validation for `unsafe` crates (+ no-GHA gate enforcement)

> **STATUS: IN PROGRESS.** A worker builds the *config*: the Miri gate (§1), the review-bar
> rule (§3), and the runner config — `quality.yml` (`runs-on: self-hosted` + a Miri job), the
> `ci.slice` cpuset unit, and a setup script (§2). All of that is verifiable now (Miri runs
> locally; the rest is config). **Two steps stay off the worker:** (a) the **operator
> registration** — the user obtains a one-time runner *registration token* (repo Settings →
> Actions → Runners; ephemeral, ~1h, never stored); (b) the **box-side install** — the foreman
> runs the setup over SSH given that token. **Secret handling (documented decision):** the only
> persistent secret is the runner's auto-managed `.credentials` in its install dir — a
> *repo-scoped, instantly-revocable* token the runner uses to poll GitHub. It is the **single,
> documented exception** to "the box holds no credentials"; there is no vault and nothing to
> rotate (register once, revoke by deleting the runner). The gate *jobs* (build/test/clippy/
> Miri/spikes) need **no** secrets — they are read-only checks.

## Why

`consonance/vmcall-transport` (task 10, merged) is the first crate with raw-pointer `unsafe`:
it materializes guest pages from raw `u64` GPAs (`*mut u8`) and copies bytes bounded by a
**host-controlled** length. Its behavioral coverage is strong (512-case proptests over
arbitrary `rax`/response bytes, the load-bearing `u64`-before-`as usize` bound check), but
*behavioral* tests cannot see **undefined behavior** that doesn't manifest as a wrong value
or panic — out-of-bounds reads with valid-looking results, pointer-provenance violations,
aliasing. **Miri** is the tool for that, and this crate is *designed* for it: the privileged
`VMCALL` lives behind the `VmExit` seam, so the **loopback tests exercise all the unsafe
pointer code with no inline asm** (Miri cannot interpret `asm!`).

This task adds Miri as a standing gate **and** fixes where gates run, since GitHub Actions
is paused (out of minutes — not to be paid for). Today the `quality.yml` suite isn't
executing on CI at all; gates have only been enforced by the foreman running them at review
time. This task makes that enforcement explicit and automatic without GHA.

## Deliverables

### 1. Miri gate

- Pin a **nightly** toolchain for Miri (`rust-toolchain`-style or documented), add the
  component (`rustup component add miri`).
- `cargo +nightly miri test -p vmcall-transport` must pass on the **asm-free loopback
  suite**. The real-`asm!` `RealVmcall` path is `#[cfg(...)]`-excluded under Miri (Miri
  rejects inline asm) — document this exclusion honestly.
- Miri is ~10–100× slower than native: cut proptest cases under it (e.g.
  `PROPTEST_CASES=16` when `cfg!(miri)`, or a `cfg(miri)`-gated constant) so the suite
  finishes in reasonable time while still exercising the bound-check and no-panic paths.
- **Non-vacuity proof (acceptance):** demonstrate Miri actually *catches* UB — temporarily
  inject an out-of-bounds read (e.g. read `rax+1` bytes past the bound), confirm Miri flags
  it, then revert. Record this in IMPLEMENTATION.md.

### 2. Gate enforcement — self-hosted runner on the box (chosen)

GHA-hosted minutes are paused (not to be paid for). Enforcement moves to a **self-hosted
GitHub Actions runner on the determinism box** (self-hosted runners consume *no* GHA minutes):

- **Reactivate `.github/workflows/quality.yml` with `runs-on: self-hosted`.** The box is
  Linux, so one runner covers the whole suite — the cross-platform gates (fmt, clippy
  `-D warnings`, nextest, public-api) **plus** the Linux/KVM-only ones (the spike harnesses)
  **plus the new Miri job** (`cargo +nightly miri test` on every `unsafe` crate). Add Miri as
  a job; keep the rest.
- **The runner MUST be cpuset-isolated per `docs/BOX-PINNING.md`** — run it in the systemd
  `ci.slice` (`AllowedCPUs=5-7,13-15`) so CI never shares a physical core or SMT sibling with
  the determinism measurements on cores 2/4. (Residual shared-L3/membw is fine for the
  contention-immune correctness gates; only the latency spikes 07/08 need a quiet box — see
  BOX-PINNING.md "Self-hosted CI runner isolation".)
- **Operator step (not a worker task):** registering the runner needs a registration token
  from the repo's Settings → Actions (a scoped, revocable credential on the box). The task
  delivers the `quality.yml` `self-hosted` config, the Miri job, the `ci.slice` unit, and an
  install/setup script; the human provisions the runner token.
- **Optional local fast-feedback: a pre-push hook.** A `.githooks/pre-push` (via
  `core.hooksPath`, installed by `scripts/install-quality-tools.sh`) running the *fast*
  cross-platform gates (fmt, clippy, nextest, Miri on `unsafe` crates) gives developers
  early signal before the runner does the full pass. Nice-to-have, not the gate of record.

### 3. Review-bar rule

- Add to `AGENTS.md` and the `pr-review` skill (§ test-sufficiency): **any crate containing
  `unsafe` must run clean under Miri**, and the reviewer runs `cargo +nightly miri test` on
  it as part of the gates. (My #23 review verified correctness four ways but didn't run
  Miri — this closes that.)

## Acceptance gates

- `cargo +nightly miri test -p vmcall-transport` passes (loopback suite, reduced cases).
- The injected-UB non-vacuity check is documented (Miri caught it).
- `quality.yml` is `runs-on: self-hosted` with a Miri job added; the `ci.slice`
  (`AllowedCPUs=5-7,13-15`) unit + setup script are committed, so the runner is
  cpuset-isolated per `docs/BOX-PINNING.md` (off measurement cores 2/4). Runner
  *registration* is the operator step.
- (optional) `.githooks/pre-push` runs the fast cross-platform gates incl. Miri for local
  fast-feedback.
- `AGENTS.md` + `pr-review` skill carry the unsafe⇒Miri rule.

## Scope / conventions

- Stays in `consonance/vmcall-transport/` (Cargo.toml dev-config + the `cfg(miri)` case
  reduction), `.githooks/`, `scripts/`, `tasks/00-CONVENTIONS.md` (if a new rule is added),
  `AGENTS.md`, `.claude/skills/pr-review/`, and `.github/workflows/quality.yml` (the paused
  note). No determinism/behavioral change to the crate itself.
- Honest about Miri's limits (no inline asm; slower; the real `RealVmcall` path is not
  Miri-covered — only the pointer/bound-check logic is).
