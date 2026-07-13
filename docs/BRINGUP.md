# Bring-up plan — Phase 0 → first deterministic guest

Frontier work (per PLAN.md / ROADMAP "frontier" lane): the `vmm-core` KVM skeleton, designed
with the user and driven on the determinism box. **Box-only** — Linux bare-metal Intel, VMX,
`/dev/kvm`, `perf_event` (`docs/BUILDING.md` capability matrix); macOS cannot run it. This doc
sequences the work and pins the contracts it implements; it does not negotiate with R1 / R-Backend
/ the CPU-MSR contract, it consumes them.

## Milestone — "stretch into determinism" (chosen scope)

Phase 0's "boots and prints" **plus** the start of Phase 1's determinism gate, in one push:

1. **M1 — boots & prints.** The skeleton loads the task-04 `hello` payload and reproduces
   `guest/golden/hello.txt` byte-for-byte over the emulated serial port, then exits clean.
2. **M2 — deterministic twice.** `hello` and `compute` run **twice** under the unison and
   produce **identical state hashes + identical serial output**. These two payloads are
   RDTSC/RDRAND-free, so they meet the gate on **stock `KvmBackend`** — no kernel patch needed yet.
3. **M3 — patched-KVM track started (parallel).** The out-of-tree RDTSC/RDRAND/RDSEED-exit patch
   (R-Backend) is building, so `PatchedKvmBackend` is ready when a TSC/RNG-reading payload (clocks,
   features, real Linux) needs it. This is the long pole (rebase treadmill) — start it early.

Not in this milestone (deliberately deferred — see "What later phases pick up").

## Crate structure (decision: split now)

Two new workspace crates, enforcing R-Backend's "nothing above the trait branches on the impl"
at the **crate** boundary:

- **`consonance/vmm-backend`** — the trap apparatus. The `Backend` trait + `Exit`/`Event`/`VcpuState`
  types (R-Backend's shape is the starting contract) and `KvmBackend` (stock KVM via
  `kvm-ioctls`/`kvm-bindings`). The trait and value types are **portable** (compile on macOS);
  the `kvm-*` deps and `KvmBackend` live under `[target.'cfg(target_os = "linux")'.dependencies]`
  + `#[cfg(target_os = "linux")]`, so the crate still builds on a Mac (trait only, no impl).
- **`consonance/vmm-core`** — the deterministic VMM above the trait: the Multiboot loader, the
  entry-state setup, the memory map, the CPUID/MSR-filter policy (data from `docs/CPU-MSR-CONTRACT.md`),
  the bring-up device shims (8250 UART, isa-debug-exit), and the **event loop**. The event loop drives
  the vCPU **only through the `Backend` trait** — it calls `backend.run()`/`run_until()` and matches on
  the returned `Exit`; it **never issues `KVM_RUN` itself**. `KVM_RUN` is a KVM-specific ioctl that
  lives **inside `KvmBackend::run()`** (in `vmm-backend`, *below* the trait) — that is precisely what
  makes "nothing above the trait branches on the impl" literally true: vmm-core is `KVM_RUN`-unaware and
  compiles against `Backend` alone. The **one** place a concrete backend is named is the binary's
  composition root (`fn main` selects `KvmBackend` vs `PatchedKvmBackend` and injects it) — that is
  dependency injection at the top, not a branch inside the trait-consuming logic. Portability follows:
  most of vmm-core is **pure-logic unit-testable on a Mac** (loader, UART model, contract enforcement)
  against a mock `Backend`; only *instantiating* `KvmBackend` and running it live is box-only — keep
  that in the binary / a `#[cfg(target_os = "linux")]` integration test, not the library.

**Dependency note:** `kvm-ioctls`, `kvm-bindings`, `vm-memory` are *not* on the delegated-crate
whitelist (`tasks/00-CONVENTIONS.md`) — that whitelist governs Mac-portable worker crates. These
are frontier, Linux-only deps. **There is no `deny.toml` crate allowlist to add them to** —
`deny.toml` gates licenses/advisories/sources only, and (by its own header) deliberately leaves
the rule-5 *crate* whitelist as a **review-time** gate. So admitting these deps is a **reviewed
rule-5 whitelist exception** recorded in the task spec / PR description (per convention rule 5's
ask-by-comment), not a `deny.toml` edit. The two crates are
**not** delegable to Mac workers (no `/dev/kvm`); the foreman can review their spec PRs but the code
is driven on the box. Prior art to crib from: `preestablished/determinism-hypervisor` (a working
stock-KVM deterministic VMM) — see the `prior-art-det-hypervisors` memory.

## The entry contract (task 04 — host side must replicate QEMU `-kernel`)

Verified from `guest/payloads/common/src/boot.s`, `uart.rs`, `lib.rs`, `linker.ld`:

| What | Value |
|---|---|
| Load image | the Multiboot ELF as **flat binary**, honoring the **file offset**: the loadable segment is **not** at ELF file offset 0 (ELF/program headers precede it), so copy from the PT_LOAD's `p_offset` — for the current payloads **`p_offset = 0x1000`** (the Multiboot header sits at file offset `0x1000`). The Multiboot address-override formula is `file_off = mb_header_file_offset − (header_addr − load_addr)`; here `header_addr == load_addr == 0x100000`, so the `(header_addr − load_addr)` term is `0` and `file_off = 0x1000` — **do not** drop the `mb_header_file_offset` term and use `header_addr − load_addr` alone (that gives `0`, the bug). Copy `load_end_addr − load_addr` bytes into GPA `load_addr` = `0x100000` (1 MiB), then zero BSS up to `bss_end_addr`. **Do not** copy from the start of the file or treat `load_addr` as a file offset — `_start`/`entry_addr` would then point at the wrong bytes and M1 won't boot. (Header uses address-override, flag bit 16.) |
| Entry point | `entry_addr` = `_start` (at the 1 MiB load region); set `RIP`/`EIP` there |
| CPU mode | **32-bit protected mode**, paging **off** (CR0.PG=0), A20 on |
| Segments | flat 32-bit CS (base 0, limit 4 GB, code) + flat DS/ES/SS (base 0, limit 4 GB, data) |
| GPRs | `EAX = 0x2BADB002` (Multiboot **bootloader** magic the loader passes at entry — **not** `0x1BADB002`, which is the *header* magic embedded in the image); `EBX` → a minimal Multiboot info struct in guest RAM (the shim doesn't read it, but set a valid pointer); `EFLAGS.IF = 0` |
| Console | polled **8250 UART, port `0x3F8`** (115200 8N1). Guest spins on LSR (`0x3FD`) THR-empty, then writes bytes to THR (`0x3F8`) |
| Halt/exit | write `u8` to port **`0xF4`** (isa-debug-exit): `0` = PASS, `1` = FAIL; falls back to a `hlt` loop if absent |
| Oracle | `guest/golden/<name>.txt` — byte-exact expected serial output (`PAYLOAD <name> START` … `PAYLOAD <name> PASS`) |

The shim itself enables PAE/long-mode and loads a 64-bit GDT after entry, so the host only has to
nail the **Multiboot 32-bit-PM handoff** — nothing more.

## Bring-up sequence

1. **Scaffold** the two crates + workspace wiring (the `kvm-*`/`vm-memory` deps enter as a
   **reviewed rule-5 whitelist exception** recorded in the task spec — not a `deny.toml` edit, which
   has no crate allowlist; confirm the root `cargo build` stays green on macOS via the `cfg(linux)`
   gating above).
2. **`KvmBackend` MVP** (`vmm-backend`, box): open `/dev/kvm`, `KVM_CREATE_VM`, **`KVM_IRQCHIP_NONE`**
   (R1 — no in-kernel irqchip/LAPIC/PIT), one vCPU, a single memslot. `run()` issues `KVM_RUN` and
   maps the raw `kvm_run` exit into the `Exit` enum. `save()`/`restore()` over
   `KVM_GET/SET_{REGS,SREGS,...}`. **Per-exit-reason counters** from day one (R-Backend normative).
   **`Rdtsc`/`Rdrand` cannot be intercepted on stock KVM at all** — they execute in-guest and return
   host-derived values with **no `KVM_EXIT`**, so the backend never sees them and there is nothing to
   "fail closed" on at runtime. The honest posture is **non-determinism-claiming**: `KvmBackend`
   declares (in its capabilities / the unison report) that it does **not** provide deterministic
   RDTSC/RDTSCP/RDRAND/RDSEED, so determinism on it holds **only for the audited RDTSC/RNG-free
   payload subset** (M2's `hello`/`compute`). Any payload that executes those instructions requires
   `PatchedKvmBackend` (M3); `save()` must never launder a host TSC into guest state as if deterministic.
3. **Loader + entry state** (`vmm-core`): parse/flat-load the Multiboot payload to GPA `0x100000`,
   set the 32-bit-PM register/segment state above. **CPUID filter** to the frozen contract model;
   **`KVM_X86_SET_MSR_FILTER`** default-deny per `docs/CPU-MSR-CONTRACT.md` — but **first enable
   `KVM_CAP_X86_USER_SPACE_MSR`** with the full mask the contract requires —
   `KVM_MSR_EXIT_REASON_FILTER | KVM_MSR_EXIT_REASON_UNKNOWN | KVM_MSR_EXIT_REASON_INVAL` (contract
   §1) — else a filter-denied MSR (or one KVM deems unknown/invalid, e.g. a bad write to an
   allow-stateful MSR) is an **in-kernel `#GP` with no `KVM_EXIT`** (silent), not the contract's
   required loud `KVM_EXIT_X86_RDMSR/WRMSR`.
4. **Bring-up device shims** (`vmm-core`): a minimal 8250 on `0x3F8` (accept init writes; LSR reads
   return THR-empty; THR writes append to a serial capture buffer) and isa-debug-exit on `0xF4`
   (terminate the run with the code). Treat `HLT` as terminal too.
5. **M1**: boot `hello`, assert the serial capture equals `guest/golden/hello.txt` **and** the
   terminal reason is a clean isa-debug-exit with code `PASS` (0) — not the fallback `HLT` and not a
   FAIL code (a payload can print `PASS` then exit non-clean; task 04's QEMU gate checks exit status too). **Boots & prints.**
6. **M2**: drive `hello` + `compute` through the **unison** (`Subject`/`SubjectFactory` adapter,
   INTEGRATION.md §5): `state_hash` = canonical hash of **all observable state** — materialized guest
   memory + `VcpuState` (`save()`) **+ the serial capture buffer + isa-debug-exit/device state**. The
   unison contract folds output logs into the hash, so checkpoint/bisect `compare_runs` stays
   correct; a separate serial assert alone would miss an output-only divergence later scrubbed from
   registers/memory. Run each twice, assert identical hash. **Deterministic twice.**
7. **M3 (parallel)**: build the KVM patch (RDTSC/RDTSCP/RDRAND/RDSEED-exiting controls + `KVM_EXIT_*`
   + userspace plumbing, ~low-hundreds of lines, `KVM_X86_SET_MSR_FILTER`-shaped) → `PatchedKvmBackend`
   surfaces those exits; route them to V-time / seeded-PRNG answers. Unlocks `clocks`/`features`/Linux.

## Determinism gate (the forcing function)

Per PLAN.md, every phase gate is "same seed twice ⇒ identical state hash," run by the unison.
For M2 the hash covers **all observable state** — guest memory + `VcpuState` **+ the serial capture
buffer + isa-debug-exit/device state** (per `unison::Subject::state_hash`: "registers, memory,
output log"); an output-only or wrong-exit-code divergence with identical memory/VCPU must still
break the hash, or `compare_runs`/bisect is incomplete. Once `tasks/09-vm-state` lands, the canonical
`vm_state` encoding replaces the ad-hoc register hash (and folds device state into the blob). The gate
is also what keeps stock KVM honest **whenever the nondeterministic value reaches hashed state**:
if a payload reads RDTSC/RDRAND on `KvmBackend` and that value flows into hashed memory or
`VcpuState`, the two runs diverge and the gate fails loudly. The gate is **not** a complete guard,
though — a payload that reads RDTSC but never lets it reach observable/hashed state (e.g. a `clocks`
payload that branches on the TSC but only prints fixed "OK" lines) can pass the hash+serial gate
while still being nondeterministic. So the gate is the *forcing function*, not a proof: the actual
guarantee for M2 is that its payloads are **RDTSC/RNG-free by audit**, and any backend running a
TSC/RNG-touching payload must be `PatchedKvmBackend`. Don't rely on the gate alone to catch a
stock-KVM determinism hole.

## What later phases pick up (not this milestone)

- **`tasks/13-lapic` + a PIT stub** → the `interrupts` payload and Phase 2 (V-time-driven timer +
  precise injection). The skeleton runs `KVM_IRQCHIP_NONE` with no interrupt delivery yet.
- **`tasks/09-vm-state`** → the canonical snapshot blob; folds into the M2 hash, then Phase 4
  snapshot/restore (with `snapshot-store` + task 08's restore mechanism).
- **perf_event `CpuBackend`** (consumes task 07's `skid_margin=128`) → Phase 2 V-time counting.
- **Multiboot/bzImage Linux loader + guest hypercall driver** → Phase 3 full Linux boot.
- **Control API (R2), fault model (R3)** → Phases 5/6, still unmade rulings.

## Environment header (for the spun-out task specs)

> *Requires: Linux bare-metal Intel x86-64 with VMX, `/dev/kvm`, `perf_event` access; does not run
> on macOS or under nested virtualization. The pure-logic portions (loader, UART model, contract
> policy) are Mac-unit-testable; live `KVM_RUN` is box-only.*

## Next artifacts

Spin out from this plan, reviewed as a docs PR like 09/13, executed on the box:

- **`tasks/14-backend.md`** — the `vmm-backend` crate: `Backend` trait + `KvmBackend` + the `Exit`
  surface + per-exit counters (R-Backend's named follow-up).
- **`tasks/15-vmm-core-skeleton.md`** — the `vmm-core` loader + entry-state + UART/exit shims +
  `KVM_RUN`/event loop + the M1/M2 gates.
