# Task 42 — `harmony-linux`: name the guest-environment tier (`guest/` → `harmony-linux/`) + audit consonance for workload assumptions

> **STRUCTURAL + AUDIT · land atomically · aims for zero runtime-behavior change.** Like the
> rename (task 90) this is a tree-wide path move that conflicts with any branch touching `guest/`,
> so land it as one PR when the `guest/`-touching queue is clear. Unlike task 90 it also carries a
> real **audit deliverable** (the consonance check) that may *spawn* follow-up work but does not
> itself change substrate behavior.

Read `tasks/00-CONVENTIONS.md` first. This task touches multiple top-level dirs (it is a move +
an audit, not an isolated crate), so the "touch only your directory" rule is explicitly waived —
the surface below is exhaustive on purpose.

## Why: name the third tier

The repo today has **two themed tiers plus a harness**:

- **consonance** — the deterministic substrate (single-vCPU KVM x86-64 VM, bit-identical replay).
- **dissonance** — the bug finder that permutes a guest running on consonance.
- **unison** — the determinism harness.

There is an unnamed third thing sitting *between* them: the **guest environment** — the specific
OS / orchestrator world that runs on consonance and exposes guest-level faults, output, and
monitoring up to dissonance. Today that thing is `guest/`: a minimal Linux (pinned kernel +
bare-metal Multiboot payloads + committed goldens). It has no tier name, which hides the most
important architectural fact about it: **it is one of many possible guest worlds.**

Name it **`harmony-linux/`** and make the tier explicit, so that someone can later build
`harmony-kubernetes/`, `harmony-docker-compose/`, `harmony-metal/`, or
`harmony-freebsd-nomad/` as **siblings** — each a self-contained guest world that:

1. is built **on top of consonance**, which never learns what runs inside it; and
2. is **explorable by dissonance** through a generic interface (consonance-level substrate
   controls + the environment's own published fault catalog), ideally without dissonance knowing
   it is a specialized Linux/Kube/metal guest.

### Naming convention (establish it here)

The three **core** tiers get single musical names: `consonance` / `dissonance` / `unison`. The
**swappable guest environments** get **`harmony-<env>`** names: `harmony-linux`,
`harmony-kubernetes`, … . The `harmony-` prefix is the signal — *"pluggable guest world, not core
engine."* Record this convention in the doc you touch (see F4 below).

## Deliverable A — the move (`guest/` → `harmony-linux/`)

A path-only sweep; **zero behavior change**. Surface (exhaustive — a half-moved tree is the
failure mode):

- **Move the dir**: `guest/` → `harmony-linux/` (keep internal layout: `payloads/ linux/ golden/
  scripts/ Makefile *.md`). `harmony-linux/payloads` stays its **own** cargo workspace (it builds
  bare-metal `x86_64-unknown-none`; the root workspace excludes it — see next).
- **Root `Cargo.toml`**: `exclude = ["guest"]` → `["harmony-linux"]` (line ~12). `members`
  (`["consonance/*", "dissonance/*"]`) is **unchanged** — `guest/` was never a member, only an
  exclude. Do **not** add `harmony-linux/*` to members (custom targets/linker — it must stay an
  excluded sub-workspace).
- **`.gitignore`**: `/guest/build`, `/guest/dl`, `!guest/golden/*.bin` → `harmony-linux/…`.
- **`.github/workflows/quality.yml`**: the `cargo deny --manifest-path guest/payloads/Cargo.toml`
  step (~line 64) and its comment (~line 59).
- **`deny.toml`**: the comment referencing `guest/payloads` (~line 39).
- **`AGENTS.md`**: the `guest/linux/init.sh` / `guest/linux/MANIFEST.sha256` references (~line 80).
- **Makefile entry points**: every `make -C guest …` invocation in docs/READMEs becomes
  `make -C harmony-linux …`.
- **`consonance/det-corpus`**: default/example paths `guest/payloads/*.bin`, `guest/golden/*.digest`
  in `corpus-manifest.example.toml`, `src/manifest.rs`, `src/oracle.rs` (doc), and the `tests/`
  fixtures → repath. (These are default strings + test fixtures; behavior-neutral. See F3.)
- **`consonance/vmm-core`**: the `guest/payloads` build steps and `guest/linux` artifact paths in
  the live tests (`tests/live_*.rs`) and `IMPLEMENTATION.md`; the `Cargo.toml` comment. (See F1/F2.)
- **`docs/`** (~15 files) and **`tasks/`** (~15 files): update live references. Historical task
  specs may keep `guest/` **only** where they are a record of past work — prefer a clean sweep,
  exempt by exception with an inline note (task-90 precedent).

### Determinism-sensitive landmine — do NOT casually rename the build root

`harmony-linux/linux/lib-build.sh` pins `BUILD_ROOT=/tmp/hypervizor-guest-build`, and an
**identical absolute build path is part of the reproducibility contract** for the
determinism-hashed kernel/initramfs (`MANIFEST.sha256`). The string is also stale (`hypervizor`).
Two acceptable resolutions, pick one and document it:

1. **Leave the `/tmp/hypervizor-guest-build` string as-is** (a deliberate, documented stale path)
   so the committed `MANIFEST.sha256` digests still reproduce bit-for-bit. *(recommended — keeps
   this task behavior-neutral.)*
2. Rename it (e.g. `/tmp/harmony-linux-build`) **and** rebuild + **rebaseline** `MANIFEST.sha256`
   and every dependent golden in the same PR, proving the new digests reproduce twice.

Do **not** rename the string without (2). `GUEST_BUILD_ROOT` the env-var name and the
`hypervizor-guest-build` *default* are different things — changing the default changes the hash.

## Deliverable B — audit: consonance must not assume its workload

**Principle under test:** consonance's contract is over the **machine** — CPUID/MSR/TSC/LAPIC/PIT/
CMOS, guest memory, the hypercall transport — and **never over what software the guest runs.** A
deterministic VM that *knows it is running Linux* has leaked the guest tier into the substrate.

Produce **`docs/CONSONANCE-WORKLOAD-AUDIT.md`** classifying every workload coupling found in
`consonance/`. For each: **severity** (substrate-violation / consumer-coupling / naming),
**behavior-neutral to fix?** (yes/no), **action** (fix-inline here / file follow-up task / leave
with note). Seed findings (grep-confirmed today — re-verify and extend):

- **F1 — headline, substrate-violation: the Linux bzImage loader lives inside the substrate.**
  `consonance/vmm-core/src/linux_loader.rs` (~1239 LOC) parses the Linux `setup_header` and lays
  down `boot_params` / page tables / GDT; it is wired into `bringup.rs` (image autodetect +
  `boot_linux`), `lib.rs` (`pub mod linux_loader`), and `vmm.rs` (`LinuxLoad` error variant).
  Turning *a Linux kernel image* into initial memory + entry state is a **harmony-linux** concern.
  The substrate's job is "place these opaque segments at these GPAs, set initial vCPU state, run."
  **Action:** file a follow-up task to introduce a workload-agnostic `load_image(segments,
  entry_state)` primitive in consonance and move the bzImage→segments transform into
  `harmony-linux/`, which calls it. This is a determinism-sensitive behavioral refactor (boot must
  stay bit-identical) → **its own task, not this one.** Record the proposed seam in the audit doc.
- **F2 — relocate-eventually: Linux/Postgres live tests in the substrate's suite.**
  `consonance/vmm-core/tests/live_{linux_boot,postgres,branching_demo,m1_m2}.rs` are integration
  tests *of the harmony-linux tier on top of consonance*, not substrate unit tests; they couple
  consonance's test suite to a specific workload. They are `#[cfg(target_os = "linux")] + #[ignore]`
  box-only, so harm is low today. **Action:** repath their `guest/` artifact references now;
  note in the audit that long-term they belong to `harmony-linux/` (depending on consonance as a
  normal dependency). Optional follow-up task.
- **F3 — consumer-coupling, acceptable: `det-corpus` default paths.** `det-corpus` legitimately
  *consumes* guest artifacts as a determinism corpus; the coupling is only default-path strings +
  fixtures. **Action:** repath (Deliverable A). A consumer depending on harmony-linux's artifacts
  is fine; flag only that the *defaults* assume the Linux layout (a future multi-environment
  corpus would parameterize them).
- **F4 — naming/definition: docs define consonance as "runs a real Linux guest."** `docs/DISSONANCE.md`
  ("runs a real Linux guest…") and `consonance/vmm-core/src/lib.rs` / `hypercall-proto` doc-comments
  bake Linux into the substrate's *definition*. **Action:** generalize the prose — consonance runs
  an **opaque** guest; "a real Linux guest" is the **harmony-linux** instantiation (the motivating
  first target), not part of the substrate's contract. Add the `harmony-<env>` naming convention here.
- **F5 — note, not a violation: the guest-cooperation ABI is correctly OS-agnostic.** Confirm
  `hypercall-proto` / `vmcall-transport` / `pv-net` / the `Environment` `decide` seam assume *a*
  cooperating guest (hypercalls + optional SDK) but **not which OS**. If any Linux-specific
  assumption leaks into the wire types or the fault catalog's *generic* parts, that is a finding;
  otherwise record it as confirmed-clean (this is the substrate/guest ABI, intentionally
  workload-agnostic).

The audit may add findings beyond these five; F1–F5 are the floor, not the ceiling.

## Sequencing & determinism

- **Land atomically** when the `guest/`-touching queue is clear (Deliverable A conflicts with any
  branch that moves under `guest/`). The audit doc + any follow-up task files ride the same PR.
- **Determinism-neutral — verify, don't assume.** The move must touch **no hashed input**:
  `contract_hash` is over the CPU/MSR canonical form (no paths); `state_hash` is runtime
  architectural state; `MANIFEST.sha256` hashes *built artifact bytes*, which the move does not
  change **provided the build-root string is preserved** (see the landmine above). Grep the hash
  inputs to confirm no path reaches them, and prove `make -C harmony-linux test-linux` still
  reproduces the committed `MANIFEST.sha256` twice.

## Acceptance gates

1. Full standard suite green **after** the move: `build` / `nextest` / `clippy -D warnings` / `fmt`
   / `deny` (incl. the `--manifest-path harmony-linux/payloads/Cargo.toml` deny step) / miri /
   coverage / mutants / public-api — all with the new paths.
2. `make -C harmony-linux test-payloads` (macOS + Linux) and `test-linux` (Linux/box) pass; the
   committed `MANIFEST.sha256` reproduces twice (determinism-neutrality of the move proven).
3. **No stragglers**: `git grep -nI 'guest/'` returns only intentional historical references
   (a CHANGELOG-style note, a deliberately-stale build-root string documented per the landmine) —
   zero live Cargo / CI / script / import / default-path references to the old `guest/` dir.
4. **`docs/CONSONANCE-WORKLOAD-AUDIT.md`** exists with F1–F5 (+ any new findings) each classified
   (severity / behavior-neutral? / action), and every "file follow-up task" action has a
   corresponding `tasks/NN-*.md` stub committed (at minimum: the F1 loader-extraction task).
5. `IMPLEMENTATION.md` (in `harmony-linux/`) or `docs/HARMONY-LINUX.md` records the move map, the
   build-root resolution chosen, the determinism-neutrality result, and the `harmony-<env>` tier
   convention.

## Non-goals

- **Actually extracting `linux_loader` out of consonance** — that is the F1 follow-up task, not
  this one (behavioral risk; determinism-sensitive). This task *audits and documents* the seam.
- Building `harmony-kubernetes` / any second guest environment — the vision is documented here, not
  implemented.
- Any runtime-behavior change; renaming concepts *inside* crates; the GitHub-repo / local-dir
  renames (integrator, out-of-band).
