# Integration map

How the delegated components (tasks 01–05) plug into `vmm-core` (the KVM VMM, frontier work,
not yet specced). This document owns the seams that deliberately live in **no** single task
spec. It is the source of truth for ABI decisions; task specs defer to it.

## 1. Hypercall-doorbell transport ABI (v1 — port-I/O doorbell)

The hypercall channel's transport, deliberately out of scope for tasks 01 and 04. **Reworked
in task 20** from the v0 `VMCALL` doorbell to a **port-I/O doorbell** so the channel works on
**stock KVM with no kernel patch**.

> **Why not `VMCALL` (integrator ruling, 2026-06-23).** Stock KVM services `VMCALL` *in-kernel*:
> for our magic number (`0x3150_4348`) `kvm_emulate_hypercall` returns `-ENOSYS` to the guest
> and resumes — it never surfaces a `KVM_EXIT_HYPERCALL` to userspace for a custom number (only
> `KVM_HC_MAP_GPA_RANGE` exits). A `VMCALL` doorbell therefore needs the patched/direct-VMX
> backend (task 21). A port `OUT` to a magic port, by contrast, **is** surfaced by stock
> KVM as `KVM_EXIT_IO` — so the hypercall channel works with **zero** kernel patch. (RDTSC/RNG
> interception still needs the patched backend — that is separate; this is only the doorbell.)

**Fixed ABI constants** (an `OUT` cannot carry a pointer, so the doorbell carries none — the
two frame pages live at fixed GPAs the contract reserves and the VMM maps; pinned in
`consonance/vmcall-transport`):

- `DOORBELL_PORT = 0x0CA1` — the magic 16-bit I/O port the guest rings (`> 0xFF`, so addressed
  through `DX`). Chosen clear of the legacy ISA/PCI port map (PIT `0x40`–`0x43`, PIC, PS/2,
  PCI-config `0xCF8`/`0xCFC`).
- `REQ_GPA = 0x0000_E000`, `RESP_GPA = 0x0000_F000` — two fixed, distinct, page-aligned 4 KiB
  guest-RAM pages: the **request page** and the **response page**. The loader/vmm-core reserves
  them (mark `reserved` in the guest e820 / task-04 payload map; never place the kernel, initrd,
  or cmdline there) and identity-maps them (GPA == linear address); exact placement may be
  finalized at vmm-core bring-up — what this ABI pins is that they are two fixed, VMM-reserved,
  identity-mapped pages.

**One exchange** is a **single `OUT` VM exit** (synchronous, single in-flight, wait-free):

1. The guest writes one complete request frame (task 01 wire format) into the `REQ_GPA` page.
2. `OUT DOORBELL_PORT, EAX` with `EAX` = the request-frame length → the host gets
   `Exit::Io { port: 0x0CA1, size: 4, write: Some(len) }`, reads `len` bytes from `REQ_GPA`,
   runs `Dispatcher::dispatch(request, response)`, writes the response **frame** into `RESP_GPA`,
   and resumes the guest at the next instruction (no completion — an `OUT` needs none).
3. The guest reads the **response length from the response-frame header** in `RESP_GPA` — the
   frame is self-describing (`HEADER_LEN + payload_len`, the same wire format the `Client`
   decodes) — and copies that many bytes out. A response page that does **not** begin with the
   frame magic (`0x31504348`) is a rejection (e.g. the host wrote nothing for a bad doorbell);
   the guest treats it as `Transport::Error`.

**Atomicity / single-in-flight.** The exchange is **one** exit: the host fully services it and
writes the response before resuming, holding **no pending state across a guest resume** — exactly
like the old single-`VMCALL` doorbell. The response length is folded into the frame header rather
than returned by a second `IN` exit precisely to preserve this: a two-exit `OUT`/`IN` doorbell
would resume the guest *between* the exits while the host still owed a length, so an interrupt
injected in that window whose handler re-entered the doorbell could clobber the fixed pages and
the pending length. The single vCPU is blocked for the whole exit (trivially race-free); the two
pages' contents are not preserved across the call except as specified. The header-declared length
is bound-checked by the guest shim — `HEADER_LEN + payload_len ≤ PAGE_SIZE` **and** `≤` the
caller buffer, computed and checked in `u64` before any narrowing cast (`payload_len` is
host-controlled, so the sum can exceed `u32::MAX`) — so a hostile host cannot make the guest read
past a page or write past its buffer.

