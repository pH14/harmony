# IMPLEMENTATION.md — patched-KVM RDTSC/RNG interception spike

Originated as a throwaway feasibility spike (`tasks/16-patched-kvm-rdtsc-spike.md`);
the patch series it produced is the host-Linux KVM basis for the patched backend
(`../src/patched_kvm.rs`). The **retained** deliverables are **`patches/`** +
**`BUILD.md`** and the **GO** verdict recorded here; the Rust measurement harness,
guest stubs, and the raw results table were disposable and are **not retained** (the
load-bearing numbers are inlined below). This file records the decisions, deviations,
and what the integrator must know.

## What was proven

A minimal 3-commit KVM patch (+203/−2 lines) makes `RDTSC`/`RDTSCP`/`RDRAND`/
`RDSEED` VM-exit to userspace via a new `KVM_EXIT_DETERMINISM`, with a completion
path that writes the destination register(s) and advances RIP. It applies cleanly
to the pinned `linux-6.18.35` tag and builds; live (on a 6.12.90 proxy, see below)
it is 100/100 conforming for both TSC and RNG, bit-identical across runs, at
~3.4 µs (RDTSC) / ~3.8 µs (RNG) per intercept. **Verdict: GO.**

## The one big deviation: live experiments ran on a 6.12.90 proxy

The spec's environment section assumes the box runs the pinned `linux-6.18.35`.
It runs **`6.12.90+deb13.1-amd64`**. An out-of-tree module must match the running
kernel's vermagic to load, so the canonical pinned-tag build (vermagic `6.18.35-…`)
is **build-verified but not loadable** here.

**Options considered:**

1. **(chosen) Proxy on 6.12.90.** Author + build-verify the canonical series
   against `linux-6.18.35` (gate #2), and run the live round-trips against a
   6.12.90 build of the *same* change. Low risk (module swap only, reverted),
   real 100/100 numbers, proxy named in RESULTS. The VMX exec-control bits, exit
   reasons, and userspace-exit machinery are materially identical across 6.12→6.18,
   so the proxy is faithful (named as such here). Only code delta:
   `EXPORT_SYMBOL_FOR_KVM_INTERNAL` → `EXPORT_SYMBOL_GPL` (the namespaced macro is 6.16+).
2. **Reboot the box into a self-built 6.18.35 kernel** — fully faithful, but
   reboots a *shared* box into an unproven kernel with no remote console;
   **rejected** as disproportionately risky for a feasibility spike. The spec's
   hygiene section only contemplates swapping *modules*, not the kernel.
3. **Buildability-only, no live load** — weakest evidence (no 100/100). Rejected
   since the load path was open (no Secure Boot / lockdown) and the live numbers
   are the heart of the spike.

To erase the proxy later: re-build via `BUILD.md` Part 1 and re-run the `vmm-core`
box gates (`live_determinism`, `box_corpus`) on a host whose running kernel **is**
`linux-6.18.35` — Part 1 then produces directly-loadable modules (no proxy needed).

## Patch design decisions

- **Opt-in, default-off.** Gated on a per-VM cap (`KVM_CAP_X86_DETERMINISTIC_INTERCEPTS`,
  settable only before vCPU creation). With it off, behavior is byte-for-byte
  stock (exp 2 confirms 0 exits), and RDRAND/RDSEED keep stock `#UD` semantics.
  This keeps the patch safe to carry without affecting other KVM users.
- **Modelled on `KVM_EXIT_X86_RDMSR`.** The exit fills `kvm_run`, sets
  `complete_userspace_io`, and the completion callback writes registers + advances
  RIP on re-entry — the well-trodden MSR-filter round-trip, so reviewers recognize
  the shape and RIP/skip handling is not reinvented.
- **TSC machinery untouched.** Only the guest-RDTSC read is taken over (primary
  control bit 12). Offset/scaling and the in-kernel TSC-deadline timer (host TSC)
  are not displaced; kvmclock is unused by the pinned guest.
- **RNG dest decode in kernel.** The VMX exit qualification carries no operand
  for RDRAND/RDSEED, so the patch decodes dest reg + width from the trapped
  instruction bytes (`[prefixes][REX] 0F C7 /6|/7`) via `kvm_read_guest_virt`.
  Minimal and correct for the standard encodings; a reviewer should sanity-check
  the prefix loop if exotic encodings ever matter (they don't for the pinned guest).

## Harness notes (historical — the measurement harness is not retained)

These record how the spike originally exercised the ABI, for context; the harness
itself (a box-only Rust crate + guest stubs) was not moved into the tree.

- `kvm-ioctls`/`kvm-bindings`/`libc` only (per the spec's extended whitelist).
  `unsafe` is confined to guest-RAM mmap (`guestmem.rs`) and the raw `kvm_run`
  mmap + `KVM_RUN` ioctl (`vm.rs`), each with a `// SAFETY:` comment — needed
  because `kvm-bindings` has no `KVM_EXIT_DETERMINISM`, so the harness overlays
  the determinism payload on the `kvm_run` page by documented offset.
- The guest is dropped into 64-bit long mode with a real GDT so each stub runs a
  **CPL3 phase** via `iretq` (the production guest runs these mostly in
  userspace); results are written to a guest-memory buffer the harness reads.
- Determinism (exp 7) compares all 18 GPR words + the result bytes; injection
  values come from a seeded splitmix64 (no `rand`, reproducible).

## Re-validation

The patches are re-validated by **`BUILD.md` Part 1** (`git am`-clean apply + build
against the pinned `linux-6.18.35` tag) and, with the modules loaded, the in-tree
`vmm-core` box gates (`live_determinism`, `box_corpus`) on the determinism box. This
directory is data + recipe only — it has no cargo crate, so `cargo build`/`nextest`
do not touch it.

## For the integrator / next task (`PatchedKvmBackend`)

- The ABI (`KVM_EXIT_DETERMINISM` = 41, cap 245, the `determinism` payload) is a
  spike proposal, not upstream; the real backend can rename/renumber freely. The
  load-bearing result is that the **mechanism works** and is cheap.
- Exit cost (~3.4 µs RDTSC) is the input to R-Backend's deferred in-kernel
  V-time fast path decision: fine for occasional reads, worth optimizing only for
  hot RDTSC loops. Re-measure on a release build / the real kernel before deciding.
- Non-goals untouched: AMD/SVM, multi-vCPU, nested control propagation, upstreaming.
