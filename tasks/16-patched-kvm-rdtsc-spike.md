# Task 16 — `spikes/patched-kvm-rdtsc/`: patched-KVM RDTSC/RNG interception feasibility spike

> **Historical (task 91):** the GO'd patch series + build recipe this spike produced now live,
> tracked, at `consonance/vmm-backend/kvm-patches/` (next to `patched_kvm.rs`). The throwaway
> measurement crate is not retained; paths below describe the spike as it was originally built.

Read `tasks/00-CONVENTIONS.md` first. Touch only `spikes/patched-kvm-rdtsc/`. This directory is
**not** part of the cargo workspace; it has its own `Cargo.toml`, its own gates, and a
`patches/` subdirectory holding the kernel patch series. Spike code is throwaway by design — the
deliverables are **the patch series, the build recipe, the measured numbers, and a GO/NO-GO**.

## Environment

- Runs on: **Linux bare-metal x86-64 Intel (VMX) only** — the determinism box, reached as
  `ssh <det-box>`. Nested virtualization does **not** satisfy this. macOS is your terminal/editor
  only; the kernel build, module swap, and all experiments happen on the box.
- Requires: `/dev/kvm`, Intel VMX, the **pinned kernel source `linux-6.18.35`** (the tag the
  CPU/MSR contract is written against — `docs/CPU-MSR-CONTRACT.md` cites it), kernel build
  toolchain, and root (module load/unload). The vCPU thread is **CPU-pinned per
  `docs/BOX-PINNING.md`** (dedicated physical core, SMT sibling idle) for every measured run.
- **Patched-module hygiene (load-bearing — this swaps a live kernel module on a shared box):**
  the spike builds patched `kvm.ko` + `kvm-intel.ko` (out-of-tree against the pinned tag) and
  hot-swaps them (`rmmod kvm_intel kvm` → `insmod` the patched pair). This requires **no other
  KVM workload active** during the window (the CI runner runs cargo gates, not VMs — confirm
  nothing else holds `/dev/kvm`). `RESULTS.md` MUST document: the exact build commands, the
  swap procedure, and a **tested revert to the stock modules** (`modprobe -r` → stock
  `modprobe kvm_intel`). Leave the box on **stock KVM** when the spike window ends — never leave
  a patched module loaded. If a reboot is needed to recover, say so.
- **Fail fast, never skip**: every gate script detects an unsupported host (no VMX, capability
  bit absent, wrong kernel) and fails with what's missing and where to run it — never silently
  pass. A clean, evidenced **NO-GO** is a successful spike (see Acceptance).
- Does not require: QEMU, the guest Linux image, the `vmm-backend`/`vmm-core` crates.

## Context

`docs/R-BACKEND.md`'s optionality ladder (stock `KvmBackend` → **`PatchedKvmBackend`** →
`DirectVmxBackend`) **bets** that stock KVM's RDTSC/RNG holes can be closed with a small,
auditable kernel patch rather than owning the VMCS ourselves. `docs/CPU-MSR-CONTRACT.md` §1 and
`docs/BRINGUP.md` are explicit that **stock KVM executes `RDTSC`/`RDTSCP`/`RDRAND`/`RDSEED`
in-guest with no `KVM_EXIT`** — the backend cannot make them deterministic, so M1/M2 are limited
to audited RDTSC/RNG-free payloads. Determinism-*completeness* (any real guest) requires
intercepting those instructions and supplying `RDTSC = f(V-time)` and RNG from a seeded stream.

This spike proves — **before** anyone implements `PatchedKvmBackend` — that the patch is
feasible, minimal, and behaves deterministically. If it is **not** feasible, we learn now and the
architecture falls back to `DirectVmxBackend` (a far larger lift); discovering that after M2 would
be expensive. The mechanics it must confirm (Intel SDM Vol. 3C §24.6):

- **RDTSC exiting** — Primary Processor-Based VM-Execution Controls, **bit 12**. Set ⇒ `RDTSC`
  and `RDTSCP` VM-exit (basic exit reasons 16 / 51). Stock KVM normally services TSC in-kernel
  via offset/scaling and does **not** surface it; the patch must enable the control **and route
  the exit to userspace** without breaking the TSC machinery we still depend on.
- **RDRAND exiting** — Secondary Processor-Based VM-Execution Controls, **bit 11** (exit reason
  57). **RDSEED exiting** — Secondary controls **bit 16** (exit reason 61). Cleaner: dedicated
  exit reasons, no in-kernel servicing to displace.