**Determinism note (confirmed):** `DOORBELL_PORT`/`REQ_GPA`/`RESP_GPA` are transport-ABI
constants that live **here**, not in the CPU/MSR contract. They are deliberately **not** rows
in `docs/cpu-msr-contract.toml` and therefore **never enter the §6 canonical form or
`contract_hash`** — they carry no per-host or hidden-µarch input, so hashing them would add
nothing and would couple the frozen contract to a transport detail. (The contract's VMCALL row
covers only the `VMCALL` *instruction*'s disposition under the determinism backend; see
`CPU-MSR-CONTRACT.md` §4.)

**Guest shim:** `consonance/vmcall-transport` implements the task-01 `Transport` over this
doorbell (the package/type keep their `vmcall` names to avoid churn; the mechanism is port-I/O).
`RealIoDoorbell` emits the single `OUT`; the `IoDoorbell` seam lets the whole round-trip be
unit-tested over a mock with no KVM. A `Client<VmcallTransport>` composes with the task-01
`Client` unchanged.

**Patched-backend variant (task 21):** on `PatchedKvmBackend`/`DirectVmxBackend` the same frame
semantics may instead ride a `VMCALL` doorbell surfaced as `Exit::Hypercall(HypercallRegs)` (RAX
= magic, RBX/RCX = the page GPAs, host sets RAX = response length). This is an optional
alternative to the port-I/O doorbell, not a replacement; the wire frames and `Dispatcher` are
identical either way. (The `VMCALL` variant, being its own single exit, is likewise atomic.)

Push-style input (host-initiated events) is **not** part of this ABI; it arrives as injected
interrupts at planned V-times, after which the guest pulls data via a normal hypercall.

### 1.1 Report channel (conformance value reporting)

The determinism/conformance corpus (`docs/DETERMINISM-CORPUS.md`) needs the C1 payloads to report
their **trap-dependent values** — the V-time TSC reads, the seeded RNG words, the frozen CPUID/MSR
values, the retired-instruction counts — to the host oracle (`det-corpus` O2). The serial lane
can't carry them (a raw TSC/IRQ count in the banner would perturb the byte stream and break the
Part-A shape gate), and the doorbell above **owns `0x0CA1`** — so the report stream gets its **own
dedicated port**, distinct from the doorbell.

- **`REPORT_PORT = 0x0CA2`** — adjacent to but separate from `DOORBELL_PORT = 0x0CA1`, so a
  reported value is never mistaken for a doorbell ring (and vice-versa). Chosen, like the doorbell,
  clear of the legacy ISA/PCI port map.
- **One report write is one VM exit.** `OUT REPORT_PORT, EAX` (a **32-bit** write) → the host gets
  `Exit::Io { port: 0x0CA2, size: 4, write: Some(v) }` and **appends `v`** to the VM's ordered
  `Vec<u32>` report stream, then resumes the guest. No completion (an `OUT` needs none); a non-dword
  access is unmodeled and fails closed. The guest's `report(u64)` is **two** writes — low dword then
  high — so the host reassembles a 64-bit value from that fixed (low, high) pair.
- **Determinism-clean.** Every reported value is already deterministic (a V-time TSC, a seeded-PRNG
  word, a frozen CPUID/MSR value, a retired count) and the stream is ordered by execution, so the
  stream is a **pure function of the run**. `vmm-core` digests it (with the serial banner) into the
  guest-observable `observable_digest` — the O2 conformance signal — **separately from**
  `state_hash` (the O1 full-state hash, which is byte-for-byte unchanged by this channel: a run
  that never touches the port leaves the stream empty).
- **Stock QEMU shape-testing.** QEMU has no device at `0x0CA2`, so it **discards** the writes (no
  `#GP`, nothing on serial) — the Part-A serial gate stays byte-identical; only the box, where
  `vmm-core` captures the port, sees the values. The guest shim is `guest/payloads/common::report`;
  the host side is `vmm-core`'s `Exit::Io` dispatch + `Vmm::observable_digest`.
