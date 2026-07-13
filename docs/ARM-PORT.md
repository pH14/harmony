# ARM/AArch64 port — feasibility notes

Status: **research note, not a commitment; partially superseded (2026-07-03), refined for
Apple silicon (2026-07-09).** The hardware facts, three-mechanism analysis, rr evidence base,
and hardware-first gating principle stand; the concrete gate is now substrate-specific.
The **codebase survey** ("What a port costs, by component") and its premises — "no arch seam
exists", "`vmm-core` unwritten" — predate Wave 4/5 and are **superseded by
`docs/ARCH-BOUNDARY.md`**, which rules the ISA seam design from a fresh file-level audit.
Captures what's known about porting this hypervisor to AArch64 (the question keeps recurring),
so the conclusions and the one load-bearing fact-correction aren't re-derived each time. Per
PLAN.md Decision 0, ARM is **post-v1 future work**: x86-64/KVM/VMX is the only designed target.

**Apple-silicon refinement (2026-07-09):** `docs/APPLE-SILICON.md` is now the primary
hardware-accelerated ARM research program. It targets macOS + Hypervisor.framework with a
Harmony monitor at virtual EL2 and ARM64 Linux at EL1/EL0. This document still owns the
cross-ARM mechanism analysis; the Apple document owns that substrate's evidence sequence and
GO/NO-GO gates. The old conclusion that Apple silicon was rejected is withdrawn: macOS 15's
nested-EL2 support creates a credible route that did not exist in the premises of this note.

The bottom line up front: **nothing fundamental precludes ARM** — Neoverse/Cortex have
EL2 and the Arm Virtualization Extensions are a fine substrate. What precludes it *for this
project* is that the determinism design rests on three x86 mechanisms with no drop-in ARM
equivalent (below), and that the central feasibility bet — precise retired-branch counting
for V-time — is **unproven on any candidate ARM core**. The viability gate is a hardware
measurement nobody has taken, not a code-cleanliness problem. **Do not build the arch
abstraction pre-emptively; a clean trait boundary cannot de-risk an unmeasured PMU.**
*(Refined 2026-07-03 by `docs/ARCH-BOUNDARY.md`: the boundary restructure is justified on
x86-hygiene grounds and may proceed; the trait freeze and all ARM-side building stay
spike-gated.)*

## The fact-correction that everything else hinges on

**DGX Spark / GB10 is NOT Neoverse V2.** These are different parts and they *trade places*
on the two axes this project cares about:

