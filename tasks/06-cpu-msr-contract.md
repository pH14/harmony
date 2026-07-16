# Task 06 — `docs/CPU-MSR-CONTRACT.md`: guest-visible CPU/MSR determinism contract

Read `tasks/00-CONVENTIONS.md` first. Touch only `docs/CPU-MSR-CONTRACT.md` and (if you
take the machine-readable option in gate 3) `docs/cpu-msr-contract.toml`. This is a
research-and-writing task: no crate, no cargo gates.

## Environment

Runs on: macOS or Linux. Requires: the repo (docs/PLAN.md, docs/RESEARCH.md, docs/INTEGRATION.md,
the merged crates) and web access for primary sources (Intel SDM, Linux KVM API docs, rr).
**Depends on task 04 being merged**: the kernel pin (`guest/linux/versions.lock`) and the
kernel config (`guest/linux/config-fragment`) are inputs to the reference set and the
boot-critical baseline — do not start before they are on main.
Does not require: `/dev/kvm`, the determinism box, QEMU, root.

## Context

Trapping RDTSC is necessary but nowhere near sufficient: Linux/KVM exposes time and other
nondeterminism through many side doors — paravirtual clocks, TSC-adjacent MSRs,
power/frequency counters, timer devices, the PMU, CPUID itself. `docs/INTEGRATION.md` §7
mandates that this contract be authored **before any vmm-core code**: an exhaustive,
default-deny enumeration of what the guest may see, so that every leak vector is closed by
decision rather than by accident, and so the chosen surface can be frozen, versioned, and
hashed into the determinism gate. This document is that contract. It is a design artifact
with the same authority as INTEGRATION.md: vmm-core implements it, it does not negotiate
with it.

## Deliverable (the contract's required shape — normative)

`docs/CPU-MSR-CONTRACT.md` with these sections:

1. **Scope & default-deny statement.** The guest CPU surface is allow-listed. Denied MSRs
   are trapped via `KVM_X86_SET_MSR_FILTER` **plus** `KVM_CAP_X86_USER_SPACE_MSR` with
   `KVM_MSR_EXIT_REASON_FILTER`, so a denied access exits to userspace
   (`KVM_EXIT_X86_RDMSR`/`KVM_EXIT_X86_WRMSR`) where it is **logged with MSR index and
   RIP before #GP is injected** — the filter alone produces an in-kernel #GP with no
   logging, which does not satisfy "loud". Never a silent passthrough or a silent zero.
   State the policy for reads vs writes separately.