The userspace-surfacing shape should mirror the **already-ratified `vmm-backend` contract**
(`tasks/14-backend.md`): each intercept becomes an `Exit::Rdtsc`/`Rdtscp`/`Rdrand`/`Rdseed`, the
VMM supplies the value via `complete_read(value)`, and KVM writes the destination register(s)
(`EDX:EAX`, `RDTSCP`'s `ECX=IA32_TSC_AUX`, RNG `CF=1`) and advances RIP on re-entry. The spike's
job is to prove a KVM exit can carry that round-trip.

## Deliverable

`spikes/patched-kvm-rdtsc/` containing:

- **`patches/`** — a `git format-patch` series (`0001-*.patch`, …) against the **`linux-6.18.35`
  tag**, applied with `git am`. Keep it **minimal and reviewable** — this is trusted, audited
  determinism surface, not a kernel fork. Each patch commit message states what it changes and
  why. The series must apply cleanly to a fresh checkout of the tag (a gate).
- **`BUILD.md`** — the exact apply → build (out-of-tree `kvm.ko`/`kvm-intel.ko` against the
  pinned tag) → load → revert recipe, copy-pasteable, tested on the box.
- A small Rust harness on `kvm-ioctls`/`kvm-bindings` + `libc` (dependency whitelist for this
  directory extends to those + `rustix`; **`unsafe` granted** for KVM FFI + guest-memory
  mapping, each with a `// SAFETY:` comment) that creates a VM on the patched KVM, runs tiny
  flat guest stubs, and drives the new exits.
- Tiny **deterministic guest stubs** (flat binaries) that each execute a statically-known
  sequence of `RDTSC`, `RDTSCP`, `RDRAND`, `RDSEED` (include a **CPL3 phase** — the production
  guest runs these mostly in userspace) and write a result to a port/MMIO the harness reads.
- **`RESULTS.md`** with every experiment's raw numbers, the patch summary, and the verdict.
- **`run-all.sh`** (shellcheck-clean, fail-fast) that, given the patched modules are loaded,
  runs every experiment end-to-end and regenerates the numbers.

## Experiments (normative — RESULTS.md must report each)

1. **Capability check (no patch needed).** Read `IA32_VMX_PROCBASED_CTLS` (bit 12 RDTSC-exiting)
   and `IA32_VMX_PROCBASED_CTLS2` (bit 11 RDRAND-exiting, bit 16 RDSEED-exiting) on the box's
   CPU; confirm each control is **settable** (allowed-1). Report the raw MSR values and the CPU
   model. If any control is not settable, that surface is an immediate **NO-GO** for the patched
   approach on this hardware — record it and continue with the others.
2. **Stock-KVM baseline.** On **stock** KVM, run a stub that executes each instruction and
   confirm there is **no** `KVM_EXIT` for it (the guest reads a host value and proceeds) — i.e.
   reproduce the contract's stated stock behavior, so the patched delta is unambiguous.
3. **The patch.** Author the minimal patch series: enable the three exiting controls and route
   each VM-exit to userspace as a new `KVM_EXIT_*` (carrying the instruction + a field for the
   userspace-supplied value, plus a completion path that writes the dest register(s) and
   advances RIP on re-entry). Document the chosen exit-reason names and the `kvm_run` struct
   shape. **`RDTSC`/`RDTSCP` interaction with KVM's TSC machinery is the subtle part** — state
   exactly how the patch coexists with (or takes over) TSC offset/scaling and the TSC-deadline
   path, and what it leaves untouched.
4. **Build + load (revertible).** Build the patched modules against the pinned tag, hot-swap
   them, and confirm a clean stub still boots on the patched KVM. Confirm the **revert** to stock
   modules works. Both procedures go in `BUILD.md`.
5. **RDTSC/RDTSCP round-trip.** Guest executes `RDTSC`/`RDTSCP` → harness receives the userspace
   exit → supplies a chosen 64-bit value → guest resumes and the harness verifies the guest read
   **exactly the injected value** in `EDX:EAX` (and `RDTSCP`'s `ECX` = the injected `TSC_AUX`),
   with RIP advanced past the instruction. ≥ 100 trials; 100/100 must carry the injected value.
6. **RDRAND/RDSEED round-trip.** Same, for `RDRAND`/`RDSEED`: the harness supplies a `width`-byte
   value, verifies the guest destination register holds it and `CF = 1` (deterministic success),
   RIP advanced. ≥ 100 trials, 100/100.
7. **Determinism.** Run a stub twice with the **same** scripted injected values; assert the full
   guest architectural state (registers + the result bytes it wrote) is **bit-identical** across
   the two runs. This is the property the whole patched-backend story rests on.
8. **Exit cost.** Measure the VM-exit→userspace→re-entry round-trip cost **per intercepted
   instruction** (median/p99 over ≥ 1000 exits), separately for RDTSC vs RNG. This number feeds
   the contract's deferred-RDTSC-optimization question (R-Backend observability / `exit_counts`):
   a guest that RDTSCs in a hot loop pays this each time.

## Acceptance gates

1. `RESULTS.md` reports all **eight** experiments with raw numbers, the kernel version, CPU
   model, the patched-module build/load/revert evidence, and one-command reproduction
   (`./run-all.sh`, with the patched modules loaded).
2. The `patches/` series **applies cleanly to a fresh `linux-6.18.35` checkout** (`git am`) and
   the out-of-tree modules **build** with the documented commands — a reviewer will re-apply and
   re-build. A series that doesn't apply/build fails regardless of claims.
3. Experiments 5–7 show **100/100 conforming trials** (injected value reaches the guest; RIP
   advances; two runs bit-identical) for every surface whose control was settable in experiment
   1 — or the verdict for that surface is **NO-GO** with the failing evidence.
4. `RESULTS.md` ends with an explicit verdict from **GO / CONDITIONAL-GO / NO-GO** against
   "can a minimal KVM patch deterministically intercept RDTSC/RDTSCP/RDRAND/RDSEED and surface
   them to userspace." Any surface measured on a proxy or left unproven is named; a clean,
   evidenced NO-GO (e.g. the TSC machinery can't be cleanly displaced) is a **successful spike**
   and must name the failing property with evidence.
5. The box is left on **stock KVM** (revert verified); `run-all.sh` is shellcheck-clean and
   fail-fast; `cargo build/clippy -D warnings/fmt --check` on `spikes/patched-kvm-rdtsc/` pass.

## Non-goals

The production `PatchedKvmBackend` crate (a later task — it will *consume* this patch + verdict
and implement `tasks/14-backend.md`'s trait); wiring into `vmm-core`; the `DirectVmxBackend`
fallback; AMD/SVM; multi-vCPU; performance tuning of the patch beyond the experiment-8
measurement; upstreaming the patch. Keep the patch minimal and the harness throwaway — write
`RESULTS.md` as if the harness code were already deleted (the **patch series + verdict** are what
survive).