| | DGX Spark / GB10 | Grace server (GH200/GB200) |
|---|---|---|
| Cores | **Cortex-X925 + Cortex-A725**, heterogeneous (10 perf + 10 eff) | **Neoverse V2**, homogeneous |
| Architecture | **Armv9.2-A** | **Armv9.0-A** (v8.5 baseline) |
| **FEAT_ECV** (trap `CNTVCT` for V-time) | **Yes** — mandatory ≥ Armv8.6/v9.1 | **No** — absent from the V2 TRM |
| rr allowlist entry | **None** (X925/A725 not in rr's CPU switch) | Present, but config copied from N1, untested |
| Bare-metal KVM + perf | **Confirmed working + vendor-documented** | Strong inference, no hands-on report |

Consequence: the feature PLAN.md named as *the* ARM blocker — "ARM time virtualization needs
ECV (trap CNTVCT reads)" — is **present on Spark and absent on the actual Neoverse V2.** When
speccing any ARM work, never write "Neoverse V2" for Spark; the ECV difference is exactly the
kind of thing that would silently break a time-virtualization assumption.

## What does not translate from x86 (the three load-bearing mechanisms)

1. **Time virtualization.** x86: `RDTSC`/`RDTSCP` → VMX TSC-exiting → `f(V-time)`. ARM: guest
   reads `CNTVCT_EL0`; trapping it needs **FEAT_ECV** + the generic-timer offset/scaling
   registers, and a generic-timer/GIC device model instead of PIT/HPET/LAPIC. *Better on
   Spark (has ECV) than Grace (does not).*
2. **PMU instruction clock (the hard bet).** x86: guest-only **retired conditional branches**
   via perf_event, PMC-overflow → exit, single-step to exact count. ARM: the closest event is
   `BR_RETIRED` (raw `0x21`) = **retired *taken* branches** — a *different* event. V-time =
   f(taken-branches) is still deterministic in principle, but every `skid_margin` / event-
   density constant in `vtime` was reasoned for conditional branches and must be re-measured.
3. **Guest-visible CPU contract.** `docs/CPU-MSR-CONTRACT.md` (~1640 lines) is entirely Intel
   CPUID leaves + IA32_* MSRs enforced via `KVM_X86_SET_MSR_FILTER` / VMX controls. There are
   no MSRs or CPUID on ARM. The analogue freezes the `ID_AA64*` ID registers and traps system-
   register access via `HCR_EL2`/`MDCR_EL2`. Same *philosophy* (freeze a synthetic CPU,
   default-deny), entirely new contract document and enforcement backend. The data-driven
   shape (contract table → installed CPU model) ports; the x86 leaf/MSR semantics do not.

Also new on ARM, with no x86 analogue:
- **LL/SC vs LSE atomics.** Landing an injected interrupt between `LDXR`/`STXR` clears the
  exclusive monitor → `STXR` fails → retry loop → run-to-run instruction-count divergence. A
  guest built with **LSE atomics** sidesteps this; an LL/SC guest is a determinism minefield.
  (rr refuses to record LL/SC at all for a related reason.)
- **SVE non-faulting loads** (ARMv9, present on these cores) are flagged by rr as a
  predictability risk — documented worry, not a confirmed break.

## Evidence base: what rr tells us (we use rr as proxy, we do not use rr)

rr is the best external evidence that precise branch-counting is physically achievable on a
given microarch (RESEARCH.md leans on it). Findings, all from primary rr sources (GitHub
wiki/README, `src/PerfCounters.cc`, `src/PerfCounters_aarch64.h`, issues #3234/#3607/#3861,
commit b3ffa764):

- AArch64 support is **production-quality since rr 5.6.0 (Aug 2022)**, developed on Apple M1,
  Neoverse N1/V1, Cortex-A77.
- Counter precision is **microarch-gated and only empirically trusted** (Cortex-A76 /
  Neoverse-N1 and newer; A55/A75/E1 explicitly unreliable). No source *proves* BR_RETIRED is
  exact even on N1 — rr treats it as an empirical judgement passing a crude floor-check.
- **Neoverse V2: zero tested data.** In rr's allowlist since Oct 2024, but added speculatively
  from ARM's published part numbers with a PMU config **byte-identical to N1, not measured on
  V2**. No public record of anyone recording/replaying on V2 silicon (Graviton4/Grace/GB200).
- **Cortex-X925 / A725 (i.e. Spark): not in rr's allowlist at all** — rr would `FATAL "Unknown
  aarch64 CPU type"`. Even less characterized than V2.
- The closest tested relatives (N1/V1, i.e. Graviton2/3) have a **documented arm64 kernel
  PMU-interrupt-missed bug on core migration** (#3607) — precisely the failure that would
  break precise injection (missed overflow → never breaks out of `KVM_RUN`). Mitigated by
  core-pinning (which we do anyway), but unresolved in general.

## Apple silicon: a distinct nested-EL2 route

Apple silicon is not just another KVM/arm64 host candidate. Hypervisor.framework is the L0,
and the credible design puts a small Harmony monitor at the virtual EL2 that Apple exposes on
supported hardware, with the deterministic Linux subject below it at EL1/EL0. That monitor
would own nested stage-2 translation, counter/time traps, PMU and debug controls, and exact L2
interrupt scheduling. The macOS launcher remains a thin outer run loop.

This route matters for four reasons:

1. rr already supports Apple M-series branch counting when the same silicon runs Linux, so
   the physical PMU is not disqualified in principle.
2. macOS 15 added public nested-virtualization support, including virtual EL2 and GIC
   hypervisor-control state.
3. Apple's SDK documents framework-emulated PMU register behavior for an EL2-enabled guest,
   making a nested PMU experiment possible without a private macOS PMU API.
4. Owning EL2 lets the monitor trap the L2 counter/time surface that a direct EL1
   Hypervisor.framework backend cannot close.

The open bet is narrower and harder than “does ARM virtualization work?”: does Apple's nested
virtual PMU expose a deterministic L2-only work event, deliver overflow reliably with bounded
skid, and support exact ARM debug-step landing through L2 exceptions? None of those properties
is promised by the public API. `docs/APPLE-SILICON.md` orders the experiments so this bet is
measured before Linux, snapshot optimization, or the production architecture refactor.

Apple silicon also sharpens the heterogeneous-core risk. macOS exposes no supported analogue
of Linux's hard `taskset` core pinning. A selected event must therefore retain identical
semantics across performance/efficiency-core migration, or the backend must find a real hard
mechanism that prevents migration. A QoS hint is not an enforcement mechanism.

## DGX Spark as a host: access is GREEN, determinism is the open risk

The "is it a vendor-locked appliance?" worry **does not materialize** (vendor-documented
unless noted):
- DGX OS = Canonical Ubuntu 24.04 arm64; full root, `apt source` custom-kernel path, open
  UEFI, Secure Boot user-disablable, free reflash image.
- **KVM works** (KVM-accelerated QEMU, Linux + Windows guests) — proving **bare-metal EL2 is
  available**. NVIDIA labels host virtualization "not officially supported" and **GPU
  passthrough into a guest fails** — both irrelevant to this project (no GPU/DMA in guest).
- **perf_event/PMU access is first-party documented**: DGX Spark Porting Guide gives the
  unlock (`kernel.perf_event_paranoid=-1`, `linux-tools-$(uname -r)`) and points at the
  X925/A725 PMU event lists.

So spike #1 (below) is *runnable* on a Spark — the access half is solved. The remaining risk
is entirely PMU determinism on uncharacterized, heterogeneous client cores.

## The gates: one mechanism, substrate-specific experiments

For a bare-metal Linux/KVM ARM host, before any line of ARM `vmm-core`, run PLAN.md's PMU
precise-count spike on the actual box:
measure whether `BR_RETIRED` (taken-branch) counting is **bit-deterministic** on one pinned
X925 (Spark) or V2 (Grace), whether overflow interrupts fire reliably out of `KVM_RUN`, and
the **skid bound** (→ a port-specific `PlannerConfig::skid_margin`). Pin to one core type
(heterogeneous PMUs on Spark; rr's big.LITTLE caveat applies). Confirm `ID_AA64MMFR0_EL1.ECV`
on real silicon. If this spike fails, no arch abstraction saves the port — which is the
strongest reason not to invest in abstraction first.

For Apple silicon, the equivalent gate necessarily includes a minimal launcher, virtual-EL2
monitor, and L2 assembly payload because the property under test belongs to the *nested*
virtual PMU, not a macOS userspace counter API. This bounded code is experiment apparatus, not
premature production backend work. `docs/APPLE-SILICON.md` AS-0 through AS-4 are the gate;
AS-2 (count determinism), AS-3 (overflow/skid), and AS-4 (exact landing) are existential.

## What a port costs, by component (SUPERSEDED — see `docs/ARCH-BOUNDARY.md`)

> **This section is superseded (2026-07-03).** It surveyed a pre-Wave-4 tree; `vmm-core`,
> `vmm-backend`, `lapic`, `vm-state`, the task-58 control server, and all seven dissonance
> crates have since landed. `docs/ARCH-BOUNDARY.md` replaces the estimates below with a
> file-level audit (~85% of the tree already arch-blind; coupling concentrated in the
> `vmm-backend` value types, five `vmm-core` modules, `lapic`, and the guest payloads) and
> rules the seam design. Kept for archaeology:

- **Ports as-is (~60%, the merged pure-logic crates):** `vtime` arithmetic, `snapshot-store`
  CoW, `hypercall-proto` wire format, `unison`. The `CpuBackend` trait
  (`consonance/vtime/src/planner.rs`) already models the one hard hardware seam correctly; its
  *backend* is the rewrite, not the trait. The VMCALL transport shim is the one hypercall-side
  x86 bit (ARM uses `HVC`).
- **Rewrite (~40%):** `vmm-core` (unwritten; KVM/VMX-specific from line one), all guest
  payloads (x86 boot/IDT/port-I/O/CPUID, pinned to `x86_64-unknown-none`), and the CPU
  contract document.
- **No arch seam exists yet** (no `#[cfg(target_arch)]` anywhere). The recommended discipline
  — adopt *as `vmm-core` is written*, costs ~nothing, makes ARM an "add a backend" rather than
  an "untangle": split `vmm-core` (arch-neutral orchestration) from `vmm-vmx` (the KVM/VMX
  backend) at a crate boundary, and give the other two hardware seams (memory/snapshots,
  guest loader) the same trait treatment `CpuBackend` already has for time. Then "what ARM
  must reimplement" = "the set of backend traits," small and enumerable.

## One-line recommendation

Set the ARM research priority to the novel Apple-silicon nested-EL2 backend in
`docs/APPLE-SILICON.md`. Spark/Grace remain useful comparative KVM hosts, but they are not the
primary target. Start with only enough parallel monitor/launcher apparatus to answer the
nested-PMU and exact-landing questions. If those gates return GO, continue that same isolated
codebase through deterministic Linux and snapshot/branching; do not pause for a consonance
trait refactor. Let measurements — not TCG confidence or abstraction work — decide whether
the hardware-accelerated Apple backend proceeds.
