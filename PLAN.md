# Build Plan: A Deterministic Hypervisor (Antithesis-style)

This document is the citation anchor for the CPU/MSR contract (`docs/CPU-MSR-CONTRACT.md:441,1481`
cite it) and is split in two by task 62: **Part A** is still-true, still-citable determinism
axioms; **Part B** is the original plan, now historical — superseded by `docs/ROADMAP.md` for
current state and by `docs/DISSONANCE.md` for the control-transport/fault-model design. Read
`docs/ROADMAP.md` first for "where are we now"; come here only for the axioms Part A holds, or
for archaeology in Part B.

---

## Part A — Frozen determinism axioms (still true, still citable)

Goal: a hypervisor that runs an unmodified(-ish) Linux guest fully deterministically — same seed
⇒ bit-identical execution — with fast CoW snapshot/branch, as the substrate for seed-driven
fault-injection testing. See `RESEARCH.md` for the evidence base.

### Platform

**Linux + KVM on bare-metal x86-64, written in Rust on rust-vmm crates.** VMX exit controls for
everything that must be trapped (RDTSC/RDRAND/RDSEED/CPUID), perf_event PMU access for
instruction counting, dirty-page logging for snapshots, rust-vmm ecosystem (kvm-ioctls,
vm-memory, linux-loader). Needs a bare-metal box (VMX doesn't nest well for PMU work).

### V-time-from-retired-branches

V-time = f(retired-conditional-branch count), read via perf_event on a KVM vCPU
(guest-only counting). All guest clocks (TSC, PIT, HPET, APIC timer) are derived from V-time,
never host wall-clock. Precise interrupt delivery: program the PMC to overflow slightly early,
single-step to the exact target count, inject. This is the hard core of the determinism design
and the one piece every later timer task (47/52/53/54/55) builds on.

### The trap table — sources of nondeterminism and their treatment

Per ReVirt's reduction; current disposition detail lives in `docs/CPU-MSR-CONTRACT.md` §3–4, this
table is the original, still-accurate top-level shape:

| Source | Treatment |
|---|---|
| RDTSC/RDTSCP | VMX TSC-exiting → return f(V-time) |
| RDRAND/RDSEED | exit → seeded PRNG stream |
| CPUID | filtered, fixed model; hide features that can't be determinized |
| Interrupt timing | only host-injected, at exact V-time via PMC-overflow + single-step landing |
| Time-of-day / timers | all clocks derived from V-time; PIT/HPET/APIC-timer emulated against V-time |
| /dev/(u)random | guest fed via hypercall from seed |
| Disk | read-only root image, or a deterministic block service (see `docs/DETERMINISM-CORPUS.md`) |
| External network | absent; intra-guest networking is enforced in-guest (see `docs/DISSONANCE.md`'s net-fault boundary — the *host-decides/guest-enforces* per-flow model, not the host-routed bridge this document originally sketched) |
| Internal network | containers on the guest's own network stack (loopback/bridge/veth) over deterministic CPU+RAM |
| SMP races | **v1 contract: an SMP-built kernel, exactly one *online* vCPU** (task 56 shipped `CONFIG_SMP=y`+`maxcpus=1`; the original "one vCPU, period" framing below is the *v1 scope*, not a literal `!SMP` build — real multi-vCPU is deferred, not foreclosed; ruling recorded in `docs/ROADMAP.md` and `docs/DISSONANCE.md`) |
| DMA timing | no DMA devices exist |
| Guest-kernel internals | minimal config + targeted patches (lazy TLB etc.) |

### Determinism gate discipline

Every phase/task has a determinism gate: same seed twice ⇒ identical state hashes. Never merge
work that fails its gate. This discipline is unchanged from Phase 0 through the current Wave-4
frontier tasks.

---

## Part B — Original plan (historical; superseded — see `docs/ROADMAP.md`)

The rest of this document is the **original** architecture sketch and phase plan, kept for
archaeology. It predates the dissonance design (`docs/DISSONANCE.md`), the R1 device-model
ruling (`docs/R1-DEVICE-MODEL.md`), and the Wave 3/4 pivots, and is superseded by them in several
concrete ways — noted inline rather than silently rewritten:

- **"VMCALL" as the hypercall mechanism (below).** The actual mechanism, chosen in task 20 for
  stock-KVM compatibility, is a **port-I/O doorbell**, not `VMCALL` (stock KVM services `VMCALL`
  in-kernel and never exits to userspace for it). See `docs/INTEGRATION.md` §1 and
  `docs/CPU-MSR-CONTRACT.md`'s VMCALL row for the current, accurate mechanism.
- **`ext-net` + a fault-injecting host bridge (below).** Retired. Networking is a per-flow
  **guest-plane** decision — the host *decides* a policy at the `NetFlow` seam, the guest
  *enforces* it on the intra-guest CNI (task 50; see `docs/DISSONANCE.md`'s guest fault model).
  There is no host-routed frame stream and no host-side switch (`dissonance/pv-net`, task 26,
  was retired by task 50).
- **Bare `restore` in the control-API sketch (below).** `docs/DISSONANCE.md`'s control transport
  has **no bare `restore`** — every restore is `replay` (verbatim, reproduce/gate) or `branch`
  (reseed, explore), so the reproduce-vs-diverge choice is explicit at every call site. The
  sketch below predates that split.
- **"3-node etcd" as the Phase-5 flagship distributed workload (below).** Superseded by the
  actual Wave-3 choice: single-node Postgres, escalating bare → Docker/runc → k3s (tasks
  36–38, 48, 49), because single-vCPU determinism rules out one-VM-per-node — "nodes" of a
  distributed system are containers/pods inside **one** guest instead.

### Decision 1 — Architecture (historical sketch)

```
┌─────────────────────────── host (Linux, x86-64, bare metal) ───────────────────────────┐
│  determinator-vmm (Rust, one process per VM, pinned to a core)                          │
│  ├─ vCPU loop: KVM_RUN; handles exits                                                   │
│  ├─ V-time: PMU (retired branches, guest-only) → virtual TSC/clock for ALL time         │
│  ├─ traps: RDTSC*, RDRAND/RDSEED, CPUID, RDPMC, port I/O → deterministic answers        │
│  ├─ injector: schedules interrupts at exact V-times (PMC overflow early + single-step)  │
│  ├─ devices: NONE real. hypercall channel (port-I/O doorbell, historically sketched as  │
│  │  VMCALL): console, entropy, block, ext-net (ext-net retired — see notes above)       │
│  ├─ snapshot engine: write-protect + dirty tracking; layered CoW snapshots; remap-restore│
│  └─ control API (unix socket): run-until, snapshot, branch(seed'), replay, hash-state    │
│     (historically sketched with a bare "restore" verb — retired, see notes above)        │
│                                                                                          │
│  explorer (separate process): drives N VMMs, seed scheduler, coverage map,               │
│                                branch-on-interesting, corpus of (snapshot, seed) pairs   │
└──────────────────────────────────────────────────────────────────────────────────────────┘
   guest: minimal Linux (custom config: tsc=reliable, no watchdogs, patched where
   nondeterministic) + containerized workload; internal networking is intra-guest and
   guest-enforced (ext-net / the host bridge below is retired — see notes above)
```

Sources of nondeterminism: see Part A's trap table above (this section's original copy was
consolidated there by task 62 to avoid two diverging tables).

### Phases (historical)

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
Hypercall channel: console, entropy, block (read-only image + CoW writes in guest RAM or via
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
fault schedule derived entirely from the seed. Milestone: a real distributed app runs under
injected faults, reproducibly. (Historical note: this sketch's original example workload was
"3-node etcd in containers" — superseded by the actual Wave-3 choice of single-node Postgres
escalating to k3s; see the notes at the top of Part B.)

**Phase 6 — Explorer**
Coverage via guest-side SDK hypercalls (and/or branch counts); interestingness scoring;
branch (restore + new seed) thousands of times; corpus management; minimization = replay the
seed. Milestone: finds a seeded bug in a toy distributed system and reproduces it 100/100.

### Delegable task specs (historical)

`tasks/00-CONVENTIONS.md` plus five self-contained, gate-first specs for the components that
can be implemented in parallel by delegated (cheaper-model) workers with no `/dev/kvm` and no
cross-task dependencies: `01-hypercall-proto`, `02-snapshot-store`, `03-unison`,
`04-guest-image`, `05-vtime`. This was Wave 1; it is long merged. See `docs/ROADMAP.md` for
current state.

### Known risks (historical)
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