- **Determinism note (confirmed):** like the doorbell constants, `REPORT_PORT` is a transport/
  observability ABI constant that carries **no per-host or hidden-µarch input**, so it is **not** a
  row in the §6 canonical form or `contract_hash` — it lives here and in `docs/cpu-msr-contract.toml`
  `[ports]` (documentary), not as a hashed contract row. (`contract_hash` is unchanged by it.)

## 2. Run-loop ownership

`vmm-core` owns the `KVM_RUN` loop. The vtime `InjectionPlanner` was specced as the driver
(`stop_at` calls the backend); at integration, expect to **invert** this: the event loop
handles all exit reasons (hypercall, MMIO, HLT, PMU overflow, single-step) and reuses the
planner's arithmetic/state machine to decide when to arm the counter, when to switch to
single-stepping, and when the injection point is reached. The planner's exactness property
tests are the durable asset; its driver-shaped API is allowed to bend. Non-timer exits
(hypercalls) occurring while armed are serviced inline and execution resumes toward the same
armed target — they don't disturb the plan because servicing them performs no guest work.

## 3. Idle-skip protocol

On a `HLT` exit with interrupts enabled (work frozen, V-time would stall):

1. If `TimerQueue::peek_next()` is `Some((deadline, _))`: `VClock::advance_idle(deadline −
   vns(now))`, pop due timers, inject — zero single-steps needed (`stop_at` with
   `target == now`).
2. If the queue is empty: nothing can ever wake the guest. That is a terminal state — report
   it upward (test ended / guest hung), never spin or invent time.

`HLT` with interrupts disabled is a guest shutdown idiom: treat as terminal.

## 4. Snapshot contents checklist

Rule: **anything that can influence future guest-visible behavior must be captured.** A
snapshot = snapshot-store layer (guest memory dirty pages) + the opaque `vm_state` blob,
which vmm-core serializes and must contain at least:

- vCPU: GPRs, segment/system registers, FPU/XSAVE area, relevant MSRs, LAPIC + PIC + PIT
  emulated state, pending/in-service interrupt state.
