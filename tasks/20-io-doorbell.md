# Task 20 — hypercall doorbell on stock KVM: `VMCALL` → port-IO/MMIO

Read `tasks/00-CONVENTIONS.md` first. Touch `consonance/hypercall-doorbell/` (the transport crate)
plus the two doc surfaces below. This reworks the **hypercall doorbell** so the host↔guest
hypercall channel works on **stock KVM with no kernel patch** — replacing the `VMCALL` doorbell
(which stock KVM services in-kernel and does **not** surface to userspace for a custom magic
number) with a port-`OUT`/`IN` (→ `KVM_EXIT_IO`) or MMIO (→ `KVM_EXIT_MMIO`) mechanism.

> **Why (integrator ruling, 2026-06-23):** stock KVM's `kvm_emulate_hypercall` returns `-ENOSYS`
> in-guest for our magic number (`0x3150_4348`) and resumes — only `KVM_HC_MAP_GPA_RANGE` exits via
> `KVM_EXIT_HYPERCALL`. A `VMCALL` doorbell therefore needs the patched backend. A port-`OUT` to a
> magic port, or an MMIO access to a magic GPA, **is** surfaced by stock KVM as `KVM_EXIT_IO` /
> `KVM_EXIT_MMIO` — so the hypercall channel then works with **zero** kernel patch. RDTSC/RNG
> interception still needs the patch (task 21) — that's separate; this is only the doorbell.

## Current state (task 10, merged)

`consonance/hypercall-doorbell/src/lib.rs` is built around `VMCALL`: `VMCALL_MAGIC = 0x3150_4348`
in `RAX`, request-page GPA in `RBX`, response-page GPA in `RCX`, host returns response-frame
length in `RAX`. The `VmExit` trait (`unsafe fn vmcall(magic, req_gpa, resp_gpa) -> u64`) is the
seam over the privileged instruction; `VmcallTransport::exchange(req, resp) -> usize` does the
page copy + the load-bearing bounds checks (`rax <= PAGE_SIZE`, `<= resp.len()`, **checked in u64
before the `as usize` cast** — preserve that property exactly). `RealVmcall` emits the `vmcall`
asm. The backend surfaces this as `Exit::Hypercall(HypercallRegs{rax,rbx,rcx,rdx})` awaiting
`complete_hypercall(rax)` (in `consonance/vmm-backend`).

## The rework — recommended: a port-IO doorbell

Pick **port-IO** unless you can show MMIO is materially cleaner (raise a `[question]` if so). A
single magic doorbell port; request/response live in two **fixed-GPA** pages agreed in the ABI
(so the doorbell carries no pointer — `OUT` cannot pass two 64-bit GPAs). Proposed ABI (finalize +
document it; the exact port/GPAs are yours to pick and pin in INTEGRATION.md):

- `DOORBELL_PORT` (a magic 16-bit port, e.g. `0x0CA1`), `REQ_GPA` / `RESP_GPA` (two fixed
  guest-physical pages the VMM maps and the contract reserves).
- Guest writes the request frame into the `REQ_GPA` page, then `OUT DOORBELL_PORT, AX` where the
  written value = request length → host gets `Exit::Io{port, size, write:Some(len)}`, reads
  `REQ_GPA`, services it, writes the response into `RESP_GPA`, resumes.
- Guest then `IN AX, DOORBELL_PORT` → host returns the response length (or 0 = rejected) via
  `complete_read(len)`; guest reads the response from `RESP_GPA`. (Or fold the length into the
  response-frame header to save the `IN` — your call, document it.)
- Keep the exchange **wait-free / single-in-flight** and the **u64 bounds-check-before-cast**
  invariant. The `exchange()` signature and `TransportError` set stay the same; only the doorbell
  primitive changes.

Replace the `VmExit`/`RealVmcall` seam with an `IoDoorbell`/`RealIoDoorbell` (or keep the trait
name, swap the body). The generic test seam (a mock doorbell for the loopback test) must remain so
the round-trip is unit-testable on macOS.

## Also update (a half-rework is the failure mode)

- `docs/INTEGRATION.md §1` (the transport ABI) — rewrite for the port-IO/MMIO doorbell: the port,
  the fixed page GPAs, the OUT/IN protocol, the response-length signaling. This is the wire
  contract other components build against — be exact.
- `docs/cpu-msr-contract.toml` + `docs/CPU-MSR-CONTRACT.md` — the **VMCALL row** (currently
  `mechanism = "vmx-exit(vmcall-unconditional)"`, mislabeled "stock-serviceable"). Either repoint
  it to the new doorbell mechanism or add the doorbell-port/MMIO-GPA rows; ensure the contract no
  longer claims a custom VMCALL is stock-serviceable (it isn't). Determinism note: the doorbell
  port/GPA constants must not reach a hashed input — confirm.
- `consonance/vmm-backend`: the dispatcher maps the doorbell to `Exit::Io`/`Exit::Mmio` (already
  surfaced by stock `KvmBackend`). `Exit::Hypercall` may become **vestigial** (kept for the
  patched backend or removed) — note which in IMPLEMENTATION.md; do not silently break the trait.

## Gates

```sh
cargo build  -p hypercall-doorbell --all-features
cargo nextest run -p hypercall-doorbell --all-features      # incl. the loopback round-trip over the mock doorbell
cargo clippy -p hypercall-doorbell --all-features --all-targets -- -D warnings
cargo fmt    -p hypercall-doorbell -- --check
```
- The loopback test must round-trip a request/response over the new doorbell primitive (mock),
  exercising the bounds checks (oversize request → `RequestTooLarge`; host-reject → `HostRejected`;
  out-of-range length → `BadResponseLength`).
- Property test the bounds/length handling if practical (≥256 cases).
- macOS + Linux build/test (rule 6). The doorbell asm (`OUT`/`IN`) is guest-side bare-metal; the
  host-side `Exit::Io` handling is exercisable via the mock without KVM.

## Deliverables

Reworked `hypercall-doorbell` (likely worth renaming the crate to `io-transport` or similar when
convenient — but **not** in this task; keep the package name to avoid churn, just update the prose).
Updated INTEGRATION.md §1 + the contract VMCALL row. `IMPLEMENTATION.md` documenting the chosen
mechanism (port-IO vs MMIO + why), the finalized ABI, and the `Exit::Hypercall` disposition.