2. **Frozen CPUID model.** A leaf-by-leaf table (leaf, subleaf, register, value or masking
   rule, rationale) for one named baseline microarchitecture. Must cover at minimum:
   hidden KVM/hypervisor PV leaves (`0x4000_00xx` — hidden entirely, per §7's kvmclock
   vector), the hypervisor bit (CPUID.1:ECX[31]) stance, invariant-TSC bit
   (CPUID.8000_0007:EDX[8]) stance, TSC/crystal leaves (0x15/0x16), RDRAND
   (CPUID.1:ECX[30]) and RDSEED (CPUID.7,0:EBX[18]) exposure policy (exposed-but-trapped
   vs hidden — decide and justify against docs/PLAN.md's trap table), x2APIC (CPUID.1:ECX[21]),
   TSC_DEADLINE (CPUID.1:ECX[24]), PMU-related leaves (0xA — what a hidden vPMU reports),
   RDTSCP (CPUID.8000_0001:EDX[27], paired with the IA32_TSC_AUX disposition), RDPID
   (CPUID.7,0:ECX[22]), MONITOR/MWAIT (CPUID.1:ECX[3] and leaf 5), WAITPKG —
   UMWAIT/TPAUSE are guest-visible timing instructions — (CPUID.7,0:ECX[5]), Intel PT
   (CPUID.7,0:EBX[25] and leaf 0x14), and the thermal/power CPUID leaf (6). Default for
   each of these: **hide/deny unless explicitly justified**. In addition to the
   leak-vector leaves, the model must include a **boot-critical architectural baseline**:
   every CPUID leaf/bit the task-04 pinned kernel config requires to boot — long mode,
   NX, SYSCALL, APIC, the paging feature bits, cache/topology leaves, and XSAVE/OSXSAVE
   with leaf 0xD and an XCR0/XSETBV policy consistent with INTEGRATION.md §4's FPU/XSAVE
   snapshot requirement. **A contract whose CPUID model could not boot the task-04 guest
   fails review even with every leak vector closed.** Finally: the rule for every leaf
   not explicitly listed (zeroed or fixed — pick one, state it).
3. **MSR disposition table.** Every MSR in the reference set (below) gets a disposition
   **per access direction** — one for reads, one for writes — drawn from this vocabulary:
   `allow-fixed(value)` (a **read** disposition only — its write column must be `deny-gp`
   or `deny-ignore-write`), `allow-stateful` (architecturally
   guest-writable state, readable/writable normally and **captured in `vm_state` per
   INTEGRATION.md §4** — e.g. IA32_EFER, STAR/LSTAR/CSTAR/FMASK, SYSENTER_CS/ESP/EIP,
   FS_BASE/GS_BASE/KERNEL_GS_BASE, IA32_PAT), `emulate-vtime` (value derived from V-time;
   name the formula or vtime API), `emulate-timerqueue` (writes schedule deadlines on the
   TimerQueue — IA32_TSC_DEADLINE), `emulate-apic` (the x2APIC range 0x800–0x8FF gets a
   per-register sub-table with read/write semantics consistent with the split-irqchip /
   userspace-timer plan), `deny-gp` (access injects #GP), or `deny-ignore-write` (write
   dropped, loudly logged). Each row carries a one-line determinism rationale naming the
   §7 leak vector it closes (or "architectural" for the stateful allows).

   **The reference set is defined exactly** as the union of:
   (a) the static MSR arrays behind `KVM_GET_MSR_INDEX_LIST` **and**
       `KVM_GET_MSR_FEATURE_INDEX_LIST` as listed in `arch/x86/kvm/x86.c` at Linux tag
       **v6.18.35** from kernel.org — `msrs_to_save_*`, `emulated_msrs_all`, and the
       msr-based-features arrays at that tag. (v6.18.35 is the version task 04 pins;
       cross-check `guest/linux/versions.lock` once task 04 has merged — if the two ever
       disagree, versions.lock wins and you flag a `[question]`.);
   (b) every MSR named in INTEGRATION.md §7;
   (c) the named classes below, each expanded by a **stated match rule** against
       `arch/x86/include/asm/msr-index.h` at the same tag (e.g. "every name matching
       `MSR_IA32_THERM_*` or `MSR_IA32_PACKAGE_THERM_*`"), so completeness is mechanically
       checkable — no prose classes like "neighbors".
   Classes to cover at minimum: kvmclock (`MSR_KVM_*`); TSC plumbing (IA32_TSC,
   IA32_TSC_ADJUST, IA32_TSC_DEADLINE, IA32_TSC_AUX); power/frequency/thermal/turbo
   (IA32_APERF, IA32_MPERF, IA32_PERF_CTL/IA32_PERF_STATUS, thermal and package-thermal
   ranges, turbo-ratio-limit range, IA32_PLATFORM_INFO, IA32_PM_ENABLE and the HWP range);
   PMU (IA32_PMCx, IA32_PERFEVTSELx, IA32_FIXED_CTR*, IA32_PERF_GLOBAL_*, IA32_DS_AREA,
   IA32_PEBS_ENABLE — host owns the PMU; RDPMC traps); debug/branch tracing
   (IA32_DEBUGCTL, the LBR stacks — `MSR_LASTBRANCH_*`/`IA32_LBR_*` — and BTS/DS-adjacent
   MSRs); Intel PT (`IA32_RTIT_*`);
   speculation/capability (IA32_ARCH_CAPABILITIES, IA32_CORE_CAPABILITIES, IA32_SPEC_CTRL,
   IA32_PRED_CMD, IA32_FLUSH_CMD); microcode (IA32_BIOS_SIGN_ID — fixed); x2APIC
   (0x800–0x8FF); and the architectural stateful allows named above. **No TBD entries**
   (see gate 2 for what counts as decided). MSRs outside the reference set need no row:
   the default-deny filter denies-and-logs them by construction — the table enumerates
   the *decided* surface; the filter enforces everything else.
4. **Instruction & VMX-control dispositions.** CPUID hiding does not stop a kernel-mode
   guest from *executing* an instruction. For every timing/entropy/perf instruction the
   contract touches — RDTSC, RDTSCP, RDPID, RDRAND, RDSEED, RDPMC, MONITOR/MWAIT,
   UMWAIT/TPAUSE, XGETBV/XSETBV, HLT — one row stating the **enforcement mechanism**: the
   VMX execution control that exits (RDTSC exiting, RDRAND/RDSEED exiting, RDPMC exiting,
   MWAIT/MONITOR exiting, HLT exiting), or #UD-by-hiding **plus** the interception that
   backs it, or permitted-with-emulation — and what the VMM returns. **CPUID itself gets
   a row**: it VM-exits unconditionally, and the row states that every leaf is serviced
   from the frozen model in section 2 — never KVM's defaults, never host passthrough.
   Must be consistent with docs/PLAN.md's trap table (RDTSC → f(V-time), RDRAND/RDSEED →
   seeded stream, RDPMC → trap, HLT → idle-skip per INTEGRATION.md §3).
5. **Timer/time-device surface.** Per §7's timer vector, the contract must dispose of the
   whole guest-visible time-source surface, not just timer MSRs: the PIT (ports
   0x40–0x43), HPET (MMIO — default hidden), the LAPIC timer (the x2APIC MSR rows
   **plus** the xAPIC MMIO page policy), the **RTC/CMOS** (ports 0x70/0x71 — wall-clock
   reads come from V-time-derived emulation or the device is absent, with the kernel
   config arranged accordingly), and the **ACPI PM timer** (its port block, and whether
   the ACPI tables advertise it at all — default: not advertised). Each is routed to
   TimerQueue/V-time-backed userspace emulation or hidden — never a host-clock-backed
   device.
6. **Versioning & hashing.** The contract is a config artifact: define its canonical
   serialized form, what exactly is hashed into the determinism gate, and when the version
   bumps (any value change = new version).
7. **Citations.** Every non-obvious disposition cites a primary source (Intel SDM
   volume/chapter, Linux `Documentation/virt/kvm/api.rst`, rr's techniques, docs/RESEARCH.md
   entries). Match docs/RESEARCH.md's citation discipline.

## Acceptance gates

1. **Leak-vector coverage**: every vector enumerated in `docs/INTEGRATION.md` §7 maps to
   at least one named section/table row, checkable by walking §7's list top to bottom.
2. **No-TBD rule**: every reference-set entry has a read and a write disposition. A
   `deny-gp` + `[question]` row **counts as decided** (safe-by-default; the question
   invites a later, deliberate loosening) and is acceptable at merge. Only rows lacking a
   disposition — or prose hedges like "TBD"/"probably" — fail this gate.
3. **Machine-readable surface**: the CPUID and MSR tables are either strict Markdown
   tables with a documented column grammar, or an accompanying `cpu-msr-contract.toml` —
   something vmm-core can later parse and hash without re-reading prose.
4. **Consistency**: nothing contradicts INTEGRATION.md §1–§7, docs/PLAN.md's trap table, or the
   merged task crates (e.g. timer MSRs must route to TimerQueue-style userspace emulation,
   never an in-kernel LAPIC timer; entropy instructions must route to the seeded stream).
   Contradictions you cannot resolve are `[question]`s, not silent choices.
5. **CPUID↔MSR↔instruction consistency**: every exposed CPUID feature bit whose feature
   implies MSRs, instructions, or control state has matching dispositions in the other
   tables — or the bit must be hidden. No half-exposed features.
6. **Adversarial review**: the PR review's cross-model pass will be explicitly prompted to
   find guest-reachable nondeterminism the contract misses; expect findings to be folded
   in or rebutted line by line. Write the document to survive that.

## Non-goals

Implementing anything (no KVM code, no filters); choosing the perf_event counter config
(task 07's output); AMD; multi-vCPU; ARM; deciding §6's open questions beyond what the
contract's own entries force.
