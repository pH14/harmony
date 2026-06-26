# Build Plan: A Deterministic Hypervisor (Antithesis-style)

Goal: a hypervisor that runs an unmodified(-ish) Linux guest fully deterministically —
same seed ⇒ bit-identical execution — with fast CoW snapshot/branch, as the substrate for
seed-driven fault-injection testing. See RESEARCH.md for the evidence base.

## Decision 0 — Platform

**Recommended: Linux + KVM on bare-metal x86-64, written in Rust on rust-vmm crates.**

| Option | Verdict |
|---|---|
| **Linux/KVM, x86-64** | ✅ Recommended. VMX exit controls for everything we must trap (RDTSC/RDRAND/RDSEED/CPUID), perf_event PMU access for instruction counting, dirty-page logging + userfaultfd for snapshots, rust-vmm ecosystem (kvm-ioctls, vm-memory, linux-loader). Needs a bare-metal box (VMX doesn't nest well for PMU work) — a dedicated server or cloud `.metal` instance. |
| macOS Hypervisor.framework (Apple Silicon) | ❌ for v1. ARM time virtualization needs ECV (trap CNTVCT reads) and tight PMU control; HVF exposes neither well. Revisit later as a port. |
| FreeBSD bhyve (what Antithesis did) | Viable but you'd be modifying a kernel module from day one; KVM's userspace API gets further before kernel work is needed. |
| Full software emulation (QEMU-TCG-style) | Determinism nearly free, but 10–50x slow and a different project. Useful as a cross-check oracle, not the build target. |

Practical note: development happens on the Linux box over SSH; macOS is just the terminal.

## Decision 1 — Architecture

```
┌─────────────────────────── host (Linux, x86-64, bare metal) ───────────────────────────┐
│  determinator-vmm (Rust, one process per VM, pinned to a core)                          │
│  ├─ vCPU loop: KVM_RUN; handles exits                                                   │
│  ├─ V-time: PMU (retired branches, guest-only) → virtual TSC/clock for ALL time         │
│  ├─ traps: RDTSC*, RDRAND/RDSEED, CPUID, RDPMC, port I/O → deterministic answers        │
│  ├─ injector: schedules interrupts at exact V-times (PMC overflow early + single-step)  │
│  ├─ devices: NONE real. hypercall channel (VMCALL): console, entropy, block, ext-net    │
│  ├─ snapshot engine: write-protect + dirty tracking; layered CoW snapshots; remap-restore│
│  └─ control API (unix socket): run-until, snapshot, restore, branch(seed'), hash-state   │
│                                                                                          │
│  explorer (separate process): drives N VMMs, seed scheduler, coverage map,               │
│                                branch-on-interesting, corpus of (snapshot, seed) pairs   │
└──────────────────────────────────────────────────────────────────────────────────────────┘
   guest: minimal Linux (custom config: !SMP, tsc=reliable, no watchdogs, patched where
   nondeterministic) + containerized workload + deterministic fault-injecting net bridge
```

Sources of nondeterminism and their treatment (the complete list, per ReVirt's reduction):

| Source | Treatment |
|---|---|
| RDTSC/RDTSCP | VMX TSC-exiting → return f(V-time) |
| RDRAND/RDSEED | exit → seeded PRNG stream |
| CPUID | filtered, fixed model; hide features we can't determinize |
| Interrupt timing | only host-injected, at exact V-time via PMC-overflow + single-step landing |
| Time-of-day / timers | all clocks derived from V-time; PIT/HPET/APIC-timer emulated against V-time |
| /dev/(u)random | guest fed via hypercall from seed |
| Disk | read-only root image + deterministic block-over-hypercall |
| External network | absent, or deterministic host model over hypercall |
| Internal network | containers + deterministic bridge inside the guest |
| SMP races | one vCPU, period |
| DMA timing | no DMA devices exist |
| Guest-kernel internals | minimal config + targeted patches (lazy TLB etc.) |

## Phases

Each phase has a determinism gate: same seed twice ⇒ identical state hashes. Build the
harness first; never merge a phase that fails its gate.

**Phase 0 — Skeleton (1–2 weeks of evenings)**
Rust VMM on kvm-ioctls: load a tiny kernel (or bare-metal test payload), one vCPU, serial
console via port-I/O exit. Milestone: boots and prints.

**Phase 0.5 — Feasibility spikes (first days on the rented box, before Phases 2/4 invest)**
Two throwaway experiments the architecture is betting on; if either fails on the target CPU,
design changes cascade, so prove them first:
1. *PMU precise-count spike*: guest-only retired-branch counting via perf_event on a KVM
   vCPU — verify host exits/hypercalls don't perturb the count, overflow reliably breaks out
   of KVM_RUN, measured skid is bounded, and PMU-read + KVM single-step lands at stable event
   counts across repeated runs. Output: a measured skid bound (→ `PlannerConfig::skid_margin`)
   and a go/no-go for the vtime design.
2. *KVM memory snapshot/restore spike*: restore a 1–4 GiB guest by remapping (memslot swap /
   mmap over snapshot-store layers) rather than copying — does KVM userspace allow it, and how
   fast? Measures memslot-churn cost and whether a kernel-side assist will be needed.
   Output: restore latency numbers and the chosen Phase 4 restore mechanism.

**Phase 1 — Determinism harness + CPU determinization**
State-hash tool (registers + all guest pages at chosen exits); divergence bisector (binary
search by instruction count). Enable TSC/RDRAND/RDSEED exiting, CPUID filter. Run a
single-process guest payload twice ⇒ identical hashes. Milestone: deterministic
computation-only guest.

**Phase 2 — V-time + precise injection (the hard one)**
perf_event counter (retired conditional branches, guest-only) per vCPU; V-time = f(count).
Emulate APIC timer against V-time. Precise delivery: program PMC to overflow ~100 branches
early, KVM_GUESTDBG single-step to the exact count, inject. Milestone: timer-interrupt-driven
guest runs deterministically; interrupts land at identical instruction counts across runs.

**Phase 3 — Hypercall I/O + full Linux boot**
VMCALL channel: console, entropy, block (read-only image + CoW writes in guest RAM or via
channel). Minimal kernel config; patch what diverges (the harness will find it). Milestone:
Linux boots to userspace deterministically, twice.

**Phase 4 — Snapshots & branching**
v1: pause, dirty-log copy, restore by re-write. v2: write-protect all memory
(KVM_MEM_LOG_DIRTY_PAGES / mprotect on the backing memfd), layered snapshots = changed pages
+ vCPU/device state; restore by remapping memory regions; share the post-boot base image
read-only across VMs. Milestone: snapshot/restore in ms; restored run replays identically;
N VMs share one boot image.

**Phase 5 — Workload + fault injection**
Container workload in guest; deterministic bridge with seed-driven delay/drop/partition;
fault schedule derived entirely from the seed. Milestone: a real distributed app (e.g.
3-node etcd in containers) runs under injected faults, reproducibly.

**Phase 6 — Explorer**
Coverage via guest-side SDK hypercalls (and/or branch counts); interestingness scoring;
branch (restore + new seed) thousands of times; corpus management; minimization = replay the
seed. Milestone: finds a seeded bug in a toy distributed system and reproduces it 100/100.

## Delegable task specs

`tasks/00-CONVENTIONS.md` plus five self-contained, gate-first specs for the components that
can be implemented in parallel by delegated (cheaper-model) workers with no `/dev/kvm` and no
cross-task dependencies: `01-hypercall-proto`, `02-snapshot-store`, `03-unison`,
`04-guest-image`, `05-vtime`. KVM bring-up, the real perf_event backend, and all Tier-2
integration stay on frontier models. The seams between the delegated components and
vmm-core — VMCALL ABI, run-loop ownership, idle-skip, the snapshot-contents checklist, and
the guest-visible CPU/MSR contract — are owned by `docs/INTEGRATION.md`.

**Deliberately not yet specced** (they need the bare-metal box, and most need design output
from what precedes them — see the capability matrix in `docs/BUILDING.md` for environment
requirements): the vmm-core KVM skeleton; the two Phase 0.5 spikes (PMU precise-count, KVM
remap-restore); the guest-visible CPU/MSR contract document; and the
`guest/linux/hypercall-driver` task (guest-Linux side of the channel: early console, entropy
provider, block frontend — waits until the VMCALL ABI is validated against a real vmm-core).
Task specs for these get written when the box is rented, with environment headers of the
form: *Requires: Linux bare-metal Intel x86-64 with VMX, `/dev/kvm`, perf_event access;
does not run on macOS or under nested virtualization.*

## Known risks
- **PMU counter quality**: retired-branch counters have skid and (on some µarchs) overcount;
  rr's source documents per-µarch quirks — choose CPU model accordingly (modern Intel is the
  best-trodden path; that's also why Antithesis was VMX/Intel-only).
- **KVM gaps**: bhyve let Antithesis patch the kernel freely. If KVM's userspace API can't do
  precise injection or fast EPT remapping well enough, the fallback is a small out-of-tree
  kvm module patch — defer until proven necessary.
- **Guest kernel whack-a-mole**: expect a long tail of nondeterministic kernel behaviors;
  the bisector harness is the tool that keeps this tractable.
- **Scope**: phases 0–3 are a complete, publishable "deterministic Linux VM" result on their
  own; 4–6 turn it into an Antithesis-style platform.