- V-time: `VClock::snapshot_vns(work)` result and `tsc_base`/ratio config; on restore, the
  hardware counter restarts at 0 and the value goes into `vns_base` (task 05 gate 6 proves
  continuity). **Snapshot-bearing configs must use integer ratios (`ratio_den == 1`)** —
  vmm-core rejects fractional ratios at config validation. Ruling (PR #5): `snapshot_vns`
  is whole nanoseconds, so a fractional ratio's sub-ns remainder `(work·num) mod den` cannot
  survive a snapshot; a restored clock would lag a never-snapshotted run by ≤ 1 ns per
  snapshot generation and a timer's injection target could shift one counted event. Replay
  determinism is unaffected either way (quantization is deterministic; restored-vs-restored
  runs are bit-identical) — the constraint exists so restored timelines also match
  unsnapshotted references exactly.
- `TimerQueue` contents (deadlines are absolute V-time, so they survive restore unchanged).
- `Dispatcher::save_state()` (task 01) — notably the entropy PRNG position. After a restore
  intended to *branch* (explore a different future), vmm-core reseeds/perturbs the entropy
  service explicitly; after a restore intended to *replay*, it restores the state verbatim.
  That choice is the explorer's, which is why it must be explicit state, not ambient.
- Planner/injection bookkeeping: there must be **no armed-but-unfired plan** at snapshot
  time — vmm-core only snapshots at quiescent points (after an exit is fully serviced,
  nothing armed). Enforce with an assertion rather than trying to serialize plan state.

## 5. Adapter map

| Seam | Delegated side (frozen) | vmm-core side (later) |
|---|---|---|
| Hypercalls | `hypercall-proto::{Client, Transport}` (guest), `Dispatcher`/`Service` (host) | port-I/O **doorbell** handler implementing §1 (`Exit::Io` on `DOORBELL_PORT`, stock-KVM `KVM_EXIT_IO`); guest shim `consonance/vmcall-transport`. Patched-backend `VMCALL` variant via `Exit::Hypercall` (task 21) |
| Time | `vtime::{VClock, TimerQueue, InjectionPlanner, CpuBackend}` | perf_event retired-branch counter (guest-only), PMI → exit; `KVM_GUESTDBG_SINGLESTEP`; §2 inversion |
| Memory/snapshots | `snapshot-store::{Store, builders, Mapping}` | KVM dirty-log harvest → `DeltaBuilder`; `materialize()` → memslot swap; `vm_state` blob per §4 |
| Determinism testing | `unison::{Machine, MachineFactory}` | adapter: `spawn(seed)` = restore base snapshot + seed services; `run_to` = planner stop; `state_hash` = canonical hash of materialized memory + `vm_state` |
| Guests | task 04 Multiboot payload contract & Linux image | Multiboot loader (replicating QEMU `-kernel` entry state) and bzImage loader; PIT/PIC device emulation backed by `TimerQueue` |

## 6. Open questions (decide during vmm-core bring-up)

- Exact perf_event config for guest-only retired-branch counting (`exclude_host`,
  `exclude_hv`) and the measured skid bound → `PlannerConfig::skid_margin`.
- Whether `KVM_RUN`-adjacent kernel work (fast memslot swap for restore, precise PMI
  delivery) eventually needs a small kernel patch — defer until measured.
- Whether the `interrupts` payload's PIT path is emulated pre- or post-V-time integration
  (a fixed-cadence fake is acceptable for first boot).
- If sub-ns-per-branch virtual rates are ever needed: amend vtime to carry the sub-ns
  remainder through snapshots (`VClockConfig` gains a `vns_rem < ratio_den` field,
  `snapshot_vns` returns the pair, `work_for_vns` gains the rem term). Backward-compatible —
  `rem = 0` reproduces integer-ratio behavior bit-exactly — plus one vm_state blob field;
  cheap while snapshots remain ephemeral (snapshot-store has no cross-restart persistence).

## 7. Guest-visible CPU/MSR contract (author before vmm-core code)

Trapping RDTSC is necessary but nowhere near sufficient — Linux/KVM exposes time and other
nondeterminism through many side doors. Before vmm-core work starts, write the contract doc
that enumerates, exhaustively: which CPUID leaves are exposed (and their frozen values),
which MSRs are readable/writable, which are emulated against V-time, and which raise #GP.
Default-deny: KVM's MSR filter (`KVM_X86_SET_MSR_FILTER`) set to trap everything not
explicitly allowed; unknown MSR access is a loud event, not a passthrough.

Known leak vectors the contract must cover:

- **KVM paravirtual clock**: hide the KVM PV CPUID leaves entirely (no kvmclock MSRs);
  also build the guest kernel without PV-clock support as defense in depth.
- **TSC plumbing**: `IA32_TSC`, `IA32_TSC_ADJUST`, `IA32_TSC_DEADLINE` — emulated against
  V-time or denied; KVM's TSC offset/scaling must never let host TSC reach the guest.
- **Power/frequency**: `APERF`/`MPERF`, `MPERF`-adjacent, thermal/turbo MSRs — deny.
- **Timer devices**: PIT/HPET/LAPIC-timer state must be fully V-time-driven. Consequence:
  **no KVM in-kernel timer devices unless proven V-time-driven** — in-kernel LAPIC timers
  run on host hrtimers, which is real time. **Ruling R1** (`docs/R1-DEVICE-MODEL.md`) settles
  this: KVM source confirms split-irqchip keeps the LAPIC timer in-kernel on host hrtimers and
  the userspace MSR filter cannot reach the APIC / `IA32_TSC_DEADLINE` MSRs while an in-kernel
  LAPIC is live. So vmm-core uses **no in-kernel irqchip** (`KVM_IRQCHIP_NONE`) and emulates
  the LAPIC in userspace as an **xAPIC** (MMIO `0xFEE00000`), timer driven by `TimerQueue` via
  the initial-count register — no in-kernel LAPIC, no x2APIC, no TSC-deadline timer.
- **PMU**: no vPMU exposed to the guest (`RDPMC` traps/faults); the host owns the PMU.
- **APIC register surface**: per Ruling R1 the guest sees an **xAPIC** (MMIO page), not
  x2APIC — `X86_FEATURE_X2APIC` is hidden in CPUID (KVM cannot forward x2APIC MSRs to
  userspace, and an advertised-but-unbacked x2APIC `#GP`s the guest). Enumerate which xAPIC
  MMIO registers the guest may touch and how each is virtualized deterministically against
  V-time.
- **CPUID stability**: one frozen, versioned CPUID model (a config artifact, hashed into
  the determinism gate) — never inherit the host's leaves.
