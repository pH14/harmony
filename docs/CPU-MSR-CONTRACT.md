# Guest-visible CPU/MSR determinism contract

| Field | Value |
|---|---|
| contract-version | 3 |
| reference kernel | Linux **v6.18.35** — equals `guest/linux/versions.lock` `KERNEL_VERSION=6.18.35` (tarball sha256 `f78602932219125e211c5f5bfd84edcfd4ec5ce88fc944f8248413f665bef236`); all `arch/x86/kvm/x86.c` and `arch/x86/include/asm/msr-index.h` citations are to that tag |
| baseline microarchitecture | **`det-cfl-v1`** — Coffee Lake-S client (Intel Core i9-9900K, `06_9e_0c`, microcode `0xf8`); the named baseline of the frozen CPUID model (§2). The host-specific values are derived from and cited to the box dump under `docs/fragments/cfl-baseline/` |
| contract hash | `contract_hash` = SHA-256 of the canonical serialized form, computed per §6 from the assembled tables — never hand-written into this document |

This document is the exhaustive, default-deny enumeration of everything the guest may
observe of its CPU: which CPUID leaves exist and their frozen values, which MSRs are
readable/writable and with what semantics, which instructions trap and what they return,
and how the timer-device surface is virtualized. It exists because trapping RDTSC is
necessary but nowhere near sufficient — Linux/KVM exposes host time and other
nondeterminism through many side doors (INTEGRATION.md §7) — and because every leak
vector must be closed by **decision**, recorded here, rather than by accident.

**Authority.** This contract carries the same authority as `docs/INTEGRATION.md`, which
mandates it be authored before any vmm-core code (§7): vmm-core implements it, it does
not negotiate with it. Where implementation and contract disagree, the implementation is
wrong. The contract changes only by editing this document and bumping the version per §6
— never by implementation drift. It is subordinate to INTEGRATION.md §1–§7 and PLAN.md's
trap table; any contradiction discovered later is raised as a `[question]`, not resolved
by a silent local choice.

**How to read the tables.** The tables — not the prose — are the normative surface; the
prose explains and motivates but binds nothing. Column grammar, uniform across the
document:

- **MSR tables**: `| MSR | Index | Read | Write | Rationale | Citation |`. `MSR` is the
  `arch/x86/include/asm/msr-index.h` name at v6.18.35 (or the architectural name where
  no define exists); `Index` is the MSR index in hex — a single index, or an inclusive
  hex range for range rows, which must carry a stated, mechanically checkable match rule
  against `msr-index.h` at the same tag (never a prose class like "neighbors"); `Read`
  and `Write` are each exactly one disposition token from the §3 vocabulary (below);
  `Rationale` is one line naming the INTEGRATION.md §7 leak vector the row closes, or
  `architectural` for stateful allows; `Citation` lists primary sources (Intel SDM
  volume/chapter, Linux `Documentation/virt/kvm/api.rst`, kernel source at v6.18.35,
  RESEARCH.md entries).
- **CPUID table** (§2): one row per (leaf, subleaf, register) with the frozen value or
  the masking rule that produces it, plus rationale and citation; a final default rule
  disposes of every leaf not explicitly listed.
- **Instruction table** and **timer-device table**: one row per instruction/device with
  the enforcement mechanism (the VMX execution control, the interception backing a
  CPUID-hidden #UD, or permitted-with-emulation) and what the VMM returns.

Disposition tokens (full normative semantics in §3; constraints repeated here so any
table can be read standalone): `allow-fixed(value)` — read returns the constant; a
**read-only** disposition, its write column must be `deny-gp` or `deny-ignore-write`;
`allow-stateful` — architecturally guest-writable state, read/written normally and
captured in the `vm_state` blob per INTEGRATION.md §4; `emulate-vtime` — value derived
from V-time via a named `consonance/vtime` formula; `emulate-timerqueue` — writes schedule
deadlines on the userspace `TimerQueue`; `emulate-apic` — the x2APIC range, governed by
the per-register sub-table; `deny-gp` — access is trapped, loudly logged, then #GP is
injected (§1 defines the mechanism); `deny-ignore-write` — the write is dropped, loudly
logged, never silently. A row marked `deny-gp` + `[question]` is a **decided** row
(safe-by-default; the question invites a later, deliberate loosening), not a TBD; rows
without a disposition do not exist in a valid revision of this contract.

## 1. Scope & default-deny

The guest CPU surface is **allow-listed**. The unit of allowance is a table row in this
contract; everything else — every MSR index, CPUID leaf, instruction behavior, or timer
device not named by a row — is denied by construction. Scope covers the whole
guest-visible CPU surface of the single-vCPU Intel/VMX guest defined by task 04's pinned
kernel: CPUID, all RDMSR/WRMSR accesses in all guest modes, the timing/entropy/perf
instruction set (§4 instruction table), the x2APIC MSR range, and the full guest-visible
time-source surface (§5: PIT, RTC/CMOS, HPET, ACPI PM timer, LAPIC timer). Out of scope: AMD, multi-vCPU, and anything host-side that the guest
cannot observe.

**Mechanism (normative).** Default-deny is implemented with two KVM facilities together,
configured before the first `KVM_RUN` and never changed while a vCPU runs:

1. `KVM_X86_SET_MSR_FILTER` (api.rst §4.97) installed with
   `KVM_MSR_FILTER_DEFAULT_DENY`, so any MSR index not covered by an allow bitmap is
   denied. The allow bitmaps (up to `KVM_MSR_FILTER_MAX_RANGES` = 16 ranges, with
   per-direction `KVM_MSR_FILTER_READ` / `KVM_MSR_FILTER_WRITE` flags) contain **exactly
   the rows whose disposition is `allow-stateful`** — the architecturally stateful MSRs
   KVM already virtualizes correctly in-kernel and which §4 of INTEGRATION.md captures in
   `vm_state`. Every other disposition — `allow-fixed`, `emulate-vtime`,
   `emulate-timerqueue`, `deny-gp`, `deny-ignore-write` — is *denied in the filter on
   purpose*, so the access reaches userspace where the VMM supplies the contractual
   answer.
2. `KVM_CAP_X86_USER_SPACE_MSR` (api.rst §7.21/§8.26) enabled with a mask including
   `KVM_MSR_EXIT_REASON_FILTER`, **before any filter is installed and left enabled**
   (api.rst §4.97 requires this ordering; otherwise KVM may inject #GP in-kernel instead
   of exiting). With it, a filter-denied access exits to userspace as
   `KVM_EXIT_X86_RDMSR` / `KVM_EXIT_X86_WRMSR` with `kvm_run.msr.reason =
   KVM_MSR_EXIT_REASON_FILTER`. The filter alone does **not** satisfy this contract: a
   bare filter produces an in-kernel #GP with no logging, which is a silent event, and
   silent is forbidden. The mask also includes `KVM_MSR_EXIT_REASON_UNKNOWN` and
   `KVM_MSR_EXIT_REASON_INVAL`, so accesses that pass the filter but are unknown to KVM
   or architecturally invalid also surface to userspace loudly instead of being resolved
   by in-kernel defaults.

**Loud-event logging policy (normative).** Every userspace MSR exit is logged *before*
any architectural effect is committed, with at minimum: access direction (RDMSR/WRMSR),
MSR index (`kvm_run.msr.index`), the written data for WRMSR (`kvm_run.msr.data`), the
guest RIP at the faulting instruction, the exit reason, the current work counter /
V-time, and the disposition applied. Logging is host-side output only and cannot perturb
guest-visible state. Then, by disposition: `deny-gp` sets `kvm_run.msr.error = 1` so KVM
injects #GP on re-entry (api.rst §5, `kvm_run.msr`); `deny-ignore-write` logs, leaves
`error = 0`, and discards the data; `emulate-*` and `allow-fixed` reads compute the
contractual value into `kvm_run.msr.data`. **Never a silent passthrough, never a silent
zero**: no code path may answer an MSR read with an invented value (including 0) unless
a table row says exactly that value, and no write may be swallowed without a log line.
An access to an index with *no* row in the reference set is denied by
`KVM_MSR_FILTER_DEFAULT_DENY`, logged as **off-contract**, and answered `deny-gp` — and
it is additionally a contract defect: the index is triaged into a new table row (usually
`deny-gp` + rationale) with a version bump per §6, because the reference set was supposed
to be exhaustive.

**Reads vs writes (stated separately).** The two directions are filtered independently
(`KVM_MSR_FILTER_READ` vs `KVM_MSR_FILTER_WRITE`) and dispositioned independently —
every row in §3 carries one token per direction.

- *Reads default to `deny-gp`.* A read is answered only by an explicit row
  (`allow-stateful`, `allow-fixed(value)`, `emulate-*`); anything else injects #GP after
  logging. There is no "return 0 for unknown reads" path — that is the silent-zero
  hazard by name.
- *Writes default to `deny-gp`*, not `deny-ignore-write`. Silently swallowing a write
  diverges from architectural behavior invisibly — the guest believes state it never
  set. `deny-ignore-write` is never a default: it is a deliberate per-row choice, used
  only where a row's rationale shows #GP would break an otherwise-deterministic guest
  path, and it still logs every occurrence.

**Enforcement carve-outs (normative — the filter is not the whole story).** Three
documented KVM behaviors mean the MSR filter cannot be the sole enforcement mechanism,
and the contract accounts for each (api.rst §4.97 warning, v6.18.35):

- **x2APIC MSRs (0x800–0x8FF) cannot be filtered** — KVM silently ignores filter ranges
  covering them. The x2APIC class (§3.12) therefore hides x2APIC entirely — CPUID.1:ECX[21]=0,
  `IA32_APIC_BASE.EXTD` reserved, no `KVM_CREATE_IRQCHIP`; the guest's only APIC is the
  userspace xAPIC MMIO page — and takes `deny-gp` across the whole block, enforced by
  the architectural #GP (x2APIC mode can never be entered) surfaced loudly via
  `KVM_MSR_EXIT_REASON_INVAL`, not by `KVM_X86_SET_MSR_FILTER`. No row in the x2APIC
  range may claim the filter as its mechanism. *(Assembly note: an earlier draft of
  this carve-out anticipated `emulate-apic` rows under a split irqchip; updated to
  match §3.12's resolution — at v6.18.35 a split irqchip keeps the LAPIC, and its
  host-hrtimer timer, in the kernel.)*
- **MSR accesses that are side effects of instruction execution are not filtered**
  (e.g. RDPID reads `IA32_TSC_AUX`, SYSENTER reads the SYSENTER MSRs; MSRs loaded via
  dedicated VMCS fields at VM-entry/exit are likewise unfiltered). Consequently the
  contract never relies on the filter to determinize an MSR that an instruction can
  reach implicitly: every such MSR must be safe **by value** — either `allow-stateful`
  guest state captured in `vm_state`, or rendered unreachable by hiding the feature in
  the frozen CPUID model *and* intercepting the instruction per the instruction table.
  CPUID↔MSR↔instruction consistency is an acceptance gate for exactly this reason.
- **CPUID is not an MSR** and needs no filter: CPUID unconditionally VM-exits under VMX.
  The frozen model of §2 is installed once via `KVM_SET_CPUID2` with exactly the
  contract's leaves and values — host leaves are never inherited (INTEGRATION.md §7,
  "CPUID stability").

The MSR **reference set** that §3 dispositions exhaustively is defined in §3's preface:
the union of the static arrays behind `KVM_GET_MSR_INDEX_LIST` and
`KVM_GET_MSR_FEATURE_INDEX_LIST` in `arch/x86/kvm/x86.c` at v6.18.35 (`msrs_to_save_*`,
`emulated_msrs_all`, and the msr-based-features arrays), every MSR named in
INTEGRATION.md §7, and the named classes expanded by stated match rules against
`msr-index.h` at the same tag. Completeness is mechanically checkable against those
sources; the default-deny filter is what makes any omission safe (loud `deny-gp`) rather
than a leak.

**Enforcement backend dependency (normative — see [question] Backend in §4).** The MSR surface
(§3) runs on **stock** KVM: `KVM_X86_SET_MSR_FILTER` + `KVM_CAP_X86_USER_SPACE_MSR` are
upstream and need no patch. Most instruction dispositions (§4) are also stock-serviceable —
CPUID via `KVM_SET_CPUID2`, HLT via `KVM_EXIT_HLT`, RDPMC via KVM's in-kernel #GP with no
vPMU, MONITOR/MWAIT via KVM's in-kernel intercept, XSETBV via KVM's in-kernel XCR0 validation,
and the VMX/EPT opcodes via KVM's #UD when nested is off. The **hypercall channel** is also
stock-serviceable, but **not via `VMCALL`**: stock KVM services `VMCALL` in-kernel and returns
`-ENOSYS` to the guest for our magic number (`0x31504348`), never surfacing a
`KVM_EXIT_HYPERCALL` to userspace. The channel therefore rides a **port-I/O doorbell**: a single
`OUT DOORBELL_PORT` → `KVM_EXIT_IO` (the guest then reads the response length from the response
frame's self-describing header — one atomic exit, no `IN`; INTEGRATION.md §1, reworked in task 20)
on stock KVM; a `VMCALL` doorbell is a patched/direct-VMX-backend-only variant (task 21).
**Three surfaces are *not* stock-serviceable**, and the contract depends on a backend that
provides them:

- **RDTSC/RDTSCP → f(V-time)** requires **RDTSC-exiting** (VMX primary proc-based control).
  Stock KVM virtualizes the TSC *in-kernel* via offset/scaling applied to the free-running
  **host** TSC (host real time, **not** V-time) and exposes no userspace RDTSC exit — so the
  contract's `RDTSC = f(V-time)` rule is **unachievable on stock KVM**.
- **RDRAND/RDSEED → seeded stream** requires **RDRAND/RDSEED-exiting** (VMX secondary
  controls). Stock KVM does not intercept them; they hit the hardware RNG (true entropy,
  nondeterministic).
- **IA32_TSC_DEADLINE (0x6e0) `deny-gp` enforcement.** The same in-kernel WRMSR **fastpath**
  that makes deadline-mode *emulation* impossible (round-7 #6) also defeats the *deny*: under
  `KVM_IRQCHIP_NONE`, `handle_fastpath_wrmsr` services a guest `WRMSR 0x6e0` (a silent success)
  **before** the userspace MSR filter, so the §1-required logged **#GP cannot be delivered**.
  The `deny-gp` therefore holds only under the patched-KVM/direct-VMX backend (where the write
  reaches the filter and faults); under **stock KVM** it degrades to a **silent swallow** for an
  *out-of-scope adversarial* guest. This is determinism-safe in practice: the **cooperative
  guest never writes 0x6e0** (TSC-deadline is hidden, CPUID.1:ECX[24]=0), so the degraded path
  is unreachable for in-scope guests — but the contract does **not** claim plain `deny-gp`
  enforcement for 0x6e0 on stock KVM (§3.3 row carries this caveat).

The contract therefore **assumes a VMX/KVM backend that can program and surface these
exits/denials** — a patched KVM exposing them to userspace (as the MSR filter does), or a
direct-VMX backend that owns the VMCS. This is a **load-bearing dependency for determinism**,
not a nicety: without it, RDTSC is host-time-derived, RDRAND/RDSEED are true entropy, and the
0x6e0 deny silently no-ops for an adversarial guest. PLAN.md's
platform table claims "VMX exit controls for everything we must trap (RDTSC/RDRAND/RDSEED/…)"
and Phase 1 "Enable TSC/RDRAND/RDSEED exiting", yet names the backend as rust-vmm/`kvm-ioctls`
(stock) and never says how these exits are surfaced; INTEGRATION.md §6 defers a possible kernel
patch. Because PLAN.md is underspecified here, the backend is **raised for integrator
ratification** ([question] Backend, §4) rather than assumed — the contract is written for
option (a)/(b) (patched-KVM / direct-VMX) and notes what breaks under stock-only.
**Ratified — Ruling R-Backend (`docs/R-BACKEND.md`): option (a).** `PatchedKvmBackend` is the
determinism backend, decoupled behind a `Backend` trait that nothing above it may branch on;
stock `KvmBackend` is bring-up-only and direct-VMX (b) is preserved. The three not-stock-serviceable
surfaces above are exactly that trait's enumerated, fail-closed backend-dependent exits.

### 1.1 Host-homogeneity assumption — the determinism domain (normative)

**The determinism guarantee is defined over a homogeneous, single-tenant, pinned-core
fleet.** Integrator ruling: this is *the model* — not a fallback. Every contract row that
says "host-homogeneity" relies on this section; read it first. The guarantee is: *two runs
of the same guest image with the same seed produce bit-identical architectural state* —
**provided** every host in the fleet is, precisely:

- **Same CPU identity**: vendor `GenuineIntel`, family/model/stepping **06_9EH stepping 0CH**
  (Coffee Lake-S client, Intel Core i9-9900K), matching §2's frozen `det-cfl-v1` — so the
  *physical* instruction set (which instructions exist vs `#UD`) is identical across the fleet.
- **Same microcode revision** — pinned, identical on every host (microcode changes
  instruction behavior and the speculation-MSR surface).
- **Single pinned core per VM**: one vCPU pinned to one physical core, never migrated, with
  **no co-resident tenant** sharing that core's caches/predictors/SMT sibling. (PLAN.md:
  "one vCPU, period"; single-tenant deployment.)

**vmm-core asserts this at VM start and refuses to run on any mismatch.** The asserted
host-baseline is itself hashed config (the `host-assert` records, §6), so a drifted host
cannot silently run: vmm-core checks host `CPUID(1)` family/model/stepping == 06_9EH/0CH, host
microcode revision == the pinned value (`0xf8`), host `MXCSR_MASK == 0x0000FFFF`, host
MAXPHYADDR ≥ 39, and the **physical presence/absence of every variance instruction** the
contract depends on (below) — and aborts otherwise.

**What homogeneity gives, and the one thing it does NOT.** Identical CPUs running the same
architectural input produce the same architectural output, and they agree on which
instructions `#UD`. Homogeneity does **not**, by itself, make an instruction deterministic if
its result depends on **hidden microarchitectural state that varies run-to-run on a single
core** — XINUSE, the XSAVE init/modified-tracking bits, branch/Thread-Director predictor
history, cache/TLB occupancy, in-flight state. That state is **not** captured in the
reproduced architectural/`vm_state`, so two runs *on the very same pinned core* can differ.
Homogeneity is a cross-host property; this hazard is within-host run-to-run. Therefore
**CPUID-hiding is not an enforcement mechanism**: an instruction that does not consult CPUID
executes natively on a host that physically implements it, regardless of the virtual CPUID
bit — "the bit is clear so it `#GP`s/is unreachable" is **false** for such instructions.

**Determinism-class framework (normative).** Every instruction that could observe host or
hidden-µarch state is assigned **exactly one** determinism class, recorded in the hashed
`insn` record (§6) and the §4 table:

| Class | Token | Meaning | Valid only when |
|---|---|---|---|
| (a) | `arch` | Output is a pure function of reproduced architectural/`vm_state` — no hidden µarch input. | The value path provably reads only guest state (e.g. RDPID reading the guest-echoed `IA32_TSC_AUX` that KVM loads into the physical MSR during guest execution — never the host's per-core value). |
| (b) | `fault-absent` | The instruction `#UD`/`#GP`s because the **baseline host physically lacks it**. | vmm-core asserts host absence at VM start (a `host-assert … absent` record). **Not** valid as a bare "CPUID hides it" claim — only when the silicon genuinely faults. |
| (c) | `intercept` | A VMX control / unconditional exit / host-MSR pin makes vmm-core supply a fixed result. | The op actually exits or is pinned (RDTSC-exiting, XSETBV exit, MWAIT-exiting, IA32_TSX_CTRL pin, …). |
| (d) | `invisible` | The only effect is on timing / µarch history, which V-time abstracts away; there is **no** architectural output or memory write. | The op writes no register/memory the guest can read (SERIALIZE: pure fence; HRESET: predictor-history reset). |
| (e) | `scope` | The op *can* inject run-to-run nondeterminism (hidden-µarch result), is **not** interceptable (no VM-exit control), and is **not** absent on the baseline host. There is **no enforcement mechanism**; determinism holds only for the project's **cooperative, CPUID-respecting guest** (§1.2), which does not execute the op because it honors the frozen CPUID. | The guest is the project's own Linux payload: it patches the XSAVEOPT/XSAVEC/XSAVES/XGETBV1 code paths out per the hidden CPUID bits (§2) and so never emits these opcodes. The **residual risk is stated, not defended** (§1.2): an adversarial guest that executes the hidden opcode anyway observes hidden µarch state and is **out of scope**. (An earlier draft claimed a build-time "opcode scan" as enforcement — **removed as unsound**: XGETBV(ECX=1) shares opcode `0f 01 d0` with the permitted XGETBV(ECX=0), and Linux carries the XSAVE opcodes as CPUID-gated alternatives a static scan cannot reason about.) |

**Danger zone (explicit) — class (e) and the cooperative-guest boundary.** The ops where a
previous draft claimed "unreachable / `#GP` because CPUID hides it" but the op does **not**
check CPUID and the baseline host **does** support it are **XGETBV (ECX=1)** and **XSAVEOPT /
XSAVEC / XSAVES / XRSTORS** — Coffee Lake-S physically implements all of these
(box CPUID.0xD.1:EAX[0..3] = 0xf set). Their results depend on hidden µarch tracking (XINUSE;
init/modified/compacted save bytes that vary run-to-run on one core), so they are **not** made
deterministic by homogeneity, **cannot** be intercepted (no VMX control), and do **not** fault
on the baseline. **There is no mechanism that forces them deterministic for an arbitrary
guest.** They are governed by the cooperative-guest threat model in **§1.2**: the determinism
guarantee is for the project's own CPUID-respecting Linux payload, which never emits them; an
adversarial guest that does is an explicit, documented **residual risk that is out of scope**.

**Per-instruction determinism classification (normative summary; full rows in §4):**

| Instruction(s) | Class | Why deterministic on identical pinned cores |
|---|---|---|
| CPUID, VMCALL, RDTSC, RDTSCP, RDRAND, RDSEED, RDPMC, MONITOR, MWAIT, XSETBV, HLT, VMX/EPT group (VMXON/VMXOFF/VMLAUNCH/VMRESUME/VMPTRLD/VMPTRST/VMCLEAR/VMREAD/VMWRITE/INVEPT/INVVPID) | (c) `intercept` | VMX exit / unconditional exit → vmm-core supplies a fixed result; no hidden-µarch input reaches the guest. MONITOR/MWAIT (present on Coffee Lake-S) are intercepted → injected `#UD`; the VMX/EPT opcodes VM-exit unconditionally and KVM injects `#UD` (nested VMX not exposed). (TSX — XBEGIN/XEND/XTEST/XABORT — was class (c) `intercept` on the TSX-present SKX baseline; on the TSX-absent Coffee Lake-S baseline it is class (b) `fault-absent`, below.) |
| XGETBV(ECX=0), FXSAVE/XSAVE/XRSTOR/FXRSTOR (plain) | (a) `arch` | Output = pure function of architectural state (XCR0; FPU state + pinned MXCSR_MASK) — no hidden µarch input. |
| RDPID, SERIALIZE, SHA1/SHA256, HRESET, PCONFIG, UMWAIT/TPAUSE/UMONITOR, **XBEGIN/XEND/XTEST/XABORT (TSX/RTM)** | (b) `fault-absent` | **Verified physically absent on Coffee Lake-S** (RDPID=Ice Lake; SERIALIZE/HRESET=Alder Lake; SHA-NI=Ice Lake; PCONFIG=Ice Lake-SP; WAITPKG=Tremont/Alder Lake; **RTM/HLE physically absent — box CPUID.7.0:EBX[4,11]=0, `IA32_TSX_CTRL` (0x122) `#GP`s — so XBEGIN/XEND/XTEST/XABORT `#UD` natively, no host pin needed**) → deterministic `#UD`; vmm-core asserts host absence at VM start (TSX via the `rtm-disabled` host-assert, which reads EBX[11] and passes on absence). Robustness note for a future baseline that *adds* one: RDPID/SHA satisfy (a) (RDPID reads guest-loaded `IA32_TSC_AUX`, SHA = f(inputs)); SERIALIZE/HRESET satisfy (d) (no architectural output — pure fence / predictor-history, V-time-hidden); UMWAIT/TPAUSE then satisfy (c) (vmm-core must clear the VMX "enable user wait and pause" control → `#UD`); a TSX-bearing baseline would return TSX to class (c) via the `IA32_TSX_CTRL = RTM_DISABLE` pin (deterministic always-abort). |
| **XGETBV(ECX=1), XSAVEOPT, XSAVEC, XSAVES, XRSTORS** | (e) `scope` | **Verified physically present on Coffee Lake-S** (box CPUID.0xD.1:EAX[0..3] = 0xf); hidden-µarch result (XINUSE / init-modified save bytes); **not** interceptable (no VMX control), **not** absent on the baseline → **no enforcement mechanism**. In scope only under the §1.2 cooperative-guest model (the project's CPUID-respecting payload patches these paths out and never emits them); an adversarial guest executing one is a documented **residual risk, out of scope**. |

### 1.2 Threat model — cooperative guest (normative)

**The determinism guarantee is for a *cooperative*, CPUID-respecting guest: the project's own
pinned Linux payload (task 04).** This is the integrator-ratified threat model and it bounds
every class-(e) row above. It exists because a small set of instructions — **XGETBV(ECX=1)**
and the XSAVE-optimization variants **XSAVEOPT / XSAVEC / XSAVES / XRSTORS** — are, on the
Coffee Lake-S baseline, simultaneously:

- **not VMX-trappable** — there is no execution control that causes a VM-exit on them, so
  vmm-core cannot interpose; and
- **physically present on the baseline host** — so hiding their CPUID bits does **not** make
  them `#UD` (the silicon decodes them regardless of the virtual CPUID), and they are not
  class-(b) `fault-absent`.

For such an op the **only** thing that keeps execution deterministic is that **the guest never
executes it**. That holds for the cooperative payload by construction, not by interception:

- The pinned Linux kernel selects its FPU save/restore path from the **virtual** CPUID.0xD.1
  it reads (frozen with XSAVEOPT/XSAVEC/XSAVES/XGETBV1 = 0, §2), so it compiles/patches those
  paths out and emits only plain `XSAVE`/`XRSTOR`/`FXSAVE` and `XGETBV` with ECX=0; and
- the static busybox userland performs no XSAVE-family or `XGETBV` operations (the kernel owns
  xstate).

**No static "opcode scan" is claimed as enforcement** — that approach was proposed in an
earlier round and is **unsound**, for two concrete reasons the re-review identified: (1)
`XGETBV(ECX=1)` shares the exact opcode `0f 01 d0` with the *permitted* `XGETBV(ECX=0)` (the
ECX value is a runtime register, not part of the encoding), so no byte scan can forbid XGETBV1
without also forbidding the allowed XGETBV0; and (2) stock Linux x86 FPU code carries the
XSAVEOPT/XSAVEC/XSAVES encodings as **CPUID-gated alternatives** (and can emit code at
runtime), which a static scan cannot soundly reason about. The defense is therefore *the
cooperative guest's adherence to the frozen CPUID*, nothing more.

**Residual risk (explicit; documented, not defended).** An **adversarial or buggy** guest that
deliberately executes one of these hidden opcodes (e.g. `mov ecx,1; xgetbv`, or a raw
`xsaveopt`) will read hidden, run-to-run-varying microarchitectural state (XINUSE; the XSAVE
init/modified-tracking bits; compacted-area gap bytes) that is **not** part of the reproduced
architectural/`vm_state`. Two runs of such a guest **on the very same pinned core can
diverge.** This is **out of scope** for the determinism guarantee. The contract does **not**
claim "deterministic for any guest" — it claims "deterministic for the cooperative task-04
payload (and any guest that likewise respects the frozen CPUID), on the homogeneous pinned-core
fleet of §1.1." The only way to extend the guarantee to arbitrary guests would be a baseline
microarchitecture that *physically lacks* XSAVES/XGETBV1 (turning these into class-(b)
`fault-absent`), which would be a different `det-cfl-v1`.

Everything **outside** class (e) is robust against an adversarial guest too: class (b) ops
genuinely `#UD` (host lacks them), class (c) ops VM-exit or are neutralized by a hashed control
pin, and class (a)/(d) ops have no hidden-µarch-dependent architectural output. Two ops that
*look* like class-(e) candidates are in fact **hard-closed**, robust against any guest — they are
**not** part of the cooperative residual:

- **TSX** (XBEGIN/XEND/XABORT/XTEST): **physically absent on the Coffee Lake-S baseline** —
  RTM/HLE are not implemented (box CPUID.7.0:EBX[4,11]=0; `IA32_TSX_CTRL` (0x122) `#GP`s), so
  these opcodes `#UD` natively. This is class (b) `fault-absent`, robust against any guest by
  silicon — no host `IA32_TSX_CTRL` pin is installed or needed (the SKX baseline pinned
  `RTM_DISABLE` to force a deterministic always-abort; the TSX-absent box does not have that MSR
  and does not need it). `host-assert rtm-disabled true` is satisfied by physical absence
  (XBEGIN `#UD`s; the probe reads EBX[11]). §4 TSX row.
- **RDPKRU/WRPKRU + PKRU**: hiding PKU makes `CR4.PKE` a KVM-reserved bit, so the guest cannot
  set it and RDPKRU/WRPKRU `#UD` unconditionally (CR4.PKE=0); the physical PKRU is therefore
  unreachable and is also absent from the XCR0 menu / `vm_state`. `host-assert
  cr4-force-reserved [PKE, PKS]`. §4 RDPKRU/WRPKRU row.

So the cooperative-guest caveat — the **class-(e) set — is exactly XGETBV(ECX=1) and
XSAVEOPT/XSAVEC/XSAVES/XRSTORS** (uninterceptable, present on Coffee Lake-S, no control to pin). PKRU is
**not** in that set (it has a hard CR4 closure); the earlier prose that implied only "the
XSAVE-optimization set" needed consideration is corrected here by naming PKRU and showing why
it is hard-closed rather than residual.

## 2. Frozen CPUID model — baseline `det-cfl-v1`

Baseline name: **`det-cfl-v1`** — a synthetic single-socket, single-core, single-thread
client CPU modeled on Coffee Lake-S (GenuineIntel, family 6, model 0x9E, stepping 0xC — the
Intel Core i9-9900K determinism box, microcode `0xf8`), with every leak-vector feature
stripped per the default-deny rule. The host-forced identity values (family/model/stepping,
MAXPHYADDR, cache geometry, the `IA32_ARCH_CAPABILITIES` fingerprint) are **derived from and
cited to the actual box dump** under `docs/fragments/cfl-baseline/` — they are not guessed —
because the §1.1 host-assert refuses to run unless the live box matches them. It deliberately
does **not** bit-match the shipping 9900K's full leaf set (the feature/cache/brand surface is
synthetic, default-deny stripped): the validity criterion is (a) architecturally
self-consistent per the Intel SDM, (b) the host-forced cells match the box, and (c) it boots
the task-04 pinned guest (linux 6.18.35, tinyconfig + `guest/linux/config-fragment`). The
model is a config artifact: its canonical serialization is hashed into the determinism gate
and any value change bumps the version (§6, Versioning & hashing).
Antithesis likewise emulates one fixed Skylake-based model; rr records-and-replays every
leaf — both establish that a frozen full-leaf model, never host inheritance, is the proven
approach (INTEGRATION.md §7 "CPUID stability").

Service rule (enforcement): CPUID VM-exits unconditionally (Intel SDM Vol.3C §25.1.2);
every leaf is answered from this table and only this table. Concretely: the
`KVM_SET_CPUID2` table is generated byte-for-byte from this model — `KVM_GET_SUPPORTED_CPUID`
is never consulted, no host leaf is ever copied. The frozen TSC frequency is **2.0 GHz**
(virtual TSC = 2 × V-ns exactly; the ×2 integer ratio is snapshot-safe per INTEGRATION.md
§4's integer-ratio ruling) and the frozen core-crystal (ART / LAPIC-timer input) clock is
**25 MHz**; every frequency-bearing value below (0x15, 0x16, brand string, and
MSR_PLATFORM_INFO's ratio in the msr-boot-baseline fragment) derives from these two
constants — guest calibration cross-checks then agree by construction
(arch/x86/kernel/tsc.c:663–735 `native_calibrate_tsc`).

**Rule for unlisted leaves (normative): every (leaf, subleaf) not matched by a row below
reads as all-zeroes (EAX=EBX=ECX=EDX=0).** Mechanism, in two parts: (1) every *in-range*
unlisted leaf/subleaf is populated as an explicit all-zero entry in the `KVM_SET_CPUID2`
table; (2) the max basic leaf is deliberately set to 0x20 with leaf 0x20 all-zero, so the
architectural out-of-range fallback — Intel returns the highest **basic** leaf's values for any
query above the range, and KVM implements exactly this redirect for Intel-vendor guests
(linux-6.18.35 arch/x86/kvm/cpuid.c `get_out_of_range_cpuid_entry`) — makes *every*
out-of-range query (basic > 0x20, extended > 0x8000_0008, the entire hypervisor class
0x4000_0000–0x4FFF_FFFF, Centaur 0xC000_0000+) return zeroes by construction.

**Extended out-of-range redirects to the max *basic* leaf, not the max extended leaf
(rebutting a review finding).** A natural worry — raised in the GPT-5.5 cross-model pass —
is that `CPUID(0x8000_0009)` would fall through to leaf `0x8000_0008` and leak its
address-size data (`EAX=0x0000_3027`). It does not. `get_out_of_range_cpuid_entry()` for an
extended function computes `class = entry(function & 0x8000_0000)` = the `0x8000_0000` leaf
(whose `EAX = 0x8000_0008` = our max extended leaf); the check `if (class && function <=
class->eax) return NULL;` is **false** for `0x8000_0009` (it is genuinely beyond the max),
so the function proceeds to `*fn_ptr = basic->eax` — i.e. it redirects to the **max basic
leaf (0x20)**, which is all-zero — and returns that entry. So `CPUID(0x8000_0009)`,
`CPUID(0x8000_001D)`, `CPUID(0xC000_0000)` and every other extended/hypervisor/Centaur
out-of-range query read **all-zeroes**, never `0x8000_0008`'s data. (Verified against
linux-6.18 `arch/x86/kvm/cpuid.c`: the redirect target is `basic->eax`, not the extended
class leaf.) The behaviour is enforced by the running KVM, so the relevant authority is
KVM's implementation, which the frozen `max-basic-leaf = 0x20` makes return zeroes.

Column grammar (strict; vmm-core parses Value/Mask rule cells, the rest is prose):
`Leaf` is a hex leaf or inclusive range; `Subleaf` is a number, range, or `*` (all);
`Register` ∈ {EAX, EBX, ECX, EDX, ALL} — ALL rows carry all four values in one cell.
Value cells use exactly three forms: `const(0x…)` (frozen 32-bit constant),
`const-ascii("…")` (frozen byte string), or `dyn(…)` — a pure function of *guest*
architectural state only, never host state. There are exactly three `dyn` cells in the
model (leaf 1 ECX bit 27, leaf 0xB/0x1F invalid-level ECX echo, leaf 0xD.0 EBX); KVM
re-derives all three from guest state alone (arch/x86/kvm/cpuid.c:286
`kvm_update_cpuid_runtime`, :294 OSXSAVE, :314 `xstate_required_size`), so they are
deterministic and replay-stable. **These three are the only cells the §6 canonical form
serializes as a rule token** (`dyn:osxsave:<base>`, `dyn:level-echo:<type>`,
`dyn:xcr0-xsavesize`) rather than a resolved value — the hash binds the rule, which is
well-defined and replay-stable, closing the "dynamic cells make the hash impossible"
gap. Every other cell resolves to a concrete constant.

| Leaf | Subleaf | Register | Value/Mask rule | Rationale | Citation |
|---|---|---|---|---|---|
| 0x0 | 0 | ALL | EAX=const(0x00000020); EBX=const(0x756E6547); EDX=const(0x49656E69); ECX=const(0x6C65746E) | Max basic leaf 0x20 chosen so the all-zero leaf 0x20 is the architectural out-of-range fallback target (see unlisted-leaf rule); vendor string "GenuineIntel" — the guest kernel's Intel paths (leaf-0x15 TSC calibration, intel_family model checks) require it | SDM Vol.2 CPUID; linux-6.18.35 arch/x86/kvm/cpuid.c:1986; arch/x86/kernel/tsc.c:668 |
| 0x1 | 0 | EAX | const(0x000906ec) | Frozen family/model/stepping = 06_9EH stepping 0xC (Coffee Lake-S, Intel Core i9-9900K) — **derived from the box** (`cpuid-raw.txt` leaf 1 EAX = `0x000906ec`); this is the value the §1.1 host-assert `family-model-stepping` requires the live box to present. The synthetic 25 MHz crystal is **explicitly enumerated** in leaf 0x15 ECX, which `native_calibrate_tsc` consumes directly when non-zero — so the model number does not pull in a model-keyed default crystal (a real 9900K reports a 24 MHz client crystal via the same leaf; the contract overrides it to the synthetic 25 MHz, consistent with the 2.0 GHz TSC). | SDM Vol.2 CPUID leaf 01H; `docs/fragments/cfl-baseline/cpuid-raw.txt`; arch/x86/kernel/tsc.c:663–735 (CPUID.15H crystal used when ECX≠0) |
| 0x1 | 0 | EBX | const(0x00010800) | Brand index 0; CLFLUSH line 8 qwords = 64 B (consistent with leaf 4 / 0x80000006); max addressable logical CPUs = 1; **initial APIC ID = 0, constant** — the one CPUID output that varies with the running core on real hardware (rr paper) is frozen by the one-vCPU/pinned-core design | rr paper arXiv:1610.02144; antithesis.com/blog/deterministic_hypervisor (one core per VM); SDM Vol.2 CPUID 01H |
| 0x1 | 0 | ECX | dyn(const(0x76DA3203) OR (CR4.OSXSAVE << 27)) | Set (=1): SSE3, PCLMULQDQ, SSSE3, FMA, CMPXCHG16B, PCID, SSE4.1, SSE4.2, MOVBE, POPCNT, AESNI, **XSAVE[26]** (required by INTEGRATION §4 FPU/XSAVE snapshot; XCR0 policy in 0xD rows), AVX, F16C, **RDRAND[30]** = exposed-but-trapped: VMX RDRAND-exiting answers from the seeded PRNG stream — hiding the bit would not stop a kernel-mode guest from executing it, and PLAN.md's trap table routes it to the seeded stream, so exposure is free and keeps userspace RNG paths on the deterministic instruction path (rr can only mask bits because ptrace cannot trap RDRAND; a backend with RDRAND-exiting can — note that exit is **not** surfaced by stock KVM, see §1/[question]-Backend). Clear (=0): **MONITOR[3]** (MWAIT idle/C-state timing channel; hidden AND backstopped by MWAIT/MONITOR-exiting → #UD; matches frozen IA32_MISC_ENABLE=0x1801 bit 18=0, which on silicon forces this bit to read 0), DTES64/DS-CPL (no Debug Store), **VMX[5]** (locked by msr-boot-baseline: VMX MSRs 0x480–0x491 deny-gp), SMX, EIST/TM2 (power states denied), SDBG, xTPR, **PDCM[15]** (locked by msr-pmu: IA32_PERF_CAPABILITIES deny-gp), DCA, **x2APIC[21]=0** — the LAPIC is a userspace xAPIC-MMIO device only: the pinned guest kernel cannot even build x2APIC support (Kconfig dependency "IRQ_REMAP or HYPERVISOR_GUEST" unmet under tinyconfig with PV support off), and hiding x2APIC keeps every APIC access on the single trapped MMIO path (x2APIC MSR exits would bypass the MSR filter when an in-kernel LAPIC exists; see [question] 1), **HYPERVISOR[31]=0** — the guest believes it is bare metal; this is the gate bit guests probe before reading 0x4000_00xx (rr gathers hypervisor leaves only when it is set), closing §7's kvmclock vector at its root. **TSC-Deadline[24]=0** (hidden; LAPIC timer is xAPIC LVT one-shot/periodic via MMIO TMICT 0x380, emulated against V-time in §5; the in-kernel `MSR_IA32_TSC_DEADLINE` WRMSR fastpath silently swallows the write under `KVM_IRQCHIP_NONE` — `vmx.c` `handle_fastpath_wrmsr` runs before the MSR filter and `kvm_set_lapic_tscdeadline_msr` no-ops with no in-kernel apic — so deadline mode would be *unbacked* and Linux's TSC-deadline clockevent would arm-but-never-fire; aligns with Ruling R1, PR #21). **OSXSAVE[27] is dyn**: mirrors guest CR4.OSXSAVE — a pure function of guest state | PLAN.md trap table (RDRAND/RDSEED → seeded stream); linux-6.18.35 arch/x86/kvm/vmx/vmx.c (handle_fastpath_wrmsr before MSR filter), arch/x86/kvm/lapic.c (kvm_set_lapic_tscdeadline_msr no-op w/o in-kernel apic); arch/x86/Kconfig:462 (X86_X2APIC deps); arch/x86/kvm/cpuid.c:294 (OSXSAVE runtime); §5 (xAPIC LVT timer); INTEGRATION.md §7; fragments msr-tsc/msr-pmu/msr-boot-baseline |
| 0x1 | 0 | EDX | const(0x0F8BBB7F) | Set (=1): FPU, VME, DE, PSE, **TSC[4]** = exposed-but-trapped: RDTSC/RDTSCP VM-exit via VMX TSC-exiting and are answered from V-time only — the bit must be 1 (the pinned kernel requires TSC; hiding it is not an option, trapping is the mechanism), MSR, PAE, CX8, **APIC[9]** (boot-critical: LAPIC present, emulated in userspace against TimerQueue), SEP (SYSENTER MSRs are allow-stateful per contract §3), MTRR (msr-arch-stateful vMTRR rows assume this bit), PGE, CMOV, PAT (IA32_PAT allow-stateful), PSE-36, CLFSH, MMX, FXSR, SSE, SSE2 (FPU/PSE/PGE/MSR/PAE/CX8/CMOV/FXSR/SSE/SSE2 are the pinned kernel's hard boot requirements; SSE2+LM are also checked by verify_cpu.S), SS. Clear (=0): MCE[7]/MCA[14] (host machine-check events are asynchronous real-world nondeterminism; the tinyconfig guest builds without X86_MCE, and hiding removes the IA32_MCG_*/MCi_* MSR surface entirely), **PSN[18]** (serial number — host identity), **DS[21]** (Debug Store — BTS/PEBS denied per msr-pmu; matches MISC_ENABLE 0x1801 bits 11/12), ACPI[22] (thermal-monitor MSR surface denied), HTT[28]=0 (one logical processor), TM, PBE | linux-6.18.35 arch/x86/Kconfig.cpufeatures:36–101 (required features for X86_64 !SMP); arch/x86/kernel/verify_cpu.S; PLAN.md trap table (RDTSC → f(V-time)); INTEGRATION.md §7 (TSC plumbing); fragments msr-arch-stateful/msr-pmu |
| 0x2 | 0 | ALL | EAX=const(0x0000FF01); EBX=ECX=EDX=const(0x00000000) | Legacy cache-descriptor leaf: AL=01 (run once), descriptor 0xFF = "no cache info here, use leaf 4" — pushes all cache identity to the single, explicit leaf-4 encoding instead of duplicating frozen constants | SDM Vol.2 CPUID leaf 02H (descriptor 0xFF) |
| 0x4 | 0 | ALL | EAX=const(0x00000121); EBX=const(0x01C0003F); ECX=const(0x0000003F); EDX=const(0x00000000) | L1D: 32 KiB, 8-way, 64-B line, 64 sets, level 1, self-initializing; sharing fields (EAX[25:14], EAX[31:26]) = 0 → one core, one thread — frozen topology | SDM Vol.2 CPUID leaf 04H |
| 0x4 | 1 | ALL | EAX=const(0x00000122); EBX=const(0x01C0003F); ECX=const(0x0000003F); EDX=const(0x00000000) | L1I: 32 KiB, 8-way, 64-B line — frozen constant cache identity | SDM Vol.2 CPUID leaf 04H |
| 0x4 | 2 | ALL | EAX=const(0x00000143); EBX=const(0x00C0003F); ECX=const(0x000003FF); EDX=const(0x00000000) | L2 unified: **256 KiB, 4-way**, 64-B line, 1024 sets — **derived from the box** (`cpuid-raw.txt` leaf 4 sub2 EBX=`0x00c0003f` → 4 ways; Coffee Lake-S client L2 = 256 KiB/4-way, vs Skylake-SP's 1 MiB/16-way); topology sharing fields (EAX[31:14]) cleared for the single-core synthetic model | SDM Vol.2 CPUID leaf 04H; `docs/fragments/cfl-baseline/cpuid-raw.txt` |
| 0x4 | 3 | ALL | EAX=const(0x00000163); EBX=const(0x03C0003F); ECX=const(0x00003FFF); EDX=const(0x00000000) | L3 unified: **16 MiB, 16-way**, 64-B line, 16384 sets — **derived from the box** (`cpuid-raw.txt` leaf 4 sub3 EBX=`0x03c0003f`/ECX=`0x00003fff`; Coffee Lake-S shared inclusive L3 = 16 MiB/16-way, vs Skylake-SP's 1.375 MiB/11-way per-core slice); EAX topology sharing fields (bits 31:14) cleared for the single-core synthetic model. EDX=0 keeps the SKX modeling decision (no inclusivity/complex-indexing claims — the box reports EDX=`0x6` for its 8-core shared L3, but complex cache indexing is meaningless for the single-core synthetic package, so it is not advertised) | SDM Vol.2 CPUID leaf 04H; `docs/fragments/cfl-baseline/cpuid-raw.txt` |
| 0x4 | 4+ | ALL | const(0x00000000) ×4 | EAX[4:0]=0 terminates cache enumeration | SDM Vol.2 CPUID leaf 04H |
| 0x5 | * | ALL | const(0x00000000) ×4 | MONITOR/MWAIT leaf zeroed: feature hidden (1.0:ECX[3]=0) — MWAIT C-state hints and MONITOR address-watch are real-time/power timing channels; instruction backstop = MWAIT/MONITOR-exiting → #UD | SDM Vol.2 MONITOR/MWAIT + CPUID leaf 05H; RESEARCH.md principle 5; PLAN.md trap table |
| 0x6 | * | ALL | const(0x00000000) ×4 | Thermal/power leaf zeroed wholesale — closes §7 power/frequency: EAX[0] DTS, EAX[1] turbo, **EAX[7] HWP** (+all HWP sub-bits; IA32_HWP_* 0x770–0x777 deny-gp), **EAX[19] HFI / EAX[23] ITD** (IA32_HW_FEEDBACK_* publish real thermal/scheduling state), **ECX[0] MPERF/APERF** (effective-frequency ratio = elapsed-real-time oracle; IA32_MPERF/APERF deny-gp), ECX[3] energy-perf-bias — every bit gates an MSR surface that imports host real time, so the leaf is all-zero and the MSR fragments deny the lot | SDM Vol.3B ch.15 + §14.9; docs.kernel.org/arch/x86/intel-hfi; INTEGRATION.md §7 (power/frequency); guest config-fragment (CONFIG_CPU_FREQ off) |
| 0x7 | 0 | EAX | const(0x00000000) | Max leaf-7 subleaf = 0: subleaf 1+ (HRESET, AVX512-FP16, LAM, …) architecturally out of range — nothing half-exposed | SDM Vol.2 CPUID leaf 07H |
| 0x7 | 0 | EBX | const(0x009C27AB) | **Re-derived from the box** (`cpuid-raw.txt` leaf 7.0 EBX=`0x029c67af`; the contract exposes the box's architectural bits and clears the deny-list). Set (=1): FSGSBASE, **TSC_ADJUST[1]** (IA32_TSC_ADJUST is emulate-vtime per msr-tsc fragment — exposed because the emulation is exact, and hiding it would make the guest's TSC-coherence logic diverge), BMI1, AVX2, SMEP, BMI2, ERMS, INVPCID, ZERO_FCS_FDS[13] (deprecated x87 CS/DS always read 0 — strictly more deterministic), **RDSEED[18]** = exposed-but-trapped via VMX RDSEED-exiting → seeded PRNG stream (same justification as RDRAND; rr must clear it, we trap it; note RDSEED *executes* even when its CPUID bit is masked, so the exiting control is the real enforcement either way), ADX, SMAP, CLFLUSHOPT. Clear (=0): **FDP_EXCPTN_ONLY[6]=0 and CLWB[24]=0 — both physically absent on Coffee Lake-S** (box EBX bits 6,24 clear; both were *set* on the Skylake-SP baseline — a CFL-client-vs-SKX-server ripple, CLWB being a server/Ice Lake+ feature), so the contract cannot expose them; SGX[2] (present on the box but hidden — leaf 0x12 zeroed), **HLE[4]/RTM[11]=0 — TSX physically absent on the box** (RTM/HLE not implemented; IA32_TSX_CTRL 0x122 #GPs; XBEGIN/&c #UD natively — §3.9 / §4 TSX rows), **RDT-M[12]/RDT-A[15]** (cache-occupancy/bandwidth counters = cross-VM uarch oracle; leaves 0xF/0x10 zeroed), MPX[14] (present on the box but hidden — msr-arch-stateful: BNDCFGS deny-gp), AVX512F[16]/DQ[17]/IFMA[21]/PF..VL[26-31]=0 (no AVX-512 on CFL client: keeps frozen XCR0 ≤ 0x7, the XSAVE image small and canonical), **Intel PT[25]=0** (present on the box but locked by msr-intel-pt: RTIT MSRs deny-gp, leaf 0x14 zeroed), SHA[29]=0 (physically absent on the box) | PLAN.md trap table; `docs/fragments/cfl-baseline/cpuid-raw.txt`; rr src/RecordSession.cc (CPUID_RDSEED_FLAG, CPUID_RTM_FLAG); fragments msr-tsc/msr-speculation/msr-intel-pt/msr-arch-stateful; SDM Vol.2 CPUID 07H |
| 0x7 | 0 | ECX | const(0x00000000) | All clear, notably: **WAITPKG[5]=0** (locked by msr-timing-instr: UMWAIT/TPAUSE wait on real-TSC deadlines and leak timeout-vs-wake in CF; IA32_UMWAIT_CONTROL deny-gp), CET_SS[7]=0 (msr-arch-stateful), PKU[3]/OSPKE[4]=0 (keeps XCR0 menu at {x87,SSE,AVX}), UMIP[2]=0, **RDPID[22]=0** — user-mode read of IA32_TSC_AUX would expose a CPU/node id channel without CPUID; the bit is hidden, and because TSC_AUX is allow-stateful (vm_state-echoed, never the host's per-core value, per msr-tsc) even a guest that executes RDPID anyway reads only deterministic guest state (see [question] 4: RDPID cannot be made to #UD while RDTSCP is enabled), LA57[16]=0 (4-level paging only, matches 48 virtual bits in 0x80000008) | felixcloutier.com/x86/rdpid; fragments msr-timing-instr/msr-tsc/msr-arch-stateful; SDM Vol.2 CPUID 07H |
| 0x7 | 0 | EDX | const(0x20000000) | Only **ARCH_CAPABILITIES[29]=1** is set — it enumerates the read-only `IA32_ARCH_CAPABILITIES` MSR (0x10a), which is `allow-fixed` in §3.9 and is *not* a control surface, so exposing it leaks nothing and is required for CPUID↔MSR consistency (gate 5: a fixed 0x10a row with bit 29 clear would be a half-exposed MSR). The frozen 0x10a value's `*_NO` bits are exactly what keep guest mitigation code from ever reaching for the denied *control* MSRs. All other bits clear, notably: **SERIALIZE[14]=0** — enumerated deliberately (the baseline µarch predates it; any future exposure is a version bump, not an accident; SERIALIZE has no architectural output, so a guest that executes it anyway leaks only host *support*, addressed by the §4 host-homogeneity assumption), **PCONFIG[18]=0** (MKTME platform-key state is unvirtualizable-deterministically — deny; leaf 0x1B zeroed), **ARCH_LBR[19]=0** (locked by msr-debug-lbr: branch history + cycle timing; leaf 0x1C zeroed), CET_IBT[20]=0, AVX512-4VNNIW/4FMAPS/VP2INTERSECT=0, MD_CLEAR[10]=0, **IBRS/IBPB[26], STIBP[27], L1D_FLUSH[28], SSBD[31] = 0** — locked by msr-speculation: every speculation-*control* feature is hidden and its control MSRs (SPEC_CTRL, PRED_CMD, FLUSH_CMD) deny-gp; the single-tenant pinned-core deployment is the mitigation, not guest-driven controls | Intel ISE ref 319433 (SERIALIZE, PCONFIG, HRESET); fragments msr-debug-lbr/msr-speculation; §3.9 (MSR_IA32_ARCH_CAPABILITIES 0x10a allow-fixed); SDM Vol.2 CPUID 07H |
| 0x7 | 1+ | ALL | const(0x00000000) ×4 | Out of range via 0x7.0:EAX=0 and explicitly zero: **HRESET (7.1:EAX[22])=0** — resets µarch predictor/Thread-Director history via IA32_HRESET_ENABLE (0x17DA), µarch-state-affecting, denied; likewise AVX-VNNI, AVX512-FP16, LAM, FRED — none enumerated | Intel ISE ref 319433/843860 (HRESET); qemu-devel HRESET RFC |
| 0x3, 0x8–0x9, 0xC, 0xE, 0x11, 0x13, 0x17–0x20 | * | ALL | const(0x00000000) ×4 | Explicit zero block: PSN leaf 0x3 (serial number — host identity), reserved 0x8/0xC/0xE/0x11/0x13, SoC-vendor 0x17, TLB 0x18 (host TLB identity), KeyLocker 0x19, hybrid 0x1A (no Thread Director), PCONFIG-enum 0x1B, **Arch-LBR enum 0x1C** (gate-5 pair of 7.0:EDX[19]=0), AMX 0x1D/0x1E (no tile state in XCR0), topology-v2 0x1F (leaf 0xB is the single topology source), HRESET-enum 0x20 (gate-5 pair of 7.1:EAX[22]=0; also the all-zero out-of-range fallback target) | SDM Vol.2 CPUID; fragments msr-debug-lbr (leaf 0x1C), msr-arch-stateful (AMX/XFD) |
| 0xA | 0 | ALL | const(0x00000000) ×4 | **Architectural PerfMon version = 0: no vPMU exists.** No counters, no fixed counters, no PEBS/DS — the host owns the PMU as the V-time instrument (retired-branch counting + PMI injection); the guest kernel consequently never sets CR4.PCE and RDPMC is enforced by VMX RDPMC-exiting → #GP (instruction table); the entire msr-pmu class is deny-gp, architecturally consistent with version 0 | SDM Vol.3B ch.20; INTEGRATION.md §7 (PMU); PLAN.md trap table (RDPMC → trap); fragment msr-pmu |
| 0xB | 0 | ALL | EAX=const(0x00000000); EBX=const(0x00000001); ECX=const(0x00000100); EDX=const(0x00000000) | Extended topology, SMT level: shift 0, 1 logical processor, level type 1; **EDX (x2APIC ID) = const 0** — on real hardware this returns the *running core's* ID, the one nondeterministic CPUID output even on a fixed host (rr pins to one core for exactly this reason); the single-vCPU frozen topology makes it a constant. The leaf stays populated (despite 1.0:ECX[21]=0) so Linux's `detect_extended_topology` sees one clean thread/core/package; x2APIC *IDs* are architecturally enumerable without x2APIC *mode* | rr paper arXiv:1610.02144; antithesis.com/blog/deterministic_hypervisor; SDM Vol.3A x2APIC ID enumeration |
| 0xB | 1 | ALL | EAX=const(0x00000000); EBX=const(0x00000001); ECX=const(0x00000201); EDX=const(0x00000000) | Core level: shift 0, 1 processor, level type 2 — one core per package, frozen | SDM Vol.2 CPUID leaf 0BH |
| 0xB | 2+ | ALL | EAX=EBX=EDX=const(0x00000000); ECX=dyn(input subleaf in ECX[7:0], level type 0) | Architectural terminator for past-the-end subleaves; the ECX echo is a pure function of the guest's own input — deterministic | SDM Vol.2 CPUID leaf 0BH |
| 0xD | 0 | ALL | EAX=const(0x00000007); EBX=dyn(0x240 if XCR0∈{0x1,0x3}; 0x340 if XCR0=0x7); ECX=const(0x00000340); EDX=const(0x00000000) | **XCR0/XSETBV policy**: supported XCR0 = 0x7 (x87+SSE+AVX) and nothing else — XSETBV VM-exits unconditionally and the VMM permits exactly XCR0 ∈ {0x1, 0x3, 0x7} (architectural constraints: bit 0 set, AVX requires SSE), injecting #GP otherwise; no supervisor states (IA32_XSS surface deny-gp per msr-arch-stateful). Max save area (ECX) = 0x340 = 832 B standard format — small, fixed, fully specified, satisfying INTEGRATION §4's FPU/XSAVE vm_state capture with a canonical image. EBX is dyn on guest XCR0 only (KVM recomputes it from `vcpu->arch.xcr0`) | SDM Vol.1 ch.13; SDM Vol.3C §25.1.2 (XSETBV exits); linux-6.18.35 arch/x86/kvm/cpuid.c:314; INTEGRATION.md §4 |
| 0xD | 1 | ALL | const(0x00000000) ×4 | **EAX[0] XSAVEOPT = 0** — rr disables it unconditionally: the modified/init optimizations make the saved image depend on hidden µarch tracking state (context-switch history), i.e. the memory image itself becomes nondeterministic; **EAX[1] XSAVEC = 0, EAX[3] XSAVES = 0** — compacted format + init optimization leave unspecified gap bytes in the save area (snapshot-hash poison); **EAX[2] XGETBV1 = 0** — clearing the bit stops the *pinned kernel* from issuing XGETBV(ECX=1) (it gates on this CPUID bit), but it does **not** make the instruction #UD/#GP on Coffee Lake-S, which physically implements XGETBV1 (box CPUID.0xD.1:EAX[2]=1): native `xgetbv` with ECX=1 still returns XINUSE (live µarch init-tracking state). XGETBV(ECX=1) is therefore class (e) `scope` in §1.2/§4 (uninterceptable hidden-µarch op, in-scope only for the cooperative CPUID-respecting guest, which never emits it) — **not** a false "#GP because the bit is clear" claim and **not** defended by an opcode scan (it shares opcode `0f 01 d0` with XGETBV0); **EAX[4] XFD = 0** (msr-arch-stateful: IA32_XFD deny-gp). EBX=0 (no XSAVES area), ECX=EDX=0 (no IA32_XSS bits — gate-5 pair of the MSR_IA32_XSS deny-gp row). Plain XSAVE/XRSTOR/FXSAVE remain: their output is a pure function of architectural state (class (a) `arch`). **These bits being clear stops the cooperative kernel from emitting the variants, but does not #UD them on Coffee Lake-S (which has them physically — box CPUID.0xD.1:EAX[0..3]=0xf) — so XSAVEOPT/XSAVEC/XSAVES are class (e) `scope` per §1.2 (cooperative-guest scope: the CPUID-respecting kernel never emits them; no opcode scan is claimed), not enforced by the hidden bit.** | rr src/RecordSession.cc (CPUID_XSAVEOPT_FLAG + comment); SDM Vol.1 §13.4.3, §13.7–13.11; felixcloutier.com/x86/xgetbv; §1.2 (cooperative-guest threat model); fragment msr-arch-stateful |
| 0xD | 2 | ALL | EAX=const(0x00000100); EBX=const(0x00000240); ECX=const(0x00000000); EDX=const(0x00000000) | AVX (YMM_Hi128) component: 256 B at fixed offset 0x240 — standard (non-compacted) layout is fully determined by these constants, so the vm_state XSAVE image has one defined byte layout | SDM Vol.1 §13.4.2–13.4.3 |
| 0xD | 3–0x3F | ALL | const(0x00000000) ×4 | No further XSAVE components (no MPX, AVX-512, PKRU, CET, AMX states — matching every cleared feature bit above); rr records all 64 subleaves to make traces portable — we freeze all 64 for the same reason: the XSAVE layout is part of the hashed model | rr src/util.cc gather_cpuid_records (CPUID_GETXSAVE, 64 subleaves) |
| 0xF, 0x10 | * | ALL | const(0x00000000) ×4 | RDT monitoring/allocation zeroed (gate-5 pair of 7.0:EBX[12]/[15]=0): occupancy and memory-bandwidth counters measure real cache/memory behavior — a host-activity oracle | SDM Vol.3B ch.17.18–17.19 |
| 0x12 | * | ALL | const(0x00000000) ×4 | SGX zeroed (gate-5 pair of 7.0:EBX[2]=0 and FEAT_CTL SGX bits clear in msr-boot-baseline): enclave state is unsnapshottable and EPC is host-physical identity | fragment msr-boot-baseline (MSR_IA32_FEAT_CTL); SDM Vol.3D |
| 0x14 | * | ALL | const(0x00000000) ×4 | **Intel PT leaf zeroed** (gate-5 pair of 7.0:EBX[25]=0 and the msr-intel-pt deny-gp class): PT emits TSC/MTC/CYC wall-clock packets and a full control-flow trace into guest-visible memory — a real-time and µarch oracle; rr enumerates this leaf, we deny the facility entirely | SDM Vol.3C ch.33; fragment msr-intel-pt; rr src/util.cc |
| 0x15 | 0 | ALL | EAX=const(0x00000001); EBX=const(0x00000050); ECX=const(0x017D7840); EDX=const(0x00000000) | **TSC/crystal ratio: TSC = 25 MHz × 80/1 = 2.0 GHz, crystal reported explicitly (ECX = 25,000,000)** — with kvmclock hidden this is the guest's primary calibration source: `native_calibrate_tsc` takes this path, sets TSC_KNOWN_FREQ (no PIT recalibration drift), and — critically — presets `lapic_timer_period` = crystal/HZ directly from ECX, **skipping the PIT-vs-LAPIC measurement loop entirely**, so the userspace LAPIC timer must tick at exactly 25 MHz of V-time and the emulated PIT at 1.193182 MHz of V-time for all three sources to stay mutually consistent | linux-6.18.35 arch/x86/kernel/tsc.c:663–735 (:731 lapic_timer_period), arch/x86/kernel/apic/apic.c:791–809 (calibration skip); SDM Vol.2 CPUID 15H; INTEGRATION.md §7 (timer devices) |
| 0x16 | 0 | ALL | EAX=const(0x000007D0); EBX=const(0x000007D0); ECX=const(0x00000064); EDX=const(0x00000000) | Base = max = 2000 MHz (no turbo — leaf 6 EAX[1]=0, no turbo MSRs), bus = 100 MHz; consistent with 0x15 (2.0 GHz), with the brand string, and with MSR_PLATFORM_INFO ratio 20 (= 2.0 GHz / 100 MHz — this row answers the msr-boot-baseline [question]) | SDM Vol.2 CPUID 16H; fragment msr-boot-baseline (MSR_PLATFORM_INFO) |
| 0x40000000–0x4FFFFFFF | * | ALL | const(0x00000000) ×4 | **Entire hypervisor class hidden**: no "KVMKVMKVM" signature, no KVM_CPUID_FEATURES (no kvmclock bits 0/3, no steal-time, no PV-EOI, no KVM_HINTS_REALTIME), no Hyper-V leaves — closes §7's kvmclock vector at enumeration level; the msr-kvmclock/msr-kvm-exposed/msr-tsc fragments deny every PV MSR behind it, and the guest kernel is built without HYPERVISOR_GUEST as defense in depth. Enforced both by 1.0:ECX[31]=0 (guests don't probe) and by the out-of-range fallback to all-zero leaf 0x20 (probes that ignore the bit still read zeroes) | kernel.org Documentation/virt/kvm/x86/cpuid.rst + msr.rst; INTEGRATION.md §7 (KVM paravirtual clock); guest config-fragment; fragments msr-kvmclock/msr-kvm-exposed |
| 0x80000000 | 0 | ALL | EAX=const(0x80000008); EBX=ECX=EDX=const(0x00000000) | Max extended leaf 0x80000008; vendor fields zero (Intel convention); rr's AMD-specific leaves 0x8000001D/0x80000020 are out of range → zeroes by the fallback rule | SDM Vol.2 CPUID 80000000H |
| 0x80000001 | 0 | ALL | EAX=EBX=const(0x00000000); ECX=const(0x00000121); EDX=const(0x2C100800) | ECX: LAHF/SAHF-64, LZCNT, PREFETCHW = 1; everything else 0. EDX: **SYSCALL[11]=1** (boot-critical; STAR/LSTAR/CSTAR/FMASK are allow-stateful), **NX[20]=1** (boot-critical; EFER.NXE allow-stateful), PDPE1GB[26]=1, **RDTSCP[27]=1** = exposed-but-trapped: VMX "enable RDTSCP" is on and TSC-exiting makes RDTSCP exit like RDTSC — answered as (f(V-time), vm_state TSC_AUX); the AUX half never reflects a host core id because MSR_TSC_AUX is allow-stateful (msr-tsc), **LM[29]=1** (boot-critical, verify_cpu.S long-mode check) | linux-6.18.35 arch/x86/kernel/verify_cpu.S:110–126; PLAN.md trap table (RDTSC/RDTSCP); fragment msr-tsc (MSR_TSC_AUX allow-stateful); SDM Vol.2 CPUID 80000001H |
| 0x80000002–0x80000004 | 0 | ALL | const-ascii("Deterministic vCPU (CFL-class) @ 2.00GHz") NUL-padded to 48 bytes, packed little-endian into EAX..EDX across the three leaves | Frozen brand string; deliberately synthetic (no shipping-part impersonation — the box's real brand is "Intel(R) Core(TM) i9-9900K CPU @ 3.60GHz"). The "(CFL-class)" descriptor tracks the det-cfl-v1 re-baseline (was "(SKX-class)"); the synthetic "@ 2.00GHz" token is kept consistent with the frozen 0x15/0x16 (2.0 GHz) because assorted software derives frequency from it; constant bytes are part of the hashed model | SDM Vol.2 CPUID 80000002H–80000004H |
| 0x80000005 | 0 | ALL | const(0x00000000) ×4 | Reserved on Intel — explicit zeroes | SDM Vol.2 CPUID 80000005H |
| 0x80000006 | 0 | ALL | EAX=EBX=EDX=const(0x00000000); ECX=const(0x01004040) | L2: 256 KiB (size 0x100), 4-way (assoc code 0x4), 64-B line — **mirrors the authoritative leaf-4 sub2 L2** (256 KiB/4-way), preserving the SKX design decision that 0x80000006 describes the same L2 as leaf 4. **Note:** the box's legacy 0x80000006 ECX is `0x01006040` (assoc code 0x6 = "8-to-15-way"), which disagrees with its own leaf-4 (4-way); the contract follows leaf 4, the SDM-authoritative cache leaf. (See IMPLEMENTATION.md `[question]`.) | SDM Vol.2 CPUID 80000006H; `docs/fragments/cfl-baseline/cpuid-raw.txt` (leaf 4 sub2 + 0x80000006) |
| 0x80000007 | 0 | ALL | EAX=EBX=ECX=const(0x00000000); EDX=const(0x00000100) | **Invariant TSC = 1 (the only set bit)**: the contract *can* honestly promise a constant-rate TSC regardless of host because the virtual TSC is 2 × V-ns by construction; the guest then trusts the TSC as clocksource (with tsc=reliable cmdline) and never invokes recalibration/watchdog paths that would compare clocks. RAS/power bits (TM, PLN, PTM) all 0 | SDM Vol.3B §18.17 (invariant TSC); INTEGRATION.md §7 (TSC plumbing); PLAN.md (tsc=reliable) |
| 0x80000008 | 0 | ALL | EAX=const(0x00003027); EBX=ECX=EDX=const(0x00000000) | **39 physical / 48 virtual** address bits — **derived from the box** (`cpuid-raw.txt` leaf 0x80000008 EAX=`0x00003027`: phys=0x27=39, virt=0x30=48; Coffee Lake-S client reports 39 physical bits, vs Skylake-SP server's 46). 4-level paging, LA57=0; frozen guest MAXPHYADDR must be ≤ host MAXPHYADDR — asserted at VM start (§1.1 `maxphyaddr-min 39`, see [question] 2); EBX=0: no WBNOINVD or AMD extensions | SDM Vol.2 CPUID 80000008H; `docs/fragments/cfl-baseline/cpuid-raw.txt` |
| (any unlisted) | * | ALL | const(0x00000000) ×4 | Normative catch-all restated as a row so the table is closed: in-range gaps are explicit zero entries; out-of-range queries hit the architectural redirect to all-zero leaf 0x20 (Intel-vendor fallback, implemented verbatim by KVM) | SDM Vol.2 CPUID (out-of-range behavior); linux-6.18.35 arch/x86/kvm/cpuid.c:1986 |

### FPU/XSAVE save-image determinism — pinned `MXCSR_MASK` (normative)

CPUID freezes the XSAVE *layout* (leaf 0xD rows: XCR0 menu {0x1,0x3,0x7}, 832-byte
standard image, no compacted/optimized variants), but one byte-range of the legacy save
area is a host CPU constant the layout rules do not cover: **`MXCSR_MASK` at offset 28** of
the FXSAVE/XSAVE legacy region. `FXSAVE`/`XSAVE` write it from a hardware value that varies
by host part (Linux reads it once into `mxcsr_feature_mask`, `arch/x86/kernel/fpu/xstate.c`),
so two hosts with different masks would make a guest's saved FPU image — and therefore the
determinism gate's `state_hash` over guest memory — diverge. The instruction cannot be
intercepted (FXSAVE/XSAVE do not VM-exit), so the value is **pinned and asserted**, exactly
as MAXPHYADDR is ([question] 2): the frozen baseline's `MXCSR_MASK` is
**`0x0000FFFF`** (DAZ supported — **box-confirmed** via FXSAVE offset 28, `docs/fragments/cfl-baseline/mxcsr.txt`; the same value carries over from the SKX baseline), it is part of the hashed model
(`mxcsr-mask` in the canonical form, §6), and **vmm-core asserts `host MXCSR_MASK ==
0x0000FFFF` at VM start and refuses to run otherwise.** This is the same host-homogeneity
assumption the contract already relies on for non-interceptable instruction-presence leaks
(§4); it is recorded here so the FPU image is a fully specified part of the frozen surface
rather than a silent host constant. See [question] MXCSR below.

### Coverage map (gate-1 walk of the required leak-vector/boot-critical list)

- RDTSC 1.0:EDX[4] → leaf 1 EDX row (exposed-but-trapped, V-time). RDTSCP
  0x80000001:EDX[27] + IA32_TSC_AUX → 0x80000001 row + msr-tsc. RDPID 7.0:ECX[22] → 7.0
  ECX row + [question] 4.
- RDRAND 1.0:ECX[30] / RDSEED 7.0:EBX[18] → leaf 1 ECX / 7.0 EBX rows: **decision =
  exposed-but-trapped** (VMX exiting → task-01 seeded stream), justified against PLAN.md's
  trap table; hiding is rr's ptrace-era workaround, not ours.
- MONITOR/MWAIT 1.0:ECX[3] + leaf 5 → hidden + zeroed. WAITPKG 7.0:ECX[5] → hidden.
- XSAVE/XGETBV/XSETBV 1.0:ECX[26,27] + leaf 0xD (.0–.0x3F incl. XSAVEOPT/XSAVEC/XGETBV1/
  XSAVES) → leaf 1 ECX + three 0xD rows; XCR0 menu {0x1,0x3,0x7}.
- PMU leaf 0xA → version 0, no vPMU, RDPMC #GP. Intel PT 7.0:EBX[25] + leaf 0x14 → hidden
  + zeroed. ARCH_LBR 7.0:EDX[19] + leaf 0x1C → hidden + zeroed.
- PCONFIG 7.0:EDX[18], SERIALIZE 7.0:EDX[14], HRESET 7.1:EAX[22] + leaf 0x20 → 7.0 EDX /
  7.1 / zero-block rows.
- **TSC-deadline 1.0:ECX[24]=0 (hidden)** → MSR 0x6e0 `deny-gp` (§3.3; LAPIC timer is xAPIC
  LVT one-shot/periodic, §5). TSC_ADJUST 7.0:EBX[1]=1, invariant TSC 0x80000007:EDX[8]=1 →
  respective rows, dispositions in §3.3.
- Power/thermal 0x6 (MPERF/APERF ECX[0], HWP EAX[7], HFI/ITD EAX[19/23]) → leaf 6 row.
- kvmclock / hypervisor bit / KVM leaves 1.0:ECX[31] + 0x4000_0000–0x4FFF_FFFF → leaf 1
  ECX row + hypervisor-class row.
- TSC/crystal 0x15/0x16 → rows with PIT/LAPIC/PLATFORM_INFO consistency chain.
- Topology/x2APIC-ID 1.0:EBX[31:24], 0xB, 0x1F → leaf 1 EBX, 0xB rows, zero-block.
- x2APIC 1.0:ECX[21] → hidden; stance + flip condition in [question] 1.
- Boot-critical baseline (long mode, NX, SYSCALL, APIC, paging bits, cache/topology,
  XSAVE/OSXSAVE+0xD) → leaf 1 EDX, 0x80000001, leaf 2/4, 0xB, 0xD rows; checked against
  the v6.18.35 required-feature set and verify_cpu.S.
- Full-model rule (trap every leaf, never inherit host) → preamble service rule +
  unlisted-leaf rule + catch-all row.

### Open questions

[question] 1 — x2APIC hidden (1.0:ECX[21]=0): chosen because the task-04 pinned kernel
cannot build x2APIC support (arch/x86/Kconfig:462 — needs IRQ_REMAP or HYPERVISOR_GUEST,
both off) and because a userspace-emulated xAPIC MMIO page keeps every APIC access on one
trapped path (KVM's MSR filter explicitly does not intercept x2APIC MSRs when an in-kernel
LAPIC is enabled — Documentation/virt/kvm/api.rst, KVM_X86_SET_MSR_FILTER). If the x2APIC
MSR-class fragment or vmm-core instead adopts `emulate-apic` for 0x800–0x8FF with MSR-based
APIC, this bit must flip to 1 in the same contract version bump, and the 0x800–0x8FF rows
flip from deny-gp to the per-register sub-table. Until then, deny-gp on 0x800–0x8FF is
architectural (x2APIC not enumerated).

[question] 2 — MAXPHYADDR frozen at 39 (0x80000008:EAX[7:0]): the det-cfl-v1 box (i9-9900K)
reports exactly 39 physical bits (box `cpuid-raw.txt`), so the frozen value matches the host;
legal on hosts with ≥ 39 physical bits (true for the Coffee Lake-S client box). vmm-core must
assert guest-MAXPHYADDR ≤ host-MAXPHYADDR at VM creation and refuse to start otherwise;
shrinking the frozen value later is a version bump with snapshot-compatibility impact. (The
SKX baseline froze 46 — the server part's physical width; the re-baseline lowers it to the
client box's 39.)

[question] 3 — Frozen frequency constants (2.0 GHz TSC, 25 MHz crystal, 100 MHz bus): these
answer the msr-boot-baseline fragment's open question (MSR_PLATFORM_INFO ratio = 20 =
0x14 in bits 15:8). All five frequency-bearing surfaces (0x15, 0x16, brand string,
PLATFORM_INFO, and the vtime tsc ratio config = 2 ticks/vns) must be changed together or
not at all; the canonical TOML should derive them from a single `tsc_hz = 2_000_000_000`
key so the hash can't capture a half-updated set.

[question] 4 — RDPID disposition (resolved): there is no RDPID-specific VMX control, and
hiding the virtual CPUID.7.0:ECX[22] bit does **not** force #UD — the silicon honors RDPID by
its *physical* support. **The `det-cfl-v1` baseline is Coffee Lake-S, which physically lacks RDPID
(introduced in Ice Lake) — box CPUID.7.0:ECX[22]=0 — so on the homogeneous fleet RDPID `#UD`s —
class (b) `fault-absent` (§1.1/§4), and vmm-core asserts host absence (`host-assert host-absent
RDPID`).** Robustness note
for a hypothetical future RDPID-bearing baseline: it would become class (a) `arch`, because
the value path reads the guest's `IA32_TSC_AUX` that vmm-core loads into the physical MSR
during guest execution (§3.3) — deterministic, `allow-stateful`, never host-derived. The
alternative (hide RDTSCP too, clear enable-RDTSCP, #UD both) was rejected because PLAN.md's
trap table commits to RDTSC/RDTSCP → f(V-time), and it would not stop RDPID anyway (no control
gates it).

[question] MXCSR — frozen `MXCSR_MASK = 0x0000FFFF` (FPU/XSAVE save-image pin above): legal
only on a host fleet whose CPUs report this mask (**box-confirmed** for the Coffee Lake-S
baseline via FXSAVE, `docs/fragments/cfl-baseline/mxcsr.txt`; DAZ supported). vmm-core asserts `host MXCSR_MASK == 0x0000FFFF` at VM start and refuses to run
otherwise (parallel to the MAXPHYADDR assertion in [question] 2). If a deployment must span
hosts with a different mask, the frozen value is a version bump, not an in-place edit. The
non-interceptability of FXSAVE/XSAVE means this is enforced by host-homogeneity, not by a
trap — the same assumption §4 records for instruction-presence leaks (XSAVE-variants,
SERIALIZE, SHA).

## 3. MSR disposition tables

Every MSR in the reference set carries one disposition token per access direction; §1
defines the enforcement mechanism behind every token. The **reference set** is defined
exactly as the union of:

- **(a)** the static MSR arrays behind `KVM_GET_MSR_INDEX_LIST` and
  `KVM_GET_MSR_FEATURE_INDEX_LIST` in `arch/x86/kvm/x86.c` at Linux tag **v6.18.35** —
  `msrs_to_save_base`, `msrs_to_save_pmu`, `emulated_msrs_all`, and
  `msr_based_features_all_except_vmx`, plus the VMX feature-MSR probe range
  `KVM_FIRST_EMULATED_VMX_MSR..KVM_LAST_EMULATED_VMX_MSR` (0x480–0x491, x86.h:94–95).
  v6.18.35 is the tag task 04 pins; cross-checked against `guest/linux/versions.lock`
  (`KERNEL_VERSION=6.18.35`) — they agree;
- **(b)** every MSR named in INTEGRATION.md §7; and
- **(c)** the classes below, each expanded by the stated, mechanically checkable match
  rule in its section preamble against `arch/x86/include/asm/msr-index.h` at the same
  tag.

MSRs outside the reference set need no row: the §1 default-deny filter denies, logs, and
#GPs them by construction, and any off-contract access observed at runtime is triaged
into a new row with a version bump (§6). All tables share the uniform
`| MSR | Index | Read | Write | Rationale | Citation |` grammar from the preamble.

Disposition vocabulary (normative semantics):

- `allow-fixed(value)` — RDMSR returns the 64-bit constant, computed into
  `kvm_run.msr.data` by the userspace exit handler. Read-only by definition: the row's
  write column must be `deny-gp` or `deny-ignore-write`.
- `allow-stateful` — architecturally guest-writable state, read and written normally;
  the only disposition whose rows enter the KVM MSR-filter *allow* bitmaps (KVM
  virtualizes them in-kernel), and every such value is captured in the `vm_state` blob
  per INTEGRATION.md §4.
- `emulate-vtime` — the value is a pure function of V-time via a named `consonance/vtime`
  formula (e.g. `VClock::tsc(work)`); the host clock is never consulted.
- `emulate-timerqueue` — writes convert to absolute V-time deadlines armed on the
  userspace `TimerQueue`; reads return the armed deadline from emulated timer state.
- `emulate-apic` — reserved for an x2APIC per-register sub-table; unused in this
  revision (§3.12 resolves the whole range as `deny-gp` with x2APIC hidden).
- `emulate-device` — userspace device-state emulation (PIT counters/command, CMOS index/
  data window, xAPIC PPR/EOI/ESR/ICR) whose value is a pure function of emulated device
  state and V-time; it **always** carries a closed §6 formula id (e.g. `pit.ch2`,
  `cmos.index-latch`, `apic.ppr`) — a bare `emulate` with no formula id is never valid.
  Used only in the §5 timer/CMOS/MMIO sub-tables, never for an MSR.
- `deny-gp` — the access exits to userspace (§1 mechanism), is logged with direction,
  index, data, RIP, and V-time, then #GP is injected (`kvm_run.msr.error = 1`).
- `deny-ignore-write` — the write exits to userspace, is logged the same way, then
  dropped (`error = 0`); never a default, always a deliberate per-row choice.

| § | Class | Surface | Source fragment |
|---|---|---|---|
| 3.1 | `kvmclock` | KVM PV time/feature MSRs (`MSR_KVM_*`: 0x4b564dxx + legacy 0x11/0x12) and three Hyper-V PV-time MSRs | `docs/fragments/msr-kvmclock.md` |
| 3.2 | `kvm-exposed` | KVM-manufactured Hyper-V enlightenment MSRs from `emulated_msrs_all` | `docs/fragments/msr-kvm-exposed.md` |
| 3.3 | `tsc` | TSC plumbing (IA32_TSC, TSC_ADJUST, TSC_DEADLINE, TSC_AUX) + Hyper-V TSC machinery | `docs/fragments/msr-tsc.md` |
| 3.4 | `timing-instr` | WAITPKG control (IA32_UMWAIT_CONTROL) | `docs/fragments/msr-timing-instr.md` |
| 3.5 | `power-thermal` | APERF/MPERF, P-states, thermal, turbo, RAPL, C-state residency, HWP, HFI/Thread Director | `docs/fragments/msr-power-thermal.md` |
| 3.6 | `pmu` | every performance-monitoring MSR (host owns the PMU) | `docs/fragments/msr-pmu.md` |
| 3.7 | `debug-lbr` | IA32_DEBUGCTL, LBR stacks, LER, architectural LBR, silicon debug | `docs/fragments/msr-debug-lbr.md` |
| 3.8 | `intel-pt` | `IA32_RTIT_*` | `docs/fragments/msr-intel-pt.md` |
| 3.9 | `speculation` | SPEC_CTRL/PRED_CMD/FLUSH_CMD, ARCH/CORE capabilities, TSX_CTRL, DOITM, AMD analogs | `docs/fragments/msr-speculation.md` |
| 3.10 | `microcode` | IA32_BIOS_SIGN_ID / IA32_BIOS_UPDT_TRIG | `docs/fragments/msr-microcode.md` |
| 3.11 | `entropy` | MSR_SMI_COUNT | `docs/fragments/msr-entropy.md` |
| 3.12 | `x2apic` | the full 0x800–0x8FF range, gapless | `docs/fragments/msr-x2apic.md` |
| 3.13 | `arch-stateful` | EFER, SYSCALL/SYSENTER block, FS/GS bases, PAT, MISC_ENABLE, MTRRs, MCA surface, denied CET/MPX/XFD/XSS | `docs/fragments/msr-arch-stateful.md` |
| 3.14 | `boot-baseline` | feature-control lock, platform identity/frequency, VMX capability range | `docs/fragments/msr-boot-baseline.md` |
| 3.15 | `other` | PPIN / PPIN_CTL | `docs/fragments/msr-other.md` |

**Assembly notes.**

- The class sections below were assembled from the listed fragments verbatim except for
  the explicitly noted merges; where a section says "the msr-X fragment", read the
  corresponding §3.x class section (the fragment files remain in `docs/fragments/` as
  construction artifacts).
- De-duplication (one normative row per index, per §6's serialization rule):
  `MSR_PLATFORM_INFO` (0xce) — normative row in §3.14, cross-reference in §3.5;
  `MSR_KVM_*` 0x4b564d02–0x4b564d07 — per-MSR rows in §3.1, combined duplicate removed
  from §3.2; intra-table exact-coverage duplicates collapsed in §3.6 (`pmu`) and §3.7
  (`debug-lbr`) — see the merge notes under those tables. No index coverage or
  disposition changed in any merge.
- The instruction & VMX-control disposition table and the timer/time-device surface
  (task-06 deliverable items 4–5) are **now present** as §4 and §5; rows that reference
  "the instruction table" or the xAPIC MMIO sub-table point at those sections. §6's
  canonical form carries their record types (`insn`, `timer`, `mmio`).

### 3.1 Class `kvmclock` — KVM paravirtual clock & PV-time surface

Class `kvmclock` covers the paravirtual time-export surface named by INTEGRATION.md §7's
"KVM paravirtual clock" vector: the KVM PV MSRs — defined exactly as every name matching
`MSR_KVM_*` in `arch/x86/include/uapi/asm/kvm_para.h` at v6.18.35 (which yields the legacy
indexes 0x11/0x12 **and** the custom block) plus the entire architecturally reserved KVM PV
MSR range 0x4b564d00–0x4b564dff per `Documentation/virt/kvm/x86/msr.rst` — together with
the three Hyper-V PV-time MSRs from `emulated_msrs_all` assigned to this class
(VP_RUNTIME, TIME_REF_COUNT, REFERENCE_TSC; the remaining Hyper-V time MSRs are disposed in
the `tsc` fragment). These MSRs exist only to import host real time into the guest — shared
time pages carrying host wall clock and TSC multiplier/offset, steal-time pages reporting
real nanoseconds the vCPU was descheduled, async-PF machinery that injects interrupts on
host paging latency, and a 100 ns host reference counter — so under §7 the frozen CPUID
model hides the KVM PV leaves (0x4000_00xx) entirely and enumerates no Hyper-V leaves,
making every feature gate moot (KVM_FEATURE_CLOCKSOURCE bit 0, ASYNC_PF bit 4, STEAL_TIME
bit 5, PV_EOI bit 6, POLL_CONTROL bit 12, ASYNC_PF_INT bit 14, MIGRATION_CONTROL bit 17,
CLOCKSOURCE2 bit 3 and CLOCKSOURCE_STABLE_BIT 24 likewise never advertised), and every MSR
in the class is `deny-gp` in both directions via `KVM_X86_SET_MSR_FILTER` +
`KVM_CAP_X86_USER_SPACE_MSR`/`KVM_MSR_EXIT_REASON_FILTER` — logged with index and RIP, then
#GP injected, never a silent zero. Two pitfalls are encoded in the rows: the legacy
kvmclock indexes 0x11/0x12 sit **outside** the 0x4b564dxx block, so a filter that denies
only the custom range misses them; and the whole reserved block carries a blanket-deny
residual row so PV MSRs added by future kernels stay closed by default. Defense in depth
per §7: the task-04 guest kernel is built without PV-clock support.

| MSR | Index | Read | Write | Rationale | Citation |
|---|---|---|---|---|---|
| MSR_KVM_WALL_CLOCK | 0x11 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock": legacy wall-clock MSR points a guest page at host wall-clock time; gated by KVM_FEATURE_CLOCKSOURCE (bit 0), which the hidden PV leaf never advertises — and at 0x11 it escapes any filter that only denies the 0x4b564dxx block. | linux-6.18.35 arch/x86/kvm/x86.c (emulated_msrs_all); arch/x86/include/uapi/asm/kvm_para.h; Documentation/virt/kvm/x86/msr.rst (MSR_KVM_WALL_CLOCK); INTEGRATION.md §7 (KVM paravirtual clock) |
| MSR_KVM_SYSTEM_TIME | 0x12 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock": legacy system-time MSR enables a pvclock_vcpu_time_info page carrying host TSC multiplier/offset and host wall time; gated by hidden KVM_FEATURE_CLOCKSOURCE (bit 0), and at 0x12 it escapes a 0x4b564dxx-only filter. | linux-6.18.35 arch/x86/kvm/x86.c (emulated_msrs_all); arch/x86/include/uapi/asm/kvm_para.h; Documentation/virt/kvm/x86/msr.rst (MSR_KVM_SYSTEM_TIME); INTEGRATION.md §7 (KVM paravirtual clock) |
| MSR_KVM_WALL_CLOCK_NEW | 0x4b564d00 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock": new-ABI wall-clock MSR exports host wall-clock seconds/nanoseconds into a guest page; gated by KVM_FEATURE_CLOCKSOURCE2 (bit 3), never advertised by the frozen CPUID model. | linux-6.18.35 arch/x86/kvm/x86.c (emulated_msrs_all); arch/x86/include/uapi/asm/kvm_para.h; Documentation/virt/kvm/x86/msr.rst (MSR_KVM_WALL_CLOCK_NEW); INTEGRATION.md §7 (KVM paravirtual clock, CPUID stability) |
| MSR_KVM_SYSTEM_TIME_NEW | 0x4b564d01 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock": new-ABI kvmclock shared page carries host TSC multipliers/offsets and wall time — a continuously updated host-time channel beside VClock; KVM_FEATURE_CLOCKSOURCE2 (bit 3) and CLOCKSOURCE_STABLE_BIT (24) are hidden. | linux-6.18.35 arch/x86/kvm/x86.c (emulated_msrs_all); arch/x86/include/uapi/asm/kvm_para.h; Documentation/virt/kvm/x86/msr.rst (MSR_KVM_SYSTEM_TIME_NEW); INTEGRATION.md §7 (KVM paravirtual clock, CPUID stability) |
| MSR_KVM_ASYNC_PF_EN | 0x4b564d02 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" and PLAN.md's interrupt-timing rule: async PF delivers page-ready notifications at host paging latency — interrupts the InjectionPlanner did not schedule at an exact V-time; KVM_FEATURE_ASYNC_PF (bit 4) is hidden. | linux-6.18.35 arch/x86/kvm/x86.c (emulated_msrs_all); arch/x86/include/uapi/asm/kvm_para.h; Documentation/virt/kvm/x86/msr.rst (MSR_KVM_ASYNC_PF_EN); INTEGRATION.md §7 (KVM paravirtual clock); PLAN.md trap table (interrupt timing) |
| MSR_KVM_STEAL_TIME | 0x4b564d03 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock": the steal-time page reports real nanoseconds the vCPU was descheduled on the host — a direct wall-clock leak that would also vary run to run; KVM_FEATURE_STEAL_TIME (bit 5) is hidden. | linux-6.18.35 arch/x86/kvm/x86.c (emulated_msrs_all); arch/x86/include/uapi/asm/kvm_para.h; Documentation/virt/kvm/x86/msr.rst (MSR_KVM_STEAL_TIME, struct kvm_steal_time); INTEGRATION.md §7 (KVM paravirtual clock) |
| MSR_KVM_PV_EOI_EN | 0x4b564d04 | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface" via the PV-clock clause: PV EOI moves EOI signaling into a guest/host shared page, bypassing the single deterministic emulate-apic EOI path of the split-irqchip plan; KVM_FEATURE_PV_EOI (bit 6) is hidden. | linux-6.18.35 arch/x86/kvm/x86.c (emulated_msrs_all); arch/x86/include/uapi/asm/kvm_para.h; Documentation/virt/kvm/x86/msr.rst (MSR_KVM_PV_EOI_EN); INTEGRATION.md §7 (KVM paravirtual clock, x2APIC MSR surface) |
| MSR_KVM_POLL_CONTROL | 0x4b564d05 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" / "Timer devices": halt-poll control couples guest idle behavior to host-side real-time polling, conflicting with §3's idle-skip protocol where HLT is a V-time event; KVM_FEATURE_POLL_CONTROL (bit 12) is hidden. | linux-6.18.35 arch/x86/kvm/x86.c (emulated_msrs_all); arch/x86/include/uapi/asm/kvm_para.h; Documentation/virt/kvm/x86/msr.rst (MSR_KVM_POLL_CONTROL); INTEGRATION.md §3 (idle-skip) + §7 (Timer devices) |
| MSR_KVM_ASYNC_PF_INT | 0x4b564d06 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" and PLAN.md's interrupt-timing rule: configures the interrupt vector for host-latency-timed async-PF page-ready events; KVM_FEATURE_ASYNC_PF_INT (bit 14) is hidden. | linux-6.18.35 arch/x86/kvm/x86.c (emulated_msrs_all); arch/x86/include/uapi/asm/kvm_para.h; Documentation/virt/kvm/x86/msr.rst (MSR_KVM_ASYNC_PF_INT); INTEGRATION.md §7 (KVM paravirtual clock); PLAN.md trap table (interrupt timing) |
| MSR_KVM_ASYNC_PF_ACK | 0x4b564d07 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock": ack side of the async-PF channel; meaningless and denied alongside ASYNC_PF_EN/INT since the feature bits (4, 14) are hidden — no half-exposed feature. | linux-6.18.35 arch/x86/kvm/x86.c (emulated_msrs_all); arch/x86/include/uapi/asm/kvm_para.h; Documentation/virt/kvm/x86/msr.rst (MSR_KVM_ASYNC_PF_ACK); INTEGRATION.md §7 (KVM paravirtual clock) |
| MSR_KVM_MIGRATION_CONTROL | 0x4b564d08 | deny-gp | deny-gp | Closes §7 "CPUID stability" via the PV-clock clause: migration control is host-policy surface with no deterministic analog; KVM_FEATURE_MIGRATION_CONTROL (bit 17) is hidden. In the reference set via the `MSR_KVM_*` class match rule — it is absent from emulated_msrs_all at v6.18.35. | linux-6.18.35 arch/x86/include/uapi/asm/kvm_para.h; Documentation/virt/kvm/x86/msr.rst (MSR_KVM_MIGRATION_CONTROL); INTEGRATION.md §7 (KVM paravirtual clock, CPUID stability) |
| KVM PV MSR range (reserved residual) | 0x4b564d09-0x4b564dff | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" default-deny: the whole custom block 0x4b564d00–0x4b564dff is architecturally reserved for KVM PV MSRs, so unassigned indexes (and any assigned by future kernels) #GP loudly rather than ever passing through. | linux-6.18.35 Documentation/virt/kvm/x86/msr.rst ("Custom MSRs ... 0x4b564d00 to 0x4b564dff"); INTEGRATION.md §7 (default-deny, KVM paravirtual clock) |
| HV_X64_MSR_VP_RUNTIME | 0x40000010 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" generalized to all PV time enlightenments: VP runtime counts 100 ns units of host real time the VP has consumed; the frozen CPUID model enumerates no Hyper-V leaves, so the MSR architecturally does not exist — #GP. | linux-6.18.35 arch/x86/kvm/x86.c (emulated_msrs_all, CONFIG_KVM_HYPERV); include/hyperv/hvgdk_mini.h (HV_X64_MSR_VP_RUNTIME); Hyper-V TLFS; INTEGRATION.md §7 (KVM paravirtual clock, CPUID stability) |
| HV_X64_MSR_TIME_REF_COUNT | 0x40000020 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" generalized to all PV time enlightenments: the partition reference counter is host real time in 100 ns units — the bluntest PV wall-clock leak after kvmclock itself; no Hyper-V leaves enumerated — #GP both directions (the MSR is read-only per TLFS, but it does not exist here at all). | linux-6.18.35 arch/x86/kvm/x86.c (emulated_msrs_all, CONFIG_KVM_HYPERV); include/hyperv/hvgdk_mini.h (HV_X64_MSR_TIME_REF_COUNT); Hyper-V TLFS; INTEGRATION.md §7 (KVM paravirtual clock, CPUID stability) |
| HV_X64_MSR_REFERENCE_TSC | 0x40000021 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" and "TSC plumbing": the TSC reference page exports a host TSC scale/offset pair letting the guest reconstruct host real time from raw TSC; no Hyper-V leaves enumerated — #GP. | linux-6.18.35 arch/x86/kvm/x86.c (emulated_msrs_all, CONFIG_KVM_HYPERV); include/hyperv/hvgdk_mini.h (HV_X64_MSR_REFERENCE_TSC); Hyper-V TLFS; INTEGRATION.md §7 (KVM paravirtual clock, TSC plumbing) |

### 3.2 Class `kvm-exposed` — KVM-manufactured Hyper-V/PV enlightenment MSRs

Class `kvm-exposed` covers the synthetic MSRs that KVM itself manufactures and advertises
through `KVM_GET_MSR_INDEX_LIST` — reference-set clause (a), the `emulated_msrs_all` array
in `arch/x86/kvm/x86.c` at v6.18.35 (lines 394–461; the pin agrees with
`guest/linux/versions.lock`, KERNEL_VERSION=6.18.35). Match rule: every `HV_X64_MSR_*`
name in that array's `CONFIG_KVM_HYPERV` block (x86.c:398–416) plus every `MSR_KVM_*`
PV-feature MSR in the array (x86.c:418–419, 459), excluding entries already disposed in
sibling fragments — the kvmclock fragment owns `MSR_KVM_WALL_CLOCK`/`MSR_KVM_SYSTEM_TIME`
and their `_NEW` variants (x86.c:395–396), and the `tsc` fragment owns
`HV_X64_MSR_TSC_FREQUENCY`/`APIC_FREQUENCY`/`REENLIGHTENMENT_CONTROL`/
`TSC_EMULATION_CONTROL`/`TSC_EMULATION_STATUS`/`TSC_INVARIANT_CONTROL` (x86.c:400–401,
410–411). **Exhaustiveness note (extends the match rule):** for the SynIC and synthetic-timer
groups, `emulated_msrs_all` lists only one *anchor* per group (`HV_X64_MSR_SCONTROL` 0x40000080
and `HV_X64_MSR_STIMER0_CONFIG` 0x400000b0); KVM services the remaining registers of each
group through its per-MSR switch, not the index list. Task 06 wants exhaustive machine-readable
coverage, so this class additionally rows the **full SynIC/stimer register set** defined in
`include/hyperv/hvgdk_mini.h` — `SVERSION`/`SIEFP`/`SIMP`/`EOM` (0x40000081–0x40000084),
`SINT0`–`SINT15` (0x40000090–0x4000009f), and `STIMER0_COUNT`/`STIMER1–3 CONFIG/COUNT`
(0x400000b1–0x400000b7) — as explicit `deny-gp` rows, not merely the §1 default-deny.
None of these MSRs exist on real hardware: each is a door into a paravirtual
interface, and §7's kvmclock vector mandates that the frozen CPUID model hide the PV
leaves (`0x4000_00xx`) entirely. With neither the Hyper-V vendor leaves nor the KVM
signature leaf enumerated, every MSR in this class is architecturally nonexistent, so
`deny-gp` in both directions is bit-exact with the advertised CPU; correspondingly,
KVM_FEATURE_ASYNC_PF(4), KVM_FEATURE_STEAL_TIME(5), KVM_FEATURE_PV_EOI(6),
KVM_FEATURE_POLL_CONTROL(12), and KVM_FEATURE_ASYNC_PF_INT(14) are never reported (gate 5:
no half-exposed features). The determinism stakes are concrete: Hyper-V stimers are armed
as host hrtimers against the host-real-time reference counter
(`arch/x86/kvm/hyperv.c:634–682`), the VP-assist and SynIC pages are guest memory the host
mutates asynchronously, the syndbg MSRs are a network-backed debug transport, and the
`MSR_KVM_*` block leaks host scheduling and paging latency (async-PF delivery, steal
time, PV-EOI/poll-control interrupt-path coupling). Per the contract's §1 policy,
`deny-gp` here means: `KVM_X86_SET_MSR_FILTER` + `KVM_MSR_EXIT_REASON_FILTER` exit to
userspace, log MSR index and guest RIP, then inject #GP — never a silent in-kernel fault.
As defense in depth, vmm-core must not enable `KVM_CAP_HYPERV_*` capabilities, so KVM's
in-kernel Hyper-V emulation is never reachable even if the filter were misconfigured.

| MSR | Index | Read | Write | Rationale | Citation |
|---|---|---|---|---|---|
| HV_X64_MSR_GUEST_OS_ID | 0x40000000 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" generalized to all PV enlightenments: writing the guest OS ID is the TLFS prerequisite for enabling the hypercall page and with it the whole Hyper-V time surface; no 0x4000_00xx leaves are enumerated, so the MSR does not exist — #GP architectural. | linux-6.18.35 arch/x86/kvm/x86.c:399 (emulated_msrs_all, CONFIG_KVM_HYPERV); include/hyperv/hvgdk_mini.h:63; Hyper-V TLFS (guest OS identity MSR); INTEGRATION.md §7 (KVM paravirtual clock, CPUID stability) |
| HV_X64_MSR_HYPERCALL | 0x40000001 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock": enabling it maps a host-supplied hypercall code page into guest memory (run-dependent guest-memory mutation) and opens the HvCall* surface, including timing hypercalls; interface hidden — #GP. | linux-6.18.35 arch/x86/kvm/x86.c:399 (emulated_msrs_all, CONFIG_KVM_HYPERV); include/hyperv/hvgdk_mini.h:64; Hyper-V TLFS (hypercall interface establishment); INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_VP_INDEX | 0x40000002 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" generalized: the VP index is host-assigned topology surface with no architectural analog; interface hidden — #GP (single-vCPU contract has no legitimate consumer anyway). | linux-6.18.35 arch/x86/kvm/x86.c:405 (emulated_msrs_all, CONFIG_KVM_HYPERV); include/hyperv/hvgdk_mini.h:65; Hyper-V TLFS (virtual processor index); INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_RESET | 0x40000003 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" generalized: writing 1 triggers an immediate host-side partition reset — a guest-reachable host action outside the deterministic run loop and snapshot protocol; interface hidden — #GP. | linux-6.18.35 arch/x86/kvm/x86.c:404 (emulated_msrs_all, CONFIG_KVM_HYPERV); include/hyperv/hvgdk_mini.h:66; Hyper-V TLFS (HV_X64_MSR_RESET); INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_EOI / ICR / TPR (synthetic APIC) | 0x40000070–0x40000072 | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface" via the PV clause: the Hyper-V synthetic-APIC MSRs (synthetic EOI 0x70, synthetic ICR 0x71, synthetic TPR 0x72) are an MSR-based APIC enlightenment that would bypass the single deterministic userspace-xAPIC path; no Hyper-V leaves enumerated, so the interface does not exist — #GP. Rowed for exhaustiveness (macro-defined in hvgdk_mini.h; serviced by KVM's per-MSR switch, not separate emulated_msrs_all entries). | include/hyperv/hvgdk_mini.h (HV_X64_MSR_EOI 0x40000070, ICR 0x40000071, TPR 0x40000072); linux-6.18.35 arch/x86/kvm/hyperv.c (synthetic APIC msr switch); Hyper-V TLFS (synthetic APIC); INTEGRATION.md §7 (x2APIC MSR surface, KVM paravirtual clock) |
| HV_X64_MSR_VP_ASSIST_PAGE | 0x40000073 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock": enabling it designates a guest page the host writes enlightenment state into asynchronously (APIC assist, enlightened VMCS) — DMA-like run-dependent memory mutation; interface hidden — #GP. | linux-6.18.35 arch/x86/kvm/x86.c:409 (emulated_msrs_all, CONFIG_KVM_HYPERV); include/hyperv/hvgdk_mini.h:77, 145–148 (enable/address layout); Hyper-V TLFS (VP assist page); INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_SCONTROL | 0x40000080 | deny-gp | deny-gp | Closes §7 "Timer devices": SCONTROL enables the SynIC, gateway to SINTx/SIEFP/SIMP message and event pages that the host writes asynchronously and through which host-hrtimer-fired stimer expirations are delivered; interface hidden — #GP. | linux-6.18.35 arch/x86/kvm/x86.c:407 (emulated_msrs_all, CONFIG_KVM_HYPERV); include/hyperv/hvgdk_mini.h:80; Hyper-V TLFS (SynIC); INTEGRATION.md §7 (Timer devices, KVM paravirtual clock) |
| HV_X64_MSR_STIMER0_CONFIG | 0x400000b0 | deny-gp | deny-gp | Closes §7 "Timer devices" (no in-kernel timer unless proven V-time-driven): KVM arms stimers as host hrtimers against the host-real-time reference counter (hyperv.c stimer_start), the exact in-kernel-timer leak §7 forbids; guest timing goes through the architectural LAPIC rows backed by userspace TimerQueue instead — #GP. | linux-6.18.35 arch/x86/kvm/x86.c:408 (emulated_msrs_all, CONFIG_KVM_HYPERV); arch/x86/kvm/hyperv.c:634–682 (stimer_start → hrtimer_start on get_time_ref_counter); include/hyperv/hvgdk_mini.h:114; INTEGRATION.md §7 (Timer devices) |
| HV_X64_MSR_SVERSION / SIEFP / SIMP / EOM | 0x40000081–0x40000084 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock"/"Timer devices": SynIC version + the SIEF/SIM message/event page-base MSRs + end-of-message — the host-written async message/event pages the SynIC and stimer delivery ride on; no Hyper-V leaves enumerated, so the whole interface #GPs. Rowed for exhaustiveness (macro-defined in hvgdk_mini.h; not separate entries in emulated_msrs_all — KVM services them via its per-MSR switch). | include/hyperv/hvgdk_mini.h (HV_X64_MSR_SVERSION 0x81, SIEFP 0x82, SIMP 0x83, EOM 0x84); linux-6.18.35 arch/x86/kvm/hyperv.c (SynIC msr switch); Hyper-V TLFS (SynIC); INTEGRATION.md §7 (KVM paravirtual clock, Timer devices) |
| HV_X64_MSR_SINT0–SINT15 | 0x40000090–0x4000009f | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock": the 16 synthetic interrupt-source vectors that map SynIC messages/stimer expirations to guest vectors — host-driven interrupt routing outside the InjectionPlanner; no Hyper-V leaves enumerated — #GP. Rowed for exhaustiveness (per-MSR switch, not emulated_msrs_all entries). | include/hyperv/hvgdk_mini.h (HV_X64_MSR_SINT0 0x90 … SINT15 0x9f); linux-6.18.35 arch/x86/kvm/hyperv.c (synic_set_msr); Hyper-V TLFS (SynIC SINTx); INTEGRATION.md §7 (KVM paravirtual clock); PLAN.md (interrupt timing) |
| HV_X64_MSR_STIMER0_COUNT / STIMER1–3 CONFIG+COUNT | 0x400000b1–0x400000b7 | deny-gp | deny-gp | Closes §7 "Timer devices": the remaining synthetic-timer config/count registers (STIMER0_COUNT plus STIMER1/2/3) — all arm host hrtimers against the host reference counter, the in-kernel-timer leak §7 forbids; no Hyper-V leaves enumerated — #GP. Rowed for exhaustiveness (per-MSR switch, not emulated_msrs_all entries). | include/hyperv/hvgdk_mini.h (HV_X64_MSR_STIMER0_COUNT 0xb1 … STIMER3_COUNT 0xb7); linux-6.18.35 arch/x86/kvm/hyperv.c (stimer_start → hrtimer_start); Hyper-V TLFS (synthetic timers); INTEGRATION.md §7 (Timer devices) |
| HV_X64_MSR_CRASH_P0 | 0x40000100 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" generalized: crash parameter 0 of a guest→host notification channel that is host-side state, not vm_state; interface hidden — #GP. | linux-6.18.35 arch/x86/kvm/x86.c:402 (emulated_msrs_all, CONFIG_KVM_HYPERV); include/hyperv/hvgdk_mini.h:127; Linux Documentation/virt/kvm/api.rst (KVM_CAP_HYPERV); INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_CRASH_P1 | 0x40000101 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" generalized: crash parameter 1, same hidden host-notification channel — #GP. | linux-6.18.35 arch/x86/kvm/x86.c:402 (emulated_msrs_all, CONFIG_KVM_HYPERV); include/hyperv/hvgdk_mini.h:128; INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_CRASH_P2 | 0x40000102 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" generalized: crash parameter 2, same hidden host-notification channel — #GP. | linux-6.18.35 arch/x86/kvm/x86.c:402 (emulated_msrs_all, CONFIG_KVM_HYPERV); include/hyperv/hvgdk_mini.h:129; INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_CRASH_P3 | 0x40000103 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" generalized: crash parameter 3, same hidden host-notification channel — #GP. | linux-6.18.35 arch/x86/kvm/x86.c:403 (emulated_msrs_all, CONFIG_KVM_HYPERV); include/hyperv/hvgdk_mini.h:130; INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_CRASH_P4 | 0x40000104 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" generalized: crash parameter 4, same hidden host-notification channel — #GP. | linux-6.18.35 arch/x86/kvm/x86.c:403 (emulated_msrs_all, CONFIG_KVM_HYPERV); include/hyperv/hvgdk_mini.h:131; INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_CRASH_CTL | 0x40000105 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" generalized: a read reports host crash-notify capability (CRASH_NOTIFY) — host policy, not guest state — and a write fires a host-side notification; interface hidden — #GP. | linux-6.18.35 arch/x86/kvm/x86.c:403 (emulated_msrs_all, CONFIG_KVM_HYPERV); include/hyperv/hvgdk_mini.h:132, 140 (crash param count); INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_SYNDBG_OPTIONS | 0x400000ff | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" generalized: configures the Hyper-V synthetic debugger, a network-backed transport that imports external real-world I/O into the guest; syndbg CPUID leaves (0x40000080–82) never enumerated — #GP. | linux-6.18.35 arch/x86/kvm/x86.c:412 (emulated_msrs_all, CONFIG_KVM_HYPERV); arch/x86/kvm/hyperv.h:54 (index 0x400000FF), 38–40 (syndbg CPUID leaves); INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_SYNDBG_CONTROL | 0x400000f1 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" generalized: send/receive control of the syndbg network transport — guest-triggered external I/O; never enumerated — #GP. | linux-6.18.35 arch/x86/kvm/x86.c:413 (emulated_msrs_all, CONFIG_KVM_HYPERV); arch/x86/kvm/hyperv.h:49 (index 0x400000F1); INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_SYNDBG_STATUS | 0x400000f2 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" generalized: status readback reflects external debugger/network state, varying run to run; never enumerated — #GP. | linux-6.18.35 arch/x86/kvm/x86.c:413 (emulated_msrs_all, CONFIG_KVM_HYPERV); arch/x86/kvm/hyperv.h:50 (index 0x400000F2); INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_SYNDBG_SEND_BUFFER | 0x400000f3 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" generalized: designates a guest page whose contents are pushed out the debug transport — guest-reachable external output; never enumerated — #GP. | linux-6.18.35 arch/x86/kvm/x86.c:414 (emulated_msrs_all, CONFIG_KVM_HYPERV); arch/x86/kvm/hyperv.h:51 (index 0x400000F3); INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_SYNDBG_RECV_BUFFER | 0x400000f4 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" generalized: designates a guest page the host fills with received debug-network data — nondeterministic external input written into guest memory; never enumerated — #GP. | linux-6.18.35 arch/x86/kvm/x86.c:414 (emulated_msrs_all, CONFIG_KVM_HYPERV); arch/x86/kvm/hyperv.h:52 (index 0x400000F4); INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_SYNDBG_PENDING_BUFFER | 0x400000f5 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" generalized: pending-buffer readback varies with external debugger traffic timing — a run-dependent read; never enumerated — #GP. | linux-6.18.35 arch/x86/kvm/x86.c:415 (emulated_msrs_all, CONFIG_KVM_HYPERV); arch/x86/kvm/hyperv.h:53 (index 0x400000F5); INTEGRATION.md §7 (KVM paravirtual clock) |

*Assembly merge note: the source fragment's combined row `MSR_KVM_ASYNC_PF_EN /
MSR_KVM_STEAL_TIME / MSR_KVM_PV_EOI_EN / MSR_KVM_POLL_CONTROL / MSR_KVM_ASYNC_PF_INT /
MSR_KVM_ASYNC_PF_ACK` (0x4b564d02–0x4b564d07, deny-gp/deny-gp) duplicated the per-MSR
rows of class `kvmclock` (§3.1) index-for-index with identical dispositions and was
removed; §3.1 is normative for the `MSR_KVM_*` block.*

[question] emulated_msrs_all at v6.18.35 also lists HV_X64_MSR_TIME_REF_COUNT (0x40000020),
HV_X64_MSR_REFERENCE_TSC (0x40000021) (x86.c:400) and HV_X64_MSR_VP_RUNTIME (0x40000010)
(x86.c:406), which were not assigned to this fragment and are not in the `tsc` fragment —
confirm the kvmclock (or another sibling) fragment carries them; all three must be deny-gp
both directions (TIME_REF_COUNT/REFERENCE_TSC are direct host-real-time clocks, VP_RUNTIME
is host scheduling time, the Hyper-V analog of steal time), otherwise reference-set clause
(a) coverage has a gap.
**[resolved at assembly]** The `kvmclock` class (§3.1) carries all three:
`HV_X64_MSR_VP_RUNTIME` (0x40000010), `HV_X64_MSR_TIME_REF_COUNT` (0x40000020), and
`HV_X64_MSR_REFERENCE_TSC` (0x40000021), each `deny-gp`/`deny-gp` — clause (a)
coverage is closed.

### 3.3 Class `tsc` — TSC plumbing

Class `tsc` covers every MSR through which the time-stamp counter — the primary time leak
named in INTEGRATION.md §7 — can carry host real time into the guest: the counter itself
(`IA32_TSC`), its software offset (`IA32_TSC_ADJUST`), the RDTSCP/RDPID auxiliary value
(`IA32_TSC_AUX`), the TSC-deadline LAPIC timer arm register, AMD's TSC scaling ratio, and
the six Hyper-V synthetic MSRs that re-export TSC/APIC frequency and TSC-emulation
machinery. The governing rule is §7's TSC-plumbing clause: the host TSC must never reach
the guest. Every readable value in this class is therefore either derived from
`consonance/vtime` (`VClock::tsc(work) = tsc_base + floor(vns(work) · tsc_hz / 10⁹)`, with
`tsc_base`/ratio captured in the `vm_state` blob per §4) or echoed from guest-written
state held in `vm_state`; timer arming goes through the userspace `TimerQueue` (§7 "Timer
devices": no in-kernel LAPIC hrtimer, which runs on host real time); and everything not
derivable from V-time — in particular all Hyper-V enlightenments, which the frozen CPUID
model does not advertise — is default-deny (#GP) under `KVM_X86_SET_MSR_FILTER`, surfaced
as a loud event rather than a passthrough.

| MSR | Index | Read | Write | Rationale | Citation |
|---|---|---|---|---|---|
| MSR_IA32_TSC | 0x10 | emulate-vtime | emulate-vtime | Closes §7 "TSC plumbing": reads return VClock::tsc(work) computed from retired-branch work, never the host counter; a write deterministically rebases tsc_base in vm_state (new_base = value − floor(vns·tsc_hz/10⁹)) so readback is coherent and replayable. **TSC_ADJUST coupling (SDM, gate-5 with CPUID.7.0:EBX[1]=1):** because the model enumerates IA32_TSC_ADJUST, a WRMSR to IA32_TSC must also add the delta `(value − tsc_before)` to the IA32_TSC_ADJUST value in vm_state — exactly as KVM does (`kvm_set_msr_common` MSR_IA32_TSC → `kvm_synchronize_tsc`/`adjust_tsc_offset`, which carries the adjust) — so a subsequent RDMSR 0x3b reflects the write; the delta lives entirely in vm_state with no host-TSC involvement. **Hashed formula ids (§6): read `vclock.tsc`, write `vclock.tsc.write`** (rebase tsc_base + TSC_ADJUST delta) — the coupling is in the canonical form, not just here. (No TSC-deadline TimerQueue entries exist to recompute: 0x6e0 is `deny-gp` and the LAPIC timer is V-time-absolute one-shot/periodic, §5.) | linux-6.18.35 arch/x86/kvm/x86.c (msrs_to_save_base; kvm_set_msr_common MSR_IA32_TSC → ia32_tsc_adjust delta); arch/x86/include/asm/msr-index.h; Intel SDM Vol.3B §17.17.3 (WRMSR IA32_TSC shifts IA32_TSC_ADJUST by the same delta) + Vol.4 Table 2-2; INTEGRATION.md §7 (TSC plumbing) + §4 (vm_state); consonance/vtime/src/clock.rs (VClock::tsc) |
| MSR_TSC_AUX | 0xc0000103 | allow-stateful | allow-stateful | Closes §7 "TSC plumbing" (rr-paper current-core leak, arXiv:1610.02144): RDTSCP/RDPID aux must echo the guest-written value held in vm_state, never the host's per-core IA32_TSC_AUX; pure software state with no time content of its own. **Pure-architectural requirement (§1.1 class (a), normative):** vmm-core MUST load the guest's vm_state TSC_AUX into the **physical** IA32_TSC_AUX for the duration of guest execution (KVM does this for `allow-stateful` MSRs via the VM-entry MSR-load / guest-MSR switch), so that an *uninterceptable* native read — RDPID on any future RDPID-bearing baseline, or RDTSCP if its exit were ever disabled — returns the guest-echoed value, never the host's per-core id. This is what makes RDPID class (a) rather than a host leak (on the det-cfl-v1 baseline RDPID is class (b) `fault-absent` — physically absent, §4). | linux-6.18.35 arch/x86/kvm/x86.c (msrs_to_save_base); arch/x86/include/asm/msr-index.h; Intel SDM Vol.4 Table 2-2 + RDTSCP/RDPID (felixcloutier.com/x86/rdpid); INTEGRATION.md §4 (vm_state MSR capture) |
| HV_X64_MSR_TSC_FREQUENCY | 0x40000022 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" generalized to all PV time enlightenments: the frozen CPUID model exposes no Hyper-V leaves (no HV_ACCESS_FREQUENCY_MSRS), so this synthetic frequency MSR architecturally does not exist and a host-derived tsc_hz must not leak through it. | linux-6.18.35 arch/x86/kvm/x86.c (emulated_msrs_all); Hyper-V TLFS (asm/hyperv-tlfs.h); INTEGRATION.md §7 (KVM paravirtual clock, CPUID stability) |
| HV_X64_MSR_APIC_FREQUENCY | 0x40000023 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" / "Timer devices": a host-derived APIC timer frequency would let the guest correlate V-time with real time; no Hyper-V leaves are enumerated, so the MSR does not exist — #GP. | linux-6.18.35 arch/x86/kvm/x86.c (emulated_msrs_all); Hyper-V TLFS (asm/hyperv-tlfs.h); INTEGRATION.md §7 (KVM paravirtual clock, Timer devices) |
| HV_X64_MSR_REENLIGHTENMENT_CONTROL | 0x40000106 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock": reenlightenment is migration-driven host-real-time TSC notification machinery with no deterministic analog; not enumerated by the frozen CPUID model — #GP. | linux-6.18.35 arch/x86/kvm/x86.c (emulated_msrs_all); Hyper-V TLFS (asm/hyperv-tlfs.h); INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_TSC_EMULATION_CONTROL | 0x40000107 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock": Hyper-V TSC-emulation toggling would hand the guest a second, host-coupled TSC control plane beside VClock; not enumerated — #GP. | linux-6.18.35 arch/x86/kvm/x86.c (emulated_msrs_all); Hyper-V TLFS (asm/hyperv-tlfs.h); INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_TSC_EMULATION_STATUS | 0x40000108 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock": emulation-status readback reflects host migration state, which is nondeterministic across runs; not enumerated — #GP. | linux-6.18.35 arch/x86/kvm/x86.c (emulated_msrs_all); Hyper-V TLFS (asm/hyperv-tlfs.h); INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_TSC_INVARIANT_CONTROL | 0x40000118 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock": invariant-TSC enlightenment control is host TSC policy surface; the guest sees invariant TSC only via the frozen CPUID model, never via Hyper-V MSRs — #GP. | linux-6.18.35 arch/x86/kvm/x86.c (emulated_msrs_all); Hyper-V TLFS (asm/hyperv-tlfs.h); INTEGRATION.md §7 (KVM paravirtual clock, CPUID stability) |
| MSR_IA32_TSC_ADJUST | 0x3b | emulate-vtime | emulate-vtime | Closes §7 "TSC plumbing": the SDM coherence rule (a write of delta to TSC_ADJUST also shifts IA32_TSC by delta) is satisfied entirely inside VClock — the adjust value and the rebased tsc_base both live in vm_state, with no host TSC involvement in either direction. **Hashed formula ids (§6): read `vclock.tsc_adjust`, write `vclock.tsc_adjust.write`** (offset shifts by `(new−old)`; no TSC-deadline timers to recompute — 0x6e0 deny-gp). | linux-6.18.35 arch/x86/kvm/x86.c (emulated_msrs_all); arch/x86/include/asm/msr-index.h; Intel SDM Vol.3B §17.17.3 (TSC_ADJUST coherence) + Vol.4 Table 2-2; INTEGRATION.md §7 (TSC plumbing) + §4 (vm_state) |
| MSR_IA32_TSC_DEADLINE | 0x6e0 | deny-gp | deny-gp | (round-7: **deny-gp**, was emulate-timerqueue; round-8: enforcement is **backend-dependent**, §1) Closes §7 "Timer devices": **TSC-deadline mode is hidden** (CPUID.1:ECX[24]=0, §2). **Enforcement is backend-dependent (§1):** the same in-kernel WRMSR **fastpath** that makes emulation impossible — under `KVM_IRQCHIP_NONE`, `vmx.c handle_fastpath_wrmsr` services `WRMSR 0x6e0` **before** the MSR filter (the TSC_DEADLINE case lacks the `lapic_in_kernel` bail the x2APIC-ICR case has), and `kvm_set_lapic_tscdeadline_msr` no-ops with no in-kernel apic — also means the `deny-gp`'s logged #GP **cannot be delivered on stock KVM**. So `deny-gp` holds under the patched-KVM/direct-VMX backend; under stock KVM it **degrades to a silent swallow for an out-of-scope adversarial guest**. Determinism-safe in scope: the **cooperative guest never writes 0x6e0** (CPUID[24]=0). Nothing is lost: the LAPIC timer is the xAPIC LVT **one-shot/periodic** MMIO (TMICT 0x380, §5) on V-time. Aligns with Ruling R1 (PR #21). | linux-6.18.35 arch/x86/kvm/vmx/vmx.c (handle_fastpath_wrmsr runs before MSR filter), arch/x86/kvm/x86.c (TSC_DEADLINE fastpath lacks lapic_in_kernel bail), arch/x86/kvm/lapic.c (kvm_set_lapic_tscdeadline_msr no-op w/o apic); §1 (backend dependency) + [question] Backend; §2 leaf-1 ECX[24]=0; §5 (xAPIC LVT timer); Ruling R1 (PR #21) |
| MSR_AMD64_TSC_RATIO | 0xc0000104 | deny-gp | deny-gp | Architectural, and closes §7 "TSC plumbing" (offset/scaling must never let host TSC reach the guest): TSC scaling is hypervisor-side machinery, only architecturally present when CPUID 8000_000AH EDX[4] (TscRateMsr) is set, which the frozen CPUID model does not set — #GP. | linux-6.18.35 arch/x86/kvm/x86.c (emulated_msrs_all); arch/x86/include/asm/msr-index.h; AMD APM Vol.2 §15.30.5 (TSC ratio MSR); INTEGRATION.md §7 (TSC plumbing, CPUID stability) |

### 3.4 Class `timing-instr` — user-wait timing instructions (WAITPKG)

This class covers the MSR control surface of the WAITPKG user-wait instructions
(UMWAIT/TPAUSE/UMONITOR), whose only MSR is `IA32_UMWAIT_CONTROL` (0xE1): it caps the
maximum wait of UMWAIT/TPAUSE in TSC quanta and gates the C0.2 sleep state, i.e. it
configures instructions that block until a deadline measured against the *real* TSC —
a direct real-time dependence the V-time design cannot tolerate. Match rule: every name
matching `MSR_IA32_UMWAIT_CONTROL*` in `arch/x86/include/asm/msr-index.h` at v6.18.35
(one MSR; the remaining `MSR_IA32_UMWAIT_CONTROL_*` defines are bit-field masks of it).
The frozen CPUID model hides WAITPKG (CPUID.7,0:ECX[5] = 0) per RESEARCH.md principle 5
("no waitpkg — control via CPUID filtering") and the contract's instruction table makes
UMWAIT/TPAUSE/UMONITOR #UD, so the MSR is denied in both directions; #GP on access is
also the architecturally mandated behavior when WAITPKG is not enumerated, so a correct
guest never touches it and any access is a loud, logged filter exit.

| MSR | Index | Read | Write | Rationale | Citation |
|---|---|---|---|---|---|
| MSR_IA32_UMWAIT_CONTROL | 0xE1 | deny-gp | deny-gp | Closes §7 TSC-plumbing vector: UMWAIT/TPAUSE wait on real-TSC deadlines bounded by this MSR; WAITPKG hidden in the frozen CPUID model, and #GP matches architectural behavior with CPUID.7,0:ECX[5]=0 | Intel SDM Vol. 4 Table 2-2 (IA32_UMWAIT_CONTROL, E1H) and Vol. 2 UMWAIT/TPAUSE; Linux v6.18.35 `arch/x86/kvm/x86.c` `msrs_to_save_base` (WAITPKG-gated) and `arch/x86/include/asm/msr-index.h:102`; lwn.net/Articles/791668; RESEARCH.md §principle 5 |

### 3.5 Class `power-thermal` — power/frequency/thermal/turbo/energy

The `power-thermal` class is INTEGRATION.md §7's "Power/frequency" vector ("APERF/MPERF,
MPERF-adjacent, thermal/turbo MSRs — deny") expanded into a stated, mechanically checkable
match rule, plus the energy/idle-residency/hardware-feedback side channels that ride the
same bus: every MSR here either reads back real host physics (die temperature, consumed
energy, actual-vs-reference frequency, productive cycles, wall-clock idle residency,
Thread-Director feedback tables) or lets the guest *change* real host physics (P-state
requests, clock modulation, power limits, C-state policy, HWP requests) — both directions
are fatal to determinism, so **every row except the cross-referenced `MSR_PLATFORM_INFO`
is `deny-gp` for both reads and writes**: the access exits to userspace via
`KVM_X86_SET_MSR_FILTER` + `KVM_CAP_X86_USER_SPACE_MSR` (`KVM_MSR_EXIT_REASON_FILTER`),
is logged with MSR index and RIP, and only then is #GP injected — never a silent
passthrough, and deliberately #GP rather than a fixed zero because Linux probes these via
`rdmsr_safe` behind CPUID feature gates that the frozen model clears (CPUID.6:EAX[0]
digital thermal sensor, EAX[7..11] HWP, EAX[19] HFI, EAX[23] Thread Director,
CPUID.6:ECX[0] APERF/MPERF hardware-coordination feedback, ECX[3] EPB, CPUID.7.1:EAX[22]
HRESET, CPUID.1:EDX[22]/ECX[8] TM/TM2 — leaf 6 is otherwise zeroed; only the CPUID-model
fragment's ARAT bit, CPUID.6:EAX[2], may be set), so #GP is the architecturally consistent
answer; as defense in depth the task-04 pinned guest config leaves `CONFIG_CPU_FREQ`
unset and hides MWAIT so `intel_pstate`/`intel_idle` never bind. The RAPL energy counters
are denied as a family: they are a proven physical side channel (Platypus) and
monotonically reveal real work and real time done by the host. Class match rule (all
kernel citations at Linux tag v6.18.35, cross-checked against
`guest/linux/versions.lock` KERNEL_VERSION=6.18.35): the union of (a) exact names
{`MSR_IA32_APERF`, `MSR_IA32_MPERF`, `MSR_IA32_PERF_CTL`, `MSR_IA32_PERF_STATUS`,
`MSR_PLATFORM_INFO`, `MSR_PM_ENABLE`, `MSR_IA32_TEMPERATURE_TARGET`,
`MSR_IA32_ENERGY_PERF_BIAS`, `MSR_CORE_PERF_LIMIT_REASONS`}; (b) prefixes
{`MSR_IA32_THERM_*`, `MSR_IA32_PACKAGE_THERM_*`, `MSR_THERM2_*`, `MSR_HWP_*`}; (c)
substring {`*TURBO_RATIO_LIMIT*`}; (d) exact-name additions for the energy/idle/feedback
side channels {`MSR_IA32_POWER_CTL` (also in `x86.c:emulated_msrs_all`, i.e. reference-set
clause (a) of task 06 §3), `MSR_PKG_CST_CONFIG_CONTROL`, `MSR_RAPL_POWER_UNIT`,
`MSR_PKG_POWER_LIMIT`, `MSR_PKG_ENERGY_STATUS`, `MSR_DRAM_ENERGY_STATUS`,
`MSR_PP0_ENERGY_STATUS`, `MSR_PP1_ENERGY_STATUS`, `MSR_PLATFORM_ENERGY_STATUS`,
`MSR_PPERF`, `MSR_PERF_LIMIT_REASONS`, `MSR_PKG_C2_RESIDENCY`, `MSR_PKG_C3_RESIDENCY`,
`MSR_PKG_C6_RESIDENCY`, `MSR_PKG_C7_RESIDENCY`/`MSR_ATOM_PKG_C6_RESIDENCY`,
`MSR_CORE_C3_RESIDENCY`, `MSR_CORE_C6_RESIDENCY`, `MSR_CORE_C7_RESIDENCY`,
`MSR_KNL_CORE_C6_RESIDENCY`, `MSR_PKG_C8_RESIDENCY`, `MSR_PKG_C9_RESIDENCY`,
`MSR_PKG_C10_RESIDENCY`, `MSR_IA32_HW_FEEDBACK_PTR`, `MSR_IA32_HW_FEEDBACK_CONFIG`}, all
of (a)–(d) resolved against `arch/x86/include/asm/msr-index.h` at the pinned tag; plus
(e) two SDM/ISE-documented blocks with no `msr-index.h` define at this tag, listed
explicitly so the Thread-Director surface is visibly closed (the default-deny catch-all
covers them regardless): IA32_THREAD_FEEDBACK_CHAR/IA32_HW_FEEDBACK_CHAR (0x17d2–0x17d3,
inside the 0x17d0–0x17d4 range row) and IA32_HRESET_ENABLE (0x17da). `MSR_PLATFORM_INFO`
matches rule (a) but its normative row is owned by the `boot-baseline` class (§3.14); its
duplicate row here was removed at assembly (merge note below the table). Note one naming correction versus upstream
notes: 0x64f is `MSR_PERF_LIMIT_REASONS` in msr-index.h at this tag —
`MSR_CORE_PERF_LIMIT_REASONS` is 0x690. Column grammar: `Read`/`Write` are drawn verbatim
from the task-06 §3 disposition vocabulary; `Rationale` is one line beginning with the §7
leak vector it closes (`§7 Power/frequency`); kernel citations are `file:line` at
v6.18.35.

| MSR | Index | Read | Write | Rationale | Citation |
|---|---|---|---|---|---|
| MSR_PKG_CST_CONFIG_CONTROL | 0xe2 | deny-gp | deny-gp | §7 Power/frequency: package C-state limit/demotion config — host idle policy is real-time state, and a guest write would change real host idle behavior; guest has no C-states (MWAIT hidden, HLT idle-skips per INTEGRATION.md §3) | msr-index.h:118; SDM Vol.4 model-specific MSR table |
| MSR_IA32_MPERF | 0xe7 | deny-gp | deny-gp | §7 Power/frequency: named verbatim in §7 — MPERF reference-cycle counter; APERF/MPERF ratio reveals real host frequency/utilization and reads are serializing on real counters; #GP not zero — Linux probes via rdmsr_safe only when CPUID.6:ECX[0]=1 (hidden) | msr-index.h:960; SDM Vol.3B 15.2; SDM Vol.4 Table 2-2; INTEGRATION.md §7 (Power/frequency) |
| MSR_IA32_APERF | 0xe8 | deny-gp | deny-gp | §7 Power/frequency: named verbatim in §7 — APERF actual-cycle counter; same canonical frequency/utilization side door as MPERF | msr-index.h:961; SDM Vol.3B 15.2; SDM Vol.4 Table 2-2; INTEGRATION.md §7 (Power/frequency) |
| MSR_IA32_PERF_STATUS | 0x198 | deny-gp | deny-gp | §7 Power/frequency: current P-state/voltage — real host frequency readout; pinned guest config leaves CONFIG_CPU_FREQ unset so no guest cpufreq driver issues unchecked reads | msr-index.h:950; SDM Vol.3B ch.15; SDM Vol.4 Table 2-2; guest/linux/config-fragment (CONFIG_CPU_FREQ unset) |
| MSR_IA32_PERF_CTL | 0x199 | deny-gp | deny-gp | §7 Power/frequency: P-state request — a guest write would change the real host clock, perturbing the V-time instrument itself | msr-index.h:951; SDM Vol.3B ch.15; SDM Vol.4 Table 2-2 |
| MSR_IA32_THERM_CONTROL | 0x19a | deny-gp | deny-gp | §7 Power/frequency: software clock-modulation duty cycle — guest write would throttle the real clock; read reflects host throttle policy | msr-index.h:963; SDM Vol.3B 15.8.3 |
| MSR_IA32_THERM_INTERRUPT | 0x19b | deny-gp | deny-gp | §7 Power/frequency: thermal interrupt thresholds keyed to real die temperature — both directions tie guest state to host physics | msr-index.h:964; SDM Vol.3B 15.8.2 |
| MSR_IA32_THERM_STATUS | 0x19c | deny-gp | deny-gp | §7 Power/frequency: real die temperature readout and sticky throttle log — classic host-physics side channel; CPUID.6:EAX[0] and CPUID.1:EDX[22] hidden, so #GP is architectural | msr-index.h:970; SDM Vol.3B 15.8.2; SDM Vol.4 Table 2-2 |
| MSR_THERM2_CTL | 0x19d | deny-gp | deny-gp | §7 Power/frequency: TM2 thermal-monitor control; matched by the MSR_THERM2_* prefix added to the class rule (the §7 example rule MSR_IA32_THERM_* alone would miss it) | msr-index.h:975; SDM Vol.4 model-specific MSR table |
| MSR_IA32_TEMPERATURE_TARGET | 0x1a2 | deny-gp | deny-gp | §7 Power/frequency: Tj target — host-identifying thermal constant; exact-name addition since the prefix rules miss it though it sits inside the thermal block | msr-index.h:981; SDM Vol.4 model-specific MSR table |
| MSR_TURBO_RATIO_LIMIT | 0x1ad | deny-gp | deny-gp | §7 Power/frequency: per-core-count max turbo ratios — host-identifying and frequency-policy state; frozen model advertises no turbo (PLATFORM_INFO turbo fields zeroed) | msr-index.h:255; SDM Vol.4 model-specific MSR table |
| MSR_TURBO_RATIO_LIMIT1 | 0x1ae | deny-gp | deny-gp | §7 Power/frequency: turbo ratio limits, higher core counts — same as 0x1ad | msr-index.h:256; SDM Vol.4 model-specific MSR table |
| MSR_TURBO_RATIO_LIMIT2 | 0x1af | deny-gp | deny-gp | §7 Power/frequency: turbo ratio limits, highest core counts — same as 0x1ad | msr-index.h:257; SDM Vol.4 model-specific MSR table |
| MSR_IA32_ENERGY_PERF_BIAS | 0x1b0 | deny-gp | deny-gp | §7 Power/frequency: EPB energy-vs-performance hint — guest write would change real host frequency/energy policy; CPUID.6:ECX[3] hidden so Linux never touches it; exact-name addition (sits in the thermal index range 0x19a–0x1b2) | msr-index.h:986; SDM Vol.3B 15.4.4; SDM Vol.4 Table 2-2 |
| MSR_IA32_PACKAGE_THERM_STATUS | 0x1b1 | deny-gp | deny-gp | §7 Power/frequency: package-level real temperature/throttle status — host-physics side channel | msr-index.h:994; SDM Vol.3B 15.8.4 |
| MSR_IA32_PACKAGE_THERM_INTERRUPT | 0x1b2 | deny-gp | deny-gp | §7 Power/frequency: package thermal interrupt thresholds — ties guest-visible interrupts to real die temperature | msr-index.h:1000; SDM Vol.3B 15.8.4 |
| MSR_IA32_POWER_CTL | 0x1fc | deny-gp | deny-gp | §7 Power/frequency: C1E-promotion / energy-efficiency enable bits — host idle/energy policy; in reference set via emulated_msrs_all; guest intel_idle never loads (MWAIT hidden), so nothing in the pinned guest reads it unchecked | x86.c:435 (emulated_msrs_all); msr-index.h:265; SDM Vol.4 model-specific MSR table |
| MSR_PKG_C3_RESIDENCY / MSR_PKG_C6_RESIDENCY | 0x3f8–0x3f9 | deny-gp | deny-gp | §7 Power/frequency: package C-state residency counters — real wall-clock idle time, directly breaking V-time | msr-index.h:437–438; turbostat.c; SDM Vol.4 model-specific MSR table |
| MSR_PKG_C7_RESIDENCY (= MSR_ATOM_PKG_C6_RESIDENCY) | 0x3fa | deny-gp | deny-gp | §7 Power/frequency: package C7 residency (Atom: alternate pkg-C6) — same real-idle-time leak; range extension so the 0x3f8–0x3fa block is contiguous | msr-index.h:439–440; turbostat.c |
| MSR_CORE_C3_RESIDENCY / MSR_CORE_C6_RESIDENCY / MSR_CORE_C7_RESIDENCY | 0x3fc–0x3fe | deny-gp | deny-gp | §7 Power/frequency: core C-state residency counters — directly reveal real idle time spent, breaking V-time | msr-index.h:441–443; turbostat.c; SDM Vol.4 model-specific MSR table |
| MSR_KNL_CORE_C6_RESIDENCY | 0x3ff | deny-gp | deny-gp | §7 Power/frequency: KNL variant of core C6 residency — same real-idle-time leak; range extension making 0x3fc–0x3ff contiguous | msr-index.h:444; turbostat.c |
| MSR_RAPL_POWER_UNIT | 0x606 | deny-gp | deny-gp | §7 Power/frequency: scaling units for all RAPL energy/power/time readouts — denied with the whole RAPL family (its presence invites energy probing) | msr-index.h:461; SDM Vol.3B 15.10.1; qemu.org/docs/master/specs/rapl-msr.html |
| MSR_PKG_C2_RESIDENCY | 0x60d | deny-gp | deny-gp | §7 Power/frequency: package C2 residency counter — real idle-time leak | msr-index.h:445; turbostat.c; arch/x86/events/intel/cstate.c |
| MSR_PKG_POWER_LIMIT | 0x610 | deny-gp | deny-gp | §7 Power/frequency: RAPL package power-limit/clamp config — observable (and guest-writable) real host power policy | msr-index.h:463; SDM Vol.3B 15.10.3; qemu.org rapl-msr spec |
| MSR_PKG_ENERGY_STATUS | 0x611 | deny-gp | deny-gp | §7 Power/frequency: package consumed-energy counter (~61 µJ units, ~1 ms update) — monotonically reveals real work/time done; proven physical side channel (Platypus) | msr-index.h:464; SDM Vol.3B 15.10.3; platypusattack.com; qemu.org rapl-msr spec; web.eece.maine.edu rapl-read.c |
| MSR_DRAM_ENERGY_STATUS | 0x619 | deny-gp | deny-gp | §7 Power/frequency: DRAM-domain energy counter — same energy side channel as PKG | msr-index.h:469; SDM Vol.3B 15.10.5; libmsr msr_rapl.c |
| MSR_PKG_C8_RESIDENCY / MSR_PKG_C9_RESIDENCY / MSR_PKG_C10_RESIDENCY | 0x630–0x632 | deny-gp | deny-gp | §7 Power/frequency: deep package C-state residency counters — real-time idle accounting | msr-index.h:446–448; arch/x86/events/intel/cstate.c; turbostat.c |
| MSR_PP0_ENERGY_STATUS | 0x639 | deny-gp | deny-gp | §7 Power/frequency: core/PP0-domain energy counter — same energy side channel as PKG | msr-index.h:474; SDM Vol.3B 15.10.4; libmsr msr_rapl.c |
| MSR_PP1_ENERGY_STATUS | 0x641 | deny-gp | deny-gp | §7 Power/frequency: PP1 (uncore/graphics) energy counter — energy side channel | msr-index.h:479; libmsr msr_rapl.c |
| MSR_PLATFORM_ENERGY_STATUS (PSYS) | 0x64d | deny-gp | deny-gp | §7 Power/frequency: whole-platform energy counter — energy side channel | msr-index.h:493; web.eece.maine.edu rapl-read.c |
| MSR_PPERF | 0x64e | deny-gp | deny-gp | §7 Power/frequency: productive-performance cycle counter (MPERF-adjacent) — reveals real productive time vs stalls | msr-index.h:536; SDM Vol.3B ch.15 (HWP); turbostat.c |
| MSR_PERF_LIMIT_REASONS | 0x64f | deny-gp | deny-gp | §7 Power/frequency: bitmap of why frequency was throttled (thermal/power/PROCHOT) — real-time host state; note: 0x64f is MSR_PERF_LIMIT_REASONS at this tag, not MSR_CORE_PERF_LIMIT_REASONS (that is 0x690) | msr-index.h:537; SDM Vol.4 model-specific MSR table |
| MSR_SECONDARY_TURBO_RATIO_LIMIT | 0x650 | deny-gp | deny-gp | §7 Power/frequency: secondary (e.g. E-core) turbo ratio table — host-identifying frequency policy; matched by the *TURBO_RATIO_LIMIT* substring rule | msr-index.h:494; SDM Vol.4 model-specific MSR table |
| MSR_CORE_PERF_LIMIT_REASONS | 0x690 | deny-gp | deny-gp | §7 Power/frequency: per-core frequency-limit-reason status — real-time throttle state; placed here, not pmu (the pmu rule is restricted to MSR_CORE_PERF_FIXED_*/MSR_CORE_PERF_GLOBAL_*) | msr-index.h:512; SDM Vol.4 model-specific MSR table |
| MSR_PM_ENABLE | 0x770 | deny-gp | deny-gp | §7 Power/frequency: IA32_PM_ENABLE gates the whole HWP range — HWP never exists for the guest (CPUID.6:EAX[7] hidden), so #GP is architectural | msr-index.h:538; SDM Vol.3B 15.4.2; SDM Vol.4 Table 2-2 |
| MSR_HWP_CAPABILITIES | 0x771 | deny-gp | deny-gp | §7 Power/frequency: HWP highest/guaranteed/efficient performance levels — host-identifying frequency capabilities | msr-index.h:539; SDM Vol.3B 15.4.3 |
| MSR_HWP_REQUEST_PKG | 0x772 | deny-gp | deny-gp | §7 Power/frequency: package-wide HWP request — guest write would steer real host frequency | msr-index.h:540; SDM Vol.3B 15.4.4 |
| MSR_HWP_INTERRUPT | 0x773 | deny-gp | deny-gp | §7 Power/frequency: HWP notification enables keyed to real frequency excursions | msr-index.h:541; SDM Vol.3B 15.4.6 |
| MSR_HWP_REQUEST | 0x774 | deny-gp | deny-gp | §7 Power/frequency: per-logical-CPU HWP min/max/desired/EPP request — guest write would steer real host frequency | msr-index.h:542; SDM Vol.3B 15.4.4; SDM Vol.4 Table 2-2 |
| MSR_HWP_STATUS | 0x777 | deny-gp | deny-gp | §7 Power/frequency: HWP excursion status — real frequency-delivery events; note the gap: 0x775/0x776 (IA32_PECI_HWP_REQUEST_INFO etc.) have no msr-index.h define at this tag and fall to the default-deny catch-all | msr-index.h:543; SDM Vol.3B 15.4.5 |
| IA32_HW_FEEDBACK_PTR / IA32_HW_FEEDBACK_CONFIG / IA32_THREAD_FEEDBACK_CHAR / IA32_HW_FEEDBACK_CHAR | 0x17d0–0x17d4 | deny-gp | deny-gp | §7 Power/frequency: Hardware Feedback Interface / Thread Director — per-package perf/efficiency table pointer+config and per-thread class feedback driven by real thermals and scheduling; whole block denied; only 0x17d0/0x17d1 have msr-index.h defines at this tag, 0x17d2–0x17d4 are SDM-architectural and also caught by the default-deny catch-all; CPUID.6:EAX[19]/EAX[23] hidden | msr-index.h:1265–1266; docs.kernel.org/arch/x86/intel-hfi; SDM Vol.3B 14.9 |
| IA32_HRESET_ENABLE | 0x17da | deny-gp | deny-gp | §7 Power/frequency: enables HRESET history-reset of uarch predictor/Thread-Director state — guest control over real uarch state; CPUID.7.1:EAX[22] hidden so #GP is architectural; no msr-index.h define at this tag — covered by the catch-all, row kept to make the Thread-Director surface visibly closed | Intel ISE ref. 843860; qemu-devel HRESET RFC; SDM Vol.3B 14.9 |

*Assembly merge note: the `MSR_PLATFORM_INFO` (0xce) row was removed from this table —
it is matched by this class's exact-name rule, but its single normative row lives in
class `boot-baseline` (§3.14): read `allow-fixed(0x0000000000001400)`, write `deny-gp`,
identical to the row removed here.*

[question] Adjacent non-matches left to the default-deny catch-all (denied-and-logged
either way; folding them into this class's match rule would only change
mechanical-checkability bookkeeping): MSR_TURBO_ACTIVATION_RATIO (0x64c, msr-index.h:491),
MSR_ATOM_CORE_TURBO_RATIOS/MSR_ATOM_CORE_TURBO_VIDS (0x66c/0x66d, msr-index.h:509–510)
and MSR_ATOM_CORE_RATIOS/VIDS (0x66a/0x66b), the RAPL config/limit/info/policy registers
(MSR_VR_CURRENT_CONFIG 0x601, MSR_PKG_PERF_STATUS 0x613, MSR_PKG_POWER_INFO 0x614,
MSR_DRAM_POWER_LIMIT/PERF_STATUS/POWER_INFO 0x618/0x61b/0x61c,
MSR_PP0_POWER_LIMIT/POLICY/PERF_STATUS 0x638/0x63a/0x63b, MSR_PP1_POWER_LIMIT/POLICY
0x640/0x642), the IRTL latency registers (0x60a–0x60c, 0x633–0x635), the C0-residency
and demotion family (0x658–0x65b, 0x660, 0x664, 0x668/0x669), and
MSR_GFX/RING_PERF_LIMIT_REASONS (0x6b0/0x6b1). Decide at merge whether to promote them to
explicit deny-gp rows in this class or leave them to the catch-all.

[question] IA32_HRESET_ENABLE (0x17da) and IA32_THREAD_FEEDBACK_CHAR/IA32_HW_FEEDBACK_CHAR
(0x17d2–0x17d3) have no `arch/x86/include/asm/msr-index.h` define at the pinned v6.18.35
tag, so — like MSR_CORE_THREAD_COUNT in the boot-baseline fragment — they fall outside the
strictly mechanical reference-set definition of task 06 §3. The rows are kept with
safe deny-gp/deny-gp dispositions; confirm at merge whether they stay as explicit rows
(recommended: they close the Thread-Director surface visibly) or are dropped to the
default-deny catch-all.

[question] MSR_PLATFORM_INFO (0xce) is matched by this class's exact-name rule but its
normative row (allow-fixed frozen-ratio read / deny-gp write, with the frozen constant
tied to the CPUID 0x15/0x16 model) is owned by the boot-baseline fragment; it is
duplicated here with identical dispositions. At merge, keep exactly one normative copy and
make the other a cross-reference so a value change cannot diverge.
**[resolved at assembly]** The §3.14 `boot-baseline` row is the single normative copy —
read `allow-fixed(0x0000000000001400)` (bits 15:8 = 0x14, ratio 20 = 2.0 GHz / 100 MHz
per §2's frozen frequencies), write `deny-gp`; this class retains only the
cross-reference.

### 3.6 Class `pmu` — performance monitoring (host-owned PMU)

The host owns the PMU, non-negotiably, because the PMU *is* the V-time instrument: vmm-core programs a guest-only retired-branch perf_event counter and uses PMC overflow plus single-step to land injections at exact V-times (PLAN.md Phase 2 and trap table "RDPMC → trap"; RESEARCH.md rr and XenTT rows; antithesis.com/blog/deterministic_hypervisor/). No vPMU is exposed to the guest, closing the INTEGRATION.md §7 "PMU" leak vector: CPUID leaf 0xA reports architectural perfmon version 0, CPUID.1:ECX[15] (PDCM) is hidden, and RDPMC exits via VMX RDPMC-exiting and is answered with #GP (see the instruction-disposition table). Consequently **every MSR in this class is `deny-gp` for both reads and writes**: a denied access exits to userspace via `KVM_X86_SET_MSR_FILTER` + `KVM_CAP_X86_USER_SPACE_MSR` (`KVM_MSR_EXIT_REASON_FILTER`), is logged with MSR index and RIP, and only then is #GP injected — never a silent passthrough or silent zero. This is also architecturally consistent: with perfmon version 0 and PDCM clear, real hardware #GPs on these accesses too. Class match rule (mechanically checkable, all kernel citations at Linux tag v6.18.35): the union of (a) every entry of `arch/x86/kvm/x86.c:msrs_to_save_pmu`, and (b) every name in `arch/x86/include/asm/msr-index.h` or `arch/x86/include/asm/perf_event.h` matching `MSR_CORE_PERF_*`, `MSR_ARCH_PERFMON_*`, `MSR_IA32_PMC0`..`MSR_IA32_PMC7`, `MSR_IA32_PMC_V6_*`, `MSR_PEBS_*`, `MSR_IA32_PEBS_ENABLE`, `MSR_IA32_DS_AREA`, `MSR_OFFCORE_RSP_*`, `MSR_RELOAD_*`, `MSR_K7_EVNTSEL*`, `MSR_K7_PERFCTR*`, `MSR_F15H_PERF_*`, or `MSR_AMD64_PERF_CNTR_GLOBAL_*`, plus the exact-name additions `MSR_PERF_METRICS` and `MSR_IA32_PERF_CAPABILITIES`, plus the SDM architectural ranges given as range rows below (in the source fragment, range rows deliberately overlapped the per-name rows from the KVM array with identical dispositions; the exact-coverage duplicates were collapsed at assembly to keep one row per index per §6's serialization rule — merge note below the table). Note: IA32_PERF_GLOBAL_STATUS_SET (0x391) and IA32_PERF_GLOBAL_INUSE (0x392) exist architecturally but have no `msr-index.h` define at this tag (only the AMD equivalent 0xc0000303 appears); the contract's default-deny catch-all covers them, and they are called out in the 0x38E–0x390 range row. Column grammar: `Read`/`Write` are drawn verbatim from the task-06 §3 disposition vocabulary; `Rationale` is one line beginning with the §7 leak vector it closes (`§7 PMU`).

| MSR | Index | Read | Write | Rationale | Citation |
|---|---|---|---|---|---|
| MSR_ARCH_PERFMON_FIXED_CTR0 | 0x309 | deny-gp | deny-gp | §7 PMU: fixed counter 0 (instructions retired) counts real uarch progress incl. host noise; host owns PMU | kvm/x86.c:msrs_to_save_pmu; msr-index.h (MSR_CORE_PERF_FIXED_CTR0); antithesis.com/blog/deterministic_hypervisor/; RESEARCH.md XenTT row |
| MSR_ARCH_PERFMON_FIXED_CTR1 | 0x30a | deny-gp | deny-gp | §7 PMU: fixed counter 1 (core cycles) leaks real frequency/time | kvm/x86.c:msrs_to_save_pmu; msr-index.h (MSR_CORE_PERF_FIXED_CTR1) |
| MSR_ARCH_PERFMON_FIXED_CTR0+2 | 0x30b | deny-gp | deny-gp | §7 PMU: fixed counter 2 (ref cycles) leaks wall-clock time directly; written as FIXED_CTR0+2 in the KVM array | kvm/x86.c:msrs_to_save_pmu; msr-index.h (MSR_CORE_PERF_FIXED_CTR2) |
| MSR_CORE_PERF_FIXED_CTR_CTRL | 0x38d | deny-gp | deny-gp | §7 PMU: fixed-counter control; a guest arm/disarm would contend with the host-owned PMU | kvm/x86.c:msrs_to_save_pmu; msr-index.h; SDM Vol3B 20.2.2 |
| MSR_CORE_PERF_GLOBAL_STATUS | 0x38e | deny-gp | deny-gp | §7 PMU: global status; overflow bits reflect real-time PMI timing of the host V-time counter | kvm/x86.c:msrs_to_save_pmu; msr-index.h |
| MSR_CORE_PERF_GLOBAL_CTRL | 0x38f | deny-gp | deny-gp | §7 PMU: global enable; the host V-time engine alone programs the PMU | kvm/x86.c:msrs_to_save_pmu; msr-index.h |
| MSR_IA32_PEBS_ENABLE | 0x3f1 | deny-gp | deny-gp | §7 PMU: PEBS enable; PEBS writes sample records to memory on real-time triggers — nondeterministic memory contents | kvm/x86.c:msrs_to_save_pmu; msr-index.h; SDM Vol3B 21.4 |
| MSR_IA32_DS_AREA | 0x600 | deny-gp | deny-gp | §7 PMU: debug-store area base; BTS/PEBS would scribble guest memory asynchronously to guest work | kvm/x86.c:msrs_to_save_pmu; msr-index.h; SDM Vol3B 17.4.9 / ch21 |
| MSR_PEBS_DATA_CFG | 0x3f2 | deny-gp | deny-gp | §7 PMU: PEBS data config; PEBS denied wholesale with the rest of the host-owned PMU | kvm/x86.c:msrs_to_save_pmu; msr-index.h |
| MSR_ARCH_PERFMON_PERFCTR0 | 0xc1 | deny-gp | deny-gp | §7 PMU: GP counter 0; cycle/event counts destroy determinism if guest-readable | kvm/x86.c:msrs_to_save_pmu; asm/perf_event.h |
| MSR_ARCH_PERFMON_PERFCTR1 | 0xc2 | deny-gp | deny-gp | §7 PMU: GP counter 1; same as PERFCTR0 | kvm/x86.c:msrs_to_save_pmu; asm/perf_event.h |
| MSR_ARCH_PERFMON_PERFCTR0+2 | 0xc3 | deny-gp | deny-gp | §7 PMU: GP counter 2 | kvm/x86.c:msrs_to_save_pmu |
| MSR_ARCH_PERFMON_PERFCTR0+3 | 0xc4 | deny-gp | deny-gp | §7 PMU: GP counter 3 | kvm/x86.c:msrs_to_save_pmu |
| MSR_ARCH_PERFMON_PERFCTR0+4 | 0xc5 | deny-gp | deny-gp | §7 PMU: GP counter 4 | kvm/x86.c:msrs_to_save_pmu |
| MSR_ARCH_PERFMON_PERFCTR0+5 | 0xc6 | deny-gp | deny-gp | §7 PMU: GP counter 5 | kvm/x86.c:msrs_to_save_pmu |
| MSR_ARCH_PERFMON_PERFCTR0+6 | 0xc7 | deny-gp | deny-gp | §7 PMU: GP counter 6 | kvm/x86.c:msrs_to_save_pmu |
| MSR_ARCH_PERFMON_PERFCTR0+7 | 0xc8 | deny-gp | deny-gp | §7 PMU: GP counter 7; matches KVM_MAX_NR_INTEL_GP_COUNTERS | kvm/x86.c:msrs_to_save_pmu |
| MSR_ARCH_PERFMON_EVENTSEL0 | 0x186 | deny-gp | deny-gp | §7 PMU: event select 0; arming a counter would contend with the V-time retired-branch counter (cf. rr's IN_TX/IN_TXCP eventsel handling) | kvm/x86.c:msrs_to_save_pmu; asm/perf_event.h; rr src/PerfCounters.cc (~355-390, ~1127-1190) |
| MSR_ARCH_PERFMON_EVENTSEL1 | 0x187 | deny-gp | deny-gp | §7 PMU: event select 1 | kvm/x86.c:msrs_to_save_pmu; asm/perf_event.h |
| MSR_ARCH_PERFMON_EVENTSEL0+2 | 0x188 | deny-gp | deny-gp | §7 PMU: event select 2 | kvm/x86.c:msrs_to_save_pmu |
| MSR_ARCH_PERFMON_EVENTSEL0+3 | 0x189 | deny-gp | deny-gp | §7 PMU: event select 3 | kvm/x86.c:msrs_to_save_pmu |
| MSR_ARCH_PERFMON_EVENTSEL0+4 | 0x18a | deny-gp | deny-gp | §7 PMU: event select 4 | kvm/x86.c:msrs_to_save_pmu |
| MSR_ARCH_PERFMON_EVENTSEL0+5 | 0x18b | deny-gp | deny-gp | §7 PMU: event select 5 | kvm/x86.c:msrs_to_save_pmu |
| MSR_ARCH_PERFMON_EVENTSEL0+6 | 0x18c | deny-gp | deny-gp | §7 PMU: event select 6 | kvm/x86.c:msrs_to_save_pmu |
| MSR_ARCH_PERFMON_EVENTSEL0+7 | 0x18d | deny-gp | deny-gp | §7 PMU: event select 7 | kvm/x86.c:msrs_to_save_pmu |
| MSR_K7_EVNTSEL0 | 0xc0010000 | deny-gp | deny-gp | §7 PMU: AMD K7 event select 0; baseline is Intel-only (PLAN.md Decision 0), AMD PMU never exposed | kvm/x86.c:msrs_to_save_pmu; msr-index.h |
| MSR_K7_EVNTSEL1 | 0xc0010001 | deny-gp | deny-gp | §7 PMU: AMD K7 event select 1; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu |
| MSR_K7_EVNTSEL2 | 0xc0010002 | deny-gp | deny-gp | §7 PMU: AMD K7 event select 2; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu |
| MSR_K7_EVNTSEL3 | 0xc0010003 | deny-gp | deny-gp | §7 PMU: AMD K7 event select 3; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu |
| MSR_K7_PERFCTR0 | 0xc0010004 | deny-gp | deny-gp | §7 PMU: AMD K7 perf counter 0; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu; msr-index.h |
| MSR_K7_PERFCTR1 | 0xc0010005 | deny-gp | deny-gp | §7 PMU: AMD K7 perf counter 1; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu |
| MSR_K7_PERFCTR2 | 0xc0010006 | deny-gp | deny-gp | §7 PMU: AMD K7 perf counter 2; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu |
| MSR_K7_PERFCTR3 | 0xc0010007 | deny-gp | deny-gp | §7 PMU: AMD K7 perf counter 3; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu |
| MSR_F15H_PERF_CTL0 | 0xc0010200 | deny-gp | deny-gp | §7 PMU: AMD F15h perf control 0; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu; msr-index.h |
| MSR_F15H_PERF_CTL1 | 0xc0010202 | deny-gp | deny-gp | §7 PMU: AMD F15h perf control 1 (stride 2); Intel-only baseline | kvm/x86.c:msrs_to_save_pmu |
| MSR_F15H_PERF_CTL2 | 0xc0010204 | deny-gp | deny-gp | §7 PMU: AMD F15h perf control 2; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu |
| MSR_F15H_PERF_CTL3 | 0xc0010206 | deny-gp | deny-gp | §7 PMU: AMD F15h perf control 3; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu |
| MSR_F15H_PERF_CTL4 | 0xc0010208 | deny-gp | deny-gp | §7 PMU: AMD F15h perf control 4; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu |
| MSR_F15H_PERF_CTL5 | 0xc001020a | deny-gp | deny-gp | §7 PMU: AMD F15h perf control 5; matches KVM_MAX_NR_AMD_GP_COUNTERS; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu |
| MSR_F15H_PERF_CTR0 | 0xc0010201 | deny-gp | deny-gp | §7 PMU: AMD F15h perf counter 0; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu; msr-index.h |
| MSR_F15H_PERF_CTR1 | 0xc0010203 | deny-gp | deny-gp | §7 PMU: AMD F15h perf counter 1; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu |
| MSR_F15H_PERF_CTR2 | 0xc0010205 | deny-gp | deny-gp | §7 PMU: AMD F15h perf counter 2; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu |
| MSR_F15H_PERF_CTR3 | 0xc0010207 | deny-gp | deny-gp | §7 PMU: AMD F15h perf counter 3; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu |
| MSR_F15H_PERF_CTR4 | 0xc0010209 | deny-gp | deny-gp | §7 PMU: AMD F15h perf counter 4; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu |
| MSR_F15H_PERF_CTR5 | 0xc001020b | deny-gp | deny-gp | §7 PMU: AMD F15h perf counter 5; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu |
| MSR_AMD64_PERF_CNTR_GLOBAL_CTL | 0xc0000301 | deny-gp | deny-gp | §7 PMU: AMD PerfMonV2 global control; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu; msr-index.h |
| MSR_AMD64_PERF_CNTR_GLOBAL_STATUS | 0xc0000300 | deny-gp | deny-gp | §7 PMU: AMD PerfMonV2 global status; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu; msr-index.h |
| MSR_AMD64_PERF_CNTR_GLOBAL_STATUS_CLR | 0xc0000302 | deny-gp | deny-gp | §7 PMU: AMD PerfMonV2 global status clear; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu; msr-index.h |
| MSR_AMD64_PERF_CNTR_GLOBAL_STATUS_SET | 0xc0000303 | deny-gp | deny-gp | §7 PMU: AMD PerfMonV2 global status set; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu; msr-index.h |
| MSR_IA32_PERF_CAPABILITIES | 0x345 | deny-gp | deny-gp | §7 PMU: enumerates LBR format/PEBS capability; PDCM (CPUID.1:ECX[15]) is hidden, so #GP on read is architecturally consistent; the MSR is read-only so writes #GP on real hardware too | kvm/x86.c:emulated_msrs_all + msr_based_features_all_except_vmx; msr-index.h; SDM Vol3B ch20; SDM Vol4 Table 2-2; KVM api.rst KVM_GET_MSR_FEATURE_INDEX_LIST (kernel.org) |
| MSR_OFFCORE_RSP_0 | 0x1a6 | deny-gp | deny-gp | §7 PMU: offcore-response aux event config for the host-owned PMU | msr-index.h |
| MSR_OFFCORE_RSP_1 | 0x1a7 | deny-gp | deny-gp | §7 PMU: offcore-response aux event config for the host-owned PMU | msr-index.h |
| MSR_CORE_PERF_FIXED_CTR3 | 0x30c | deny-gp | deny-gp | §7 PMU: fixed counter 3 (topdown slots); real-slot counts leak time | msr-index.h |
| MSR_PERF_METRICS | 0x329 | deny-gp | deny-gp | §7 PMU: topdown metrics derived from real slot counts; matched by exact-name addition to the class rule | msr-index.h |
| MSR_CORE_PERF_GLOBAL_OVF_CTRL | 0x390 | deny-gp | deny-gp | §7 PMU: IA32_PERF_GLOBAL_OVF_CTRL / GLOBAL_STATUS_RESET; note IA32_PERF_GLOBAL_STATUS_SET 0x391 and GLOBAL_INUSE 0x392 are architectural but NOT defined in msr-index.h at this tag (only AMD 0xc0000303 appears) — the default-deny catch-all still covers 0x391/0x392 | msr-index.h; SDM Vol3B 20.2.4 |
| MSR_PEBS_LD_LAT_THRESHOLD | 0x3f6 | deny-gp | deny-gp | §7 PMU: PEBS load-latency threshold; PEBS denied wholesale | msr-index.h |
| MSR_PEBS_FRONTEND | 0x3f7 | deny-gp | deny-gp | §7 PMU: PEBS frontend event config; PEBS denied wholesale | msr-index.h |
| MSR_RELOAD_FIXED_CTR0 | 0x1309 | deny-gp | deny-gp | §7 PMU: adaptive-PEBS reload base for fixed counters | msr-index.h |
| MSR_RELOAD_PMC0 | 0x14c1 | deny-gp | deny-gp | §7 PMU: adaptive-PEBS reload base for GP counters | msr-index.h |
| MSR_IA32_PMC_V6_GP0_CTR | 0x1900 | deny-gp | deny-gp | §7 PMU: arch-perfmon v6 GP counter base; per-counter stride MSR_IA32_PMC_V6_STEP=4, so the whole 0x1900+4N bank is denied by the catch-all | msr-index.h |
| MSR_IA32_PMC_V6_GP0_CFG_A | 0x1901 | deny-gp | deny-gp | §7 PMU: arch-perfmon v6 GP config A (stride 4) | msr-index.h |
| MSR_IA32_PMC_V6_GP0_CFG_B | 0x1902 | deny-gp | deny-gp | §7 PMU: arch-perfmon v6 GP config B (stride 4) | msr-index.h |
| MSR_IA32_PMC_V6_GP0_CFG_C | 0x1903 | deny-gp | deny-gp | §7 PMU: arch-perfmon v6 GP config C (stride 4) | msr-index.h |
| MSR_IA32_PMC_V6_FX0_CTR | 0x1980 | deny-gp | deny-gp | §7 PMU: arch-perfmon v6 fixed counter base (stride 4) | msr-index.h |
| MSR_IA32_PMC_V6_FX0_CFG_B | 0x1982 | deny-gp | deny-gp | §7 PMU: arch-perfmon v6 fixed config B (stride 4) | msr-index.h |
| MSR_IA32_PMC_V6_FX0_CFG_C | 0x1983 | deny-gp | deny-gp | §7 PMU: arch-perfmon v6 fixed config C (stride 4) | msr-index.h |
| IA32_A_PMC0-7 | 0x4C1-0x4C8 | deny-gp | deny-gp | §7 PMU: full-width aliases of the GP PMCs (when PERF_CAPABILITIES.FW_WRITE); same nondeterminism | SDM Vol3B 20.2.4 |
| MSR_UNC_PERF_FIXED_CTRL/CTR, MSR_UNC_CBO_CONFIG | 0x394-0x396 | deny-gp | deny-gp | §7 PMU: client uncore fixed counter control/counter and CBo config; uncore counts cross-core and host activity — pure nondeterminism; names not in msr-index.h at this tag (defined in arch/x86/events/intel/uncore_snb.c), covered here and by the catch-all | SDM Vol4 Table 2-2; linux arch/x86/events/intel/uncore_snb.c |

*Assembly merge note: the following source-fragment rows duplicated other rows of this
table index-for-index with identical `deny-gp`/`deny-gp` dispositions and were
collapsed: `IA32_PMC0-7` (0xC1–0xC8), `IA32_PERFEVTSEL0-7` (0x186–0x18D),
`IA32_FIXED_CTR0-2` (0x309–0x30B), and `IA32_PERF_GLOBAL_STATUS/CTRL/OVF_CTRL`
(0x38E–0x390) into the per-name KVM-array rows; the single-index `MSR_IA32_PMC0`
(0x4C1) row into the `IA32_A_PMC0-7` full-width-alias range row (0x4C1–0x4C8). The
SDM architectural ranges remain walkable from the per-name rows plus the retained
range rows; no index coverage or disposition changed.*

### 3.7 Class `debug-lbr` — debug store & last-branch records

Debug-store and last-branch-record surface: IA32_DEBUGCTL and everything it arms (legacy
LBR stacks, LBR_SELECT/TOS, LBR_INFO, architectural LBR, last-exception records, and the
silicon-debug interface). Match rule against `arch/x86/include/asm/msr-index.h` @ v6.18.35:
every name matching `MSR_LBR_*`, `MSR_ARCH_LBR_*`, `MSR_IA32_DEBUGCTLMSR`,
`MSR_IA32_LASTBRANCH*`, or `MSR_IA32_LASTINT*`, plus the SDM Vol 4 Table 2-2 ranges those
bases expand to (legacy LBR stacks 0x680-0x69F / 0x6C0-0x6DF, LBR_INFO 0xDC0-0xDDF,
architectural LBR 0x1200-0x121F / 0x1500-0x151F / 0x1600-0x161F) and IA32_DEBUG_INTERFACE
(0xC80). Blanket policy: **deny-gp on both directions for every entry**, logged loudly per
contract §1. Rationale for the class: INTEGRATION.md §7's PMU vector says the host owns the
PMU and no vPMU is exposed — KVM's LBR virtualization is vPMU-gated and its record format
is host-model-dependent (IA32_PERF_CAPABILITIES[5:0]), so any allow would leak host
identity; LBR_INFO entries additionally carry cycle counts since the last branch, a covert
timebase that bypasses the RDTSC trap; and branch-history records are recent control-flow
state that is either stale host data or a replay-divergence channel. Because the guest can
never establish state in these MSRs, none are captured in `vm_state` (INTEGRATION.md §4) —
KVM listing DEBUGCTL/LASTBRANCH/LASTINT in `msrs_to_save_base` governs host-side
save/restore ioctls only, which the MSR filter does not affect. Range rows below subsume
the base-register rows from `msr-index.h` (the file defines stack bases only; depth is
model-dependent, so the deny covers the maximal architectural span and the §1 default-deny
filter catches any wider model-specific layout). For gate-5 consistency the frozen CPUID
model must hide Arch LBR (CPUID.(EAX=07H,ECX=0):EDX[19] = 0, leaf 0x1C absent/zero) and
report no LBR format (no IA32_PERF_CAPABILITIES exposure), so no exposed feature bit
implies these MSRs.

| MSR | Index | Read | Write | Rationale | Citation |
|---|---|---|---|---|---|
| MSR_IA32_DEBUGCTLMSR | 0x1D9 | deny-gp | deny-gp | §7 PMU: DEBUGCTL arms LBR/BTS/BTF and freeze-on-PMI; host owns PMU and branch tracing, LBR format is host-dependent. | arch/x86/kvm/x86.c:msrs_to_save_base @ v6.18.35; msr-index.h @ v6.18.35; SDM Vol 3B §17.4.1 |
| MSR_LBR_SELECT | 0x1C8 | deny-gp | deny-gp | §7 PMU: LBR filtering control for a facility the guest must not see; no vPMU/LBR is virtualized. | msr-index.h @ v6.18.35; SDM Vol 3B §17.4.2 |
| MSR_LBR_TOS | 0x1C9 | deny-gp | deny-gp | §7 PMU: top-of-stack pointer would expose host LBR depth and rotation state. | msr-index.h @ v6.18.35; SDM Vol 3B §17.4.3 |
| MSR_IA32_LASTBRANCHFROMIP | 0x1DB | deny-gp | deny-gp | §7 PMU: legacy last-branch source IP leaks host/stale control-flow history. | arch/x86/kvm/x86.c:msrs_to_save_base @ v6.18.35; msr-index.h @ v6.18.35 |
| MSR_IA32_LASTBRANCHTOIP | 0x1DC | deny-gp | deny-gp | §7 PMU: legacy last-branch target IP, same channel as FROM_IP. | arch/x86/kvm/x86.c:msrs_to_save_base @ v6.18.35; msr-index.h @ v6.18.35 |
| MSR_IA32_LASTINTFROMIP | 0x1DD | deny-gp | deny-gp | §7 PMU: last-interrupt/exception source IP is asynchronous-event history, a replay-divergence channel. | arch/x86/kvm/x86.c:msrs_to_save_base @ v6.18.35; msr-index.h @ v6.18.35 |
| MSR_IA32_LASTINTTOIP | 0x1DE | deny-gp | deny-gp | §7 PMU: last-interrupt/exception target IP, paired with LASTINTFROMIP. | arch/x86/kvm/x86.c:msrs_to_save_base @ v6.18.35; msr-index.h @ v6.18.35 |
| MSR_LBR_CORE_FROM | 0x40 | deny-gp | deny-gp | §7 PMU: base of legacy Core LBR from-stack (depth model-dependent; file defines base only); branch history denied. | msr-index.h @ v6.18.35 |
| MSR_LBR_CORE_TO | 0x60 | deny-gp | deny-gp | §7 PMU: base of legacy Core LBR to-stack; branch history denied. | msr-index.h @ v6.18.35 |
| MSR_LASTBRANCH_0-15_FROM_IP | 0x680-0x68F | deny-gp | deny-gp | §7 PMU: legacy LBR-stack source IPs (Skylake layout) — recent control-flow history; subsumes the MSR_LBR_NHM_FROM base row. | SDM Vol 3B §17.4.8 |
| MSR_LASTBRANCH_0-15_TO_IP | 0x6C0-0x6CF | deny-gp | deny-gp | §7 PMU: legacy LBR-stack target IPs paired with FROM_IP; subsumes the MSR_LBR_NHM_TO base row. | SDM Vol 3B §17.4.8 |
| MSR_LBR_INFO_0 | 0xDC0-0xDDF | deny-gp | deny-gp | §7 PMU + TSC plumbing: per-entry LBR info carries cycle counts since last branch — a covert timebase bypassing the RDTSC trap; range per in-file comment "... 0xddf for _31". | msr-index.h @ v6.18.35; SDM Vol 3B §17.4.8.1 |
| MSR_ARCH_LBR_CTL | 0x14CE | deny-gp | deny-gp | §7 PMU: architectural-LBR enable/filter control; the facility is hidden (CPUID.7.0:EDX[19] = 0), so control access must #GP. | msr-index.h @ v6.18.35; SDM Vol 4 Table 2-2; arch/x86/events/intel/lbr.c |
| MSR_ARCH_LBR_DEPTH | 0x14CF | deny-gp | deny-gp | §7 PMU: arch-LBR depth select; reading would reveal host-supported depths (CPUID 0x1C), which is hidden host identity. | msr-index.h @ v6.18.35; SDM Vol 4 Table 2-2 |
| IA32_LBR_x_FROM_IP (architectural LBR) | 0x1500-0x151F | deny-gp | deny-gp | §7 PMU: architectural LBR entry source IPs; subsumes the MSR_ARCH_LBR_FROM_0 base row. | SDM Vol 3B §17.5; SDM Vol 4 Table 2-2 |
| IA32_LBR_x_TO_IP (architectural LBR) | 0x1600-0x161F | deny-gp | deny-gp | §7 PMU: architectural LBR entry target IPs; subsumes the MSR_ARCH_LBR_TO_0 base row. | SDM Vol 3B §17.5; SDM Vol 4 Table 2-2 |
| IA32_LBR_x_INFO (architectural LBR) | 0x1200-0x121F | deny-gp | deny-gp | §7 PMU + TSC plumbing: per-entry info includes cycle counts since last branch — timing leak; subsumes the MSR_ARCH_LBR_INFO_0 base row. | SDM Vol 3B §17.5; SDM Vol 4 Table 2-2 |
| IA32_DEBUG_INTERFACE | 0xC80 | deny-gp | deny-gp | §7 default-deny: silicon-debug enable/lock is host platform state and must not be probeable by the guest. | SDM Vol 4 Table 2-2 |

*Assembly merge note: the following source-fragment rows were collapsed because other
rows of this table cover the same indexes with identical `deny-gp`/`deny-gp`
dispositions: `MSR_LER_FROM_IP / MSR_LER_TO_IP` (0x1DD–0x1DE — the SDM names for the
two LASTINT rows), `MSR_LBR_NHM_FROM` (0x680) and `MSR_LBR_NHM_TO` (0x6C0) into the
legacy LBR-stack range rows, and `MSR_ARCH_LBR_FROM_0` (0x1500), `MSR_ARCH_LBR_TO_0`
(0x1600), `MSR_ARCH_LBR_INFO_0` (0x1200) into the architectural-LBR range rows. No
index coverage or disposition changed.*

#### Questions

[question] MSR_IA32_DEBUGCTLMSR (0x1D9): should a future contract revision permit a
guest-stateful mask limited to deterministic bits (e.g. BTF, bit 1, single-step-on-branches
for guest-side debugging) while keeping all LBR/BTS/freeze bits at #GP? Denied wholesale
for now (safe default); loosening requires proof that no host-dependent LBR/BTS state or
host-format dependency becomes guest-reachable, and a matching vm_state capture rule per
INTEGRATION.md §4.

### 3.8 Class `intel-pt` — Intel Processor Trace (`IA32_RTIT_*`)

Match rule: every name matching `MSR_IA32_RTIT_*` in `arch/x86/include/asm/msr-index.h`
at v6.18.35 (lines 330–380: indexes 0x560–0x561, 0x570–0x572, 0x580–0x587), plus the
architecturally reserved address-filter extension 0x588–0x58B (`ADDRn_A/B` for n=4,5; SDM
Vol3C §33.2.7 sizes the filter space by CPUID.(EAX=14H,ECX=1):EAX[2:0], and msr-index.h
names only n=0–3). All thirteen named MSRs appear in KVM's `msrs_to_save_base`
(`arch/x86/kvm/x86.c:330–345`), so they are in the reference set via
`KVM_GET_MSR_INDEX_LIST`. The entire class is denied in both directions: Intel PT is
hidden in the frozen CPUID model (CPUID.7,0:EBX[25]=0; leaf 0x14 zeroed), and on a CPU
without PT every `IA32_RTIT_*` access raises #GP — so `deny-gp` is bit-exact with the
advertised CPU. The determinism case is direct: an enabled trace embeds host-real-time
TSC/MTC/CYC timestamp packets (`RTIT_CTL` bits TSC_EN/MTC_EN/CYCLEACC, msr-index.h:332–341)
and streams them asynchronously into guest-visible memory at `OUTPUT_BASE`, a DMA-like
run-dependent memory mutation, while `RTIT_STATUS`'s PacketByteCnt/BUFFOVF fields
(msr-index.h:362–369) vary with trace volume — host TSC reaching the guest through a side
door, exactly §7's TSC-plumbing vector. Per the contract's §1 policy, `deny-gp` here means:
`KVM_X86_SET_MSR_FILTER` + `KVM_MSR_EXIT_REASON_FILTER` exit to userspace, log MSR index
and guest RIP, then inject #GP — never a silent in-kernel fault. KVM only probes these
MSRs when `X86_FEATURE_INTEL_PT` is reported (`x86.c:7682–7703`); since our CPUID model
never reports it, denying is also consistent with gate 5 (no half-exposed features).
The two range-style source entries (0x560–0x561, 0x580–0x58B) are folded into the named
rows below; 0x588–0x58B keeps its own row because msr-index.h has no names for it.

| MSR | Index | Read | Write | Rationale | Citation |
|---|---|---|---|---|---|
| MSR_IA32_RTIT_OUTPUT_BASE | 0x560 | deny-gp | deny-gp | Closes §7 TSC plumbing: steers async trace output (host-timed TSC/MTC/CYC packets) into guest memory; PT hidden in CPUID, #GP architectural | x86.c:341 (msrs_to_save_base), 7692; msr-index.h:379; SDM Vol3C §33.2.7 |
| MSR_IA32_RTIT_OUTPUT_MASK | 0x561 | deny-gp | deny-gp | Closes §7 TSC plumbing: output mask/pointers for the same run-dependent trace buffer; PT hidden in CPUID, #GP architectural | x86.c:341 (msrs_to_save_base), 7693; msr-index.h:380; SDM Vol3C §33.2.7 |
| MSR_IA32_RTIT_CTL | 0x570 | deny-gp | deny-gp | Closes §7 TSC plumbing: TraceEn/TSC_EN/MTC_EN/CYCLEACC would write host-real-time packets into guest memory; PT hidden in CPUID (7,0:EBX[25]=0) | x86.c:340 (msrs_to_save_base), 7682; msr-index.h:330; SDM Vol3C §33.2.7.2 |
| MSR_IA32_RTIT_STATUS | 0x571 | deny-gp | deny-gp | Closes §7 TSC plumbing: PacketByteCnt/BUFFOVF vary with host-timed trace volume — run-dependent reads; PT hidden in CPUID | x86.c:340 (msrs_to_save_base), 7683; msr-index.h:361; SDM Vol3C ch. 33 |
| MSR_IA32_RTIT_CR3_MATCH | 0x572 | deny-gp | deny-gp | Closes §7 TSC plumbing: CR3 filter only steers the denied trace facility; PT hidden in CPUID, #GP architectural | x86.c:340 (msrs_to_save_base), 7687; msr-index.h:378; SDM Vol3C ch. 33 |
| MSR_IA32_RTIT_ADDR0_A | 0x580 | deny-gp | deny-gp | Closes §7 TSC plumbing: IP-filter/TraceStop range start for the denied trace facility; PT hidden in CPUID, #GP architectural | x86.c:342 (msrs_to_save_base), 7699; msr-index.h:370; SDM Vol3C §33.2.7 |
| MSR_IA32_RTIT_ADDR0_B | 0x581 | deny-gp | deny-gp | Closes §7 TSC plumbing: IP-filter/TraceStop range end for the denied trace facility; PT hidden in CPUID, #GP architectural | x86.c:342 (msrs_to_save_base), 7699; msr-index.h:371; SDM Vol3C §33.2.7 |
| MSR_IA32_RTIT_ADDR1_A | 0x582 | deny-gp | deny-gp | Closes §7 TSC plumbing: IP-filter/TraceStop range start for the denied trace facility; PT hidden in CPUID, #GP architectural | x86.c:343 (msrs_to_save_base), 7699; msr-index.h:372; SDM Vol3C §33.2.7 |
| MSR_IA32_RTIT_ADDR1_B | 0x583 | deny-gp | deny-gp | Closes §7 TSC plumbing: IP-filter/TraceStop range end for the denied trace facility; PT hidden in CPUID, #GP architectural | x86.c:343 (msrs_to_save_base), 7699; msr-index.h:373; SDM Vol3C §33.2.7 |
| MSR_IA32_RTIT_ADDR2_A | 0x584 | deny-gp | deny-gp | Closes §7 TSC plumbing: IP-filter/TraceStop range start for the denied trace facility; PT hidden in CPUID, #GP architectural | x86.c:344 (msrs_to_save_base), 7699; msr-index.h:374; SDM Vol3C §33.2.7 |
| MSR_IA32_RTIT_ADDR2_B | 0x585 | deny-gp | deny-gp | Closes §7 TSC plumbing: IP-filter/TraceStop range end for the denied trace facility; PT hidden in CPUID, #GP architectural | x86.c:344 (msrs_to_save_base), 7699; msr-index.h:375; SDM Vol3C §33.2.7 |
| MSR_IA32_RTIT_ADDR3_A | 0x586 | deny-gp | deny-gp | Closes §7 TSC plumbing: IP-filter/TraceStop range start for the denied trace facility; PT hidden in CPUID, #GP architectural | x86.c:345 (msrs_to_save_base), 7699; msr-index.h:376; SDM Vol3C §33.2.7 |
| MSR_IA32_RTIT_ADDR3_B | 0x587 | deny-gp | deny-gp | Closes §7 TSC plumbing: IP-filter/TraceStop range end for the denied trace facility; PT hidden in CPUID, #GP architectural | x86.c:345 (msrs_to_save_base), 7699; msr-index.h:377; SDM Vol3C §33.2.7 |
| IA32_RTIT_ADDR4_A–ADDR5_B (reserved range) | 0x588–0x58B | deny-gp | deny-gp | Closes §7 TSC plumbing: reserved PT address-filter extension (n=4,5) beyond msr-index.h's named n=0–3; unimplemented MSR access #GPs architecturally | SDM Vol3C §33.2.7 (range count via CPUID.(EAX=14H,ECX=1):EAX[2:0]); libipt pt_config |

### 3.9 Class `speculation` — speculation control & capability enumeration

Class `speculation` covers the speculation-control and capability-enumeration MSRs: the
IBRS/STIBP/SSBD control word (`IA32_SPEC_CTRL`), the IBPB and L1D-flush command MSRs
(`IA32_PRED_CMD`, `IA32_FLUSH_CMD`), the read-only enumeration MSRs
(`IA32_ARCH_CAPABILITIES`, `IA32_CORE_CAPABILITIES`), TSX control (`IA32_TSX_CTRL`), the
DOITM timing-mode control (`IA32_UARCH_MISC_CTL`), and AMD's counterparts
(`VIRT_SPEC_CTRL`, `DE_CFG`). None of these carries time, but every one is a host
fingerprint — presence and value are functions of host microarchitecture and microcode
revision (several exist only after specific microcode updates) — which is exactly what §7
"CPUID stability" forbids the guest from inheriting: a passthrough would fold host
microcode state into guest-visible values and hence into the determinism gate's state
hashes. The policy, decided explicitly and versioned per §6 rather than
left to KVM defaults: every speculation *control* feature is hidden in the frozen CPUID
model and its MSR #GPs under `KVM_X86_SET_MSR_FILTER` + `KVM_MSR_EXIT_REASON_FILTER`
(logged with index and RIP in userspace before injection) — safe because these control
writes are semantically idempotent barriers with no architecturally readable effect, so
the guest loses nothing it could ever observe. The guest is instead told it is
*unaffected* via a frozen `IA32_ARCH_CAPABILITIES` whose `*_NO` baseline keeps guest
mitigation code quiescent so it never reaches for the denied control MSRs.
Data-operand-independent timing (DOITM) is **not** advertised (ARCH_CAPABILITIES bit 12 = 0)
because Coffee Lake-S lacks `IA32_UARCH_MISC_CTL` (box: `rdmsr 0x1b01` `#GP`s), so 0x1b01 is `deny-gp` (see the rows below;
this replaces an earlier draft that pinned DOITM on). No row
in this class is `allow-stateful`, so the class contributes nothing to §4's `vm_state`
capture list — its only guest-visible values are constants, trivially coherent across
snapshot/restore.

| MSR | Index | Read | Write | Rationale | Citation |
|---|---|---|---|---|---|
| MSR_IA32_SPEC_CTRL | 0x48 | deny-gp | deny-gp | Closes §7 "CPUID stability": presence and semantics of IBRS/STIBP/SSBD depend on host microcode (CPUID.7.0:EDX[26,27,31]); the frozen model clears all three so the MSR architecturally does not exist — the emulate-as-no-op alternative was considered and rejected in favor of hide+deny (explicit, versioned), and the frozen ARCH_CAPABILITIES *_NO baseline ensures guest mitigation code never attempts the write. | linux-6.18.35 arch/x86/kvm/x86.c (msrs_to_save_base); arch/x86/include/asm/msr-index.h (0x48); arch/x86/include/asm/cpufeatures.h (CPUID.7.0:EDX[26/27/31]); Intel SDM Vol.4 Table 2-2; INTEGRATION.md §7 (CPUID stability) |
| MSR_IA32_PRED_CMD | 0x49 | deny-gp | deny-gp | Read: architectural — IA32_PRED_CMD is a write-only command MSR, RDMSR #GPs on real silicon; write: closes §7 "CPUID stability" — IBPB/SBPB are not enumerated (CPUID.7.0:EDX[26]=0), and an IBPB is a semantically idempotent predictor barrier with no architecturally readable effect, so the deny is guest-invisible apart from the architecturally correct #GP. | linux-6.18.35 arch/x86/include/asm/msr-index.h (0x49, PRED_CMD_IBPB/SBPB); arch/x86/kvm/x86.c (kvm_set_msr_common MSR_IA32_PRED_CMD: write-only, reserved-bit checked); Intel SDM Vol.4 Table 2-2 (WO); INTEGRATION.md §7 (CPUID stability) |
| MSR_IA32_CORE_CAPS | 0xcf | deny-gp | deny-gp | Closes §7 "CPUID stability": IA32_CORE_CAPABILITIES enumerates host-dependent machinery — notably split-lock detect, which implies MSR_TEST_CTRL and host-policy-dependent #AC fault semantics, i.e. host-varying guest fault behavior; CPUID.7.0:EDX[30]=0 in the frozen model so the MSR is absent (write deny is also architectural — read-only MSR). | linux-6.18.35 arch/x86/include/asm/msr-index.h (0xcf, CORE_CAPS_SPLIT_LOCK_DETECT); arch/x86/include/asm/cpufeatures.h (CPUID.7.0:EDX[30]); Intel SDM Vol.4 Table 2-2 (RO); INTEGRATION.md §7 (CPUID stability) |
| MSR_IA32_ARCH_CAPABILITIES | 0x10a | allow-fixed(0x000000000A000C09) | deny-gp | Closes §7 "CPUID stability": the live value is a per-host microcode fingerprint, so the contract freezes the **box value read under microcode 0xf8** (`rdmsr -a 0x10a` = `0x000000000a000c09`, identical on all 16 logical CPUs ⇒ homogeneous — `docs/fragments/cfl-baseline/msrs.txt`). This **differs from the Skylake-SP fingerprint** `0x400000000D10E171` (different mitigation enumeration; that is exactly why the re-baseline must read it, not carry it). Bit-by-bit (SDM Vol.4 Table 2-2): **set** — RDCL_NO(0) (not susceptible to Meltdown; box lscpu "Meltdown: Not affected"), SKIP_VMENTRY_L1DFLUSH(3), MISC_PACKAGE_CTLS(10) + ENERGY_FILTERING_CTL(11) (the IA32_MISC_PACKAGE_CTLS energy-reporting-filtering enumeration — a microcode-added bit absent from the SKX fingerprint; the MSR itself is not exposed here), GDS_CTRL(25) (Gather-Data-Sampling mitigation control present), RFDS_NO(27) (box lscpu "Reg file data sampling: Not affected"); **clear** — notably DOITM(12)=0 (Coffee Lake-S lacks `IA32_UARCH_MISC_CTL`; box `rdmsr 0x1b01` `#GP`s; gate-5 pair with the 0x1b01 deny-gp row), TSX_CTRL_MSR(7)=0 (TSX physically absent; gate-5 with §4 TSX), and every speculation-*control*-advertising bit (IBRS_ALL(1), FB_CLEAR_CTRL(18), XAPIC_DISABLE(21)) clear so no MSR row is half-implied (gate 5). The box's `*_NO` set is *narrower* than the synthetic SKX value (e.g. SSB_NO/MDS_NO/TAA_NO are **not** set — the box mitigates those rather than being immune, consistent with lscpu), but the guest stays quiescent regardless because **every speculation-control feature is hidden in CPUID.7.0:EDX** (§2 leaf-7 EDX row) — the `*_NO` bits are belt-and-suspenders, not the enforcement. Paired with CPUID.7.0:EDX[29]=1 (this enumeration MSR is advertised since it is exposed). Write deny-gp is architectural (read-only MSR). **The frozen value is answered by the userspace MSR-exit handler** like every `allow-fixed` row (denied in the filter, §1) — the contract never relies on KVM's stored/host-sampled value. | linux-6.18.35 arch/x86/kvm/x86.c (emulated_msrs_all; msr_based_features_all_except_vmx); arch/x86/include/asm/msr-index.h (ARCH_CAP_* bits); `docs/fragments/cfl-baseline/msrs.txt` (box `rdmsr -a 0x10a`); Documentation/virt/kvm/api.rst (KVM_GET_MSR_FEATURE_INDEX_LIST); Intel SDM Vol.4 Table 2-2 (IA32_ARCH_CAPABILITIES bit map); Intel "Data Operand Independent Timing ISA Guidance" (DOITM = bit 12); INTEGRATION.md §7 (CPUID stability) |
| MSR_IA32_FLUSH_CMD | 0x10b | deny-gp | deny-gp | Read: architectural — write-only command MSR; write: closes §7 "CPUID stability" — L1D_FLUSH is not enumerated (CPUID.7.0:EDX[28]=0), and an L1D flush is a purely microarchitectural idempotent action with no architecturally readable effect, so the deny is guest-invisible apart from the correct #GP. | linux-6.18.35 arch/x86/include/asm/msr-index.h (0x10b, L1D_FLUSH); arch/x86/kvm/x86.c (kvm_set_msr_common MSR_IA32_FLUSH_CMD: write-only); arch/x86/include/asm/cpufeatures.h (CPUID.7.0:EDX[28]); Intel SDM Vol.4 Table 2-2 (WO); INTEGRATION.md §7 (CPUID stability) |
| MSR_IA32_TSX_CTRL | 0x122 | deny-gp | deny-gp | Closes §7 "CPUID stability": frozen ARCH_CAPABILITIES bit 7 (TSX_CTRL_MSR)=0 and CPUID.7.0:EBX[11 RTM, 4 HLE]=0 so the MSR architecturally does not exist — and on the **Coffee Lake-S baseline this is physical, not just hidden**: `IA32_TSX_CTRL` does not exist on the box (`rdmsr 0x122` `#GP`s — `docs/fragments/cfl-baseline/msrs.txt`), and RTM/HLE are not implemented, so XBEGIN/XEND/XTEST/XABORT `#UD` natively (§4 TSX rows). The determinism enforcement is therefore **silicon-level absence**, not a host pin — the SKX baseline (TSX-present) had to pin `IA32_TSX_CTRL = RTM_DISABLE|CPUID_CLEAR` to force a deterministic always-abort; the TSX-absent box neither has that MSR nor needs the pin. deny-gp here is then doubly architectural (the MSR is absent on the host too). | linux-6.18.35 arch/x86/kvm/x86.c (msrs_to_save_base); arch/x86/include/asm/msr-index.h (0x122, TSX_CTRL_RTM_DISABLE/CPUID_CLEAR); `docs/fragments/cfl-baseline/msrs.txt` (box `rdmsr 0x122` #GP); Intel SDM Vol.4 Table 2-2; Intel TAA guidance (TSX Async Abort deep dive); rr (RTM masking is CPUID-only at ptrace level — arXiv:1705.05937); INTEGRATION.md §7 (CPUID stability) |
| IA32_UARCH_MISC_CTL | 0x1b01 | deny-gp | deny-gp | §7 "CPUID stability" + gate-5: **Coffee Lake-S physically lacks this MSR** (box `rdmsr 0x1b01` `#GP`s — `docs/fragments/cfl-baseline/msrs.txt`; DOITM/`IA32_UARCH_MISC_CTL` was introduced post-Skylake via microcode on later parts), so the contract does **not** advertise DOITM — ARCH_CAPABILITIES bit 12 = 0 (§3.9 0x10a row) — and denies 0x1b01 in both directions. This keeps the SKX baseline's decision (the SKX part likewise lacked the MSR); the earlier "pin DOITM=1 and mirror it on the host pCPU" obligation cannot be satisfied on either baseline (no such MSR/control to mirror). DOITM cleared ↔ 0x1b01 deny-gp is the gate-5 pairing. If a future baseline includes DOITM, flip ARCH_CAPABILITIES bit 12 back to 1, change this row to allow-fixed(0x1)/deny-ignore-write, and add a `host-assert doitm-supported` — together, in one version bump. | Intel "Data Operand Independent Timing ISA Guidance" (IA32_UARCH_MISC_CTL, DOITM bit 0 = Ice Lake+/microcode); lwn.net/Articles/921232; §3.9 (MSR_IA32_ARCH_CAPABILITIES 0x10a, DOITM bit 12 = 0); INTEGRATION.md §7 (CPUID stability) |
| MSR_AMD64_VIRT_SPEC_CTRL | 0xc001011f | deny-gp | deny-gp | Closes §7 "CPUID stability": AMD-only paravirtualized SSBD, enumerated by CPUID.8000_0008:EBX[25] (VIRT_SSBD); the frozen baseline is a single Intel microarchitecture and task 06 declares AMD a non-goal, so the MSR architecturally does not exist — #GP, loudly logged. | linux-6.18.35 arch/x86/kvm/x86.c (emulated_msrs_all); arch/x86/include/asm/msr-index.h (0xc001011f); arch/x86/include/asm/cpufeatures.h (CPUID.8000_0008:EBX[25]); AMD APM Vol.2 (virtualized VIRT_SPEC_CTRL); tasks/06 non-goals (AMD); INTEGRATION.md §7 (CPUID stability) |
| MSR_AMD64_DE_CFG | 0xc0011029 | deny-gp | deny-gp | Closes §7 "CPUID stability": AMD-only decode-engine config (LFENCE dispatch-serializing bit) — a KVM msr-based *feature* MSR whose value is host CPU policy; the frozen Intel baseline never enumerates it, and LFENCE serialization on the host is a vmm-core/host concern, never guest-visible state — #GP, loudly logged. | linux-6.18.35 arch/x86/kvm/x86.c (msr_based_features_all_except_vmx); arch/x86/include/asm/msr-index.h (0xc0011029, DE_CFG_LFENCE_SERIALIZE); Documentation/virt/kvm/api.rst (KVM_GET_MSR_FEATURE_INDEX_LIST); tasks/06 non-goals (AMD); INTEGRATION.md §7 (CPUID stability) |

#### Questions

- **[resolved] TSX enforcement (det-cfl-v1: physical absence).** On the Coffee Lake-S baseline
  RTM/HLE are **physically absent** (box CPUID.7.0:EBX[4,11]=0; `IA32_TSX_CTRL` 0x122 `#GP`s), so
  XBEGIN/XEND/XTEST/XABORT `#UD` natively — class (b) `fault-absent` (§4 TSX row), robust against
  any guest by silicon. **No host pin is installed or needed.** The hashed `host-assert
  rtm-disabled true` (§6) is satisfied by physical absence (the probe reads EBX[11] and passes
  when RTM is absent — no code change). *(On the previous TSX-present SKX baseline this was
  class (c): the host pinned `IA32_TSX_CTRL = RTM_DISABLE|TSX_CPUID_CLEAR` to force a deterministic
  always-abort. The re-baseline replaces the pin with silicon absence; the determinism outcome —
  TSX non-usable, deterministically — is invariant, only the mechanism changed.)*
- **[resolved] IA32_UARCH_MISC_CTL/DOITM:** **clear DOITM** (ARCH_CAPABILITIES box value
  `0x000000000A000C09`, bit 12 = 0) and **deny-gp 0x1b01** — Coffee Lake-S physically lacks
  `IA32_UARCH_MISC_CTL` (box `rdmsr 0x1b01` `#GP`s), so the "pin DOITM=1 and mirror it on the host
  pCPU" obligation cannot be satisfied (no such MSR/control to mirror) — same as the SKX baseline.
  The two rows move together (gate 5). A future DOITM-bearing baseline would flip both back and add
  a `host-assert doitm-supported`.

### 3.10 Class `microcode` — microcode update interface

The `microcode` class covers the microcode-update interface: the update trigger
IA32_BIOS_UPDT_TRIG and the signature/revision register IA32_BIOS_SIGN_ID. Match rule
against `arch/x86/include/asm/msr-index.h` @ v6.18.35: every name matching
`MSR_IA32_UCODE_*` — exactly two entries, `MSR_IA32_UCODE_WRITE` (0x79, msr-index.h:938)
and `MSR_IA32_UCODE_REV` (0x8b, msr-index.h:939); the AMD analog `MSR_AMD64_PATCH_LOADER`
is out of scope (AMD is a task-06 non-goal) and falls to the §1 default-deny filter.
Policy: the guest must never load microcode (an update would mutate CPU behavior mid-run,
host-dependently and irreversibly), and the revision it reads must be a frozen constant
from the versioned baseline — never the host's revision, which KVM's feature-MSR path
otherwise samples straight from the host CPU (`kvm_get_feature_msr` does
`rdmsrq_safe(MSR_IA32_UCODE_REV)`, x86.c:1714) and which KVM deliberately leaves mutable
(`kvm_is_immutable_feature_msr` exempts exactly this MSR, x86.c:495). vmm-core sets the
frozen value once, host-initiated, at vCPU creation and never again. The write disposition
on 0x8b is boot-critical and not negotiable: the task-04 pinned guest builds with
CONFIG_CPU_SUP_INTEL=y (default y, Kconfig.cpu:365), so `early_init_intel()` (intel.c:207)
runs the SDM signature sequence — `native_wrmsrq(MSR_IA32_UCODE_REV, 0)`, CPUID(1), RDMSR
(microcode.h:64–77) — with no exception fixup; a #GP there kills the guest in early boot,
so the write is dropped-and-logged (`deny-ignore-write`), which together with the fixed
read value reproduces the architectural readback exactly (real hardware reloads the
signature after the CPUID, SDM Vol 3A §9.11.7.1). Nothing in this class is captured in
`vm_state` per INTEGRATION.md §4: the guest can establish no state here — the fixed
revision is versioned config hashed into the determinism gate (§6), not state. Column
grammar: `Read`/`Write` are drawn verbatim from the task-06 §3 disposition vocabulary;
`Rationale` names the INTEGRATION.md §7 leak vector closed (or `architectural`); kernel
citations are `file:line` at the pinned tag v6.18.35.

| MSR | Index | Read | Write | Rationale | Citation |
|---|---|---|---|---|---|
| MSR_IA32_UCODE_REV (IA32_BIOS_SIGN_ID) | 0x8b | allow-fixed(0x0000_0001_0000_0000) | deny-ignore-write | CPUID stability: frozen **guest-visible** revision 0x00000001 in bits 63:32 (bits 31:0 read 0) — this is the `guest-ucode-rev`, a synthetic constant the guest reads, and is **deliberately distinct from** the host-assert `host-microcode-rev` (§6), the *physical* host's pinned microcode revision (the det-cfl-v1 box value **0xf8**, §6). The two must never be conflated — a literal implementation that asserted the host equals 0x00000001 would reject the real box (which reports 0xf8). Never the host revision KVM samples via rdmsrq (x86.c:1714) — KVM treats this as the one mutable feature MSR (x86.c:495), so the contract pins it; write must not #GP because early_init_intel()'s unguarded WRMSR-0/CPUID/RDMSR signature sequence runs in the pinned guest's early boot, and ignoring the write while reading back the fixed value is exactly the architectural reload semantics. | x86.c:436 (emulated_msrs_all), x86.c:472 (msr_based_features_all_except_vmx), x86.c:495 (kvm_is_immutable_feature_msr), x86.c:1714 (kvm_get_feature_msr host rdmsrq), x86.c:3958/4420 (guest write ignored / read returns microcode_version); msr-index.h:939; intel.c:207 (early_init_intel) + microcode.h:64–77 (intel_get_microcode_revision), all @ v6.18.35; Intel SDM Vol 3A §9.11.7.1 (update signature sequence); SDM Vol 4 Table 2-2 (IA32_BIOS_SIGN_ID); KVM api.rst KVM_GET_MSR_FEATURE_INDEX_LIST |
| MSR_IA32_UCODE_WRITE (IA32_BIOS_UPDT_TRIG) | 0x79 | deny-gp | deny-gp | CPUID stability + architectural: a write is a microcode-update attempt that would change CPU behavior mid-run host-dependently, so it must fail loudly — KVM's default is a silent drop (x86.c:3949), which §1 forbids; reads #GP architecturally (the MSR is write-only per SDM Vol 4) and KVM likewise rejects them (no get-side case); no pinned-guest boot path writes it — the loader self-disables on the hypervisor bit (core.c:111) or finds no update blob in the task-04 busybox initramfs. | msr-index.h:938; x86.c:3949 (kvm_set_msr_common ignored-writes group — contract diverges: loud #GP, not silent drop); core.c:111 (microcode_loader_disabled), all @ v6.18.35; Intel SDM Vol 3A §9.11.6 (microcode update loader / BIOS_UPDT_TRIG); SDM Vol 4 Table 2-2 (IA32_BIOS_UPDT_TRIG, write-only) |

#### Questions

[question] MSR_IA32_UCODE_REV (0x8b): the pinned revision 0x00000001 must be ratified
jointly with the frozen CPUID family/model/stepping and the speculation class. If the
frozen model hides the hypervisor bit (CPUID.1:ECX[31]=0) and the chosen model/stepping
matches an entry in `spectre_bad_microcodes` (intel.c:106 @ v6.18.35),
`bad_spectre_microcode()` (intel.c:130) treats any revision ≤ the blacklisted one as bad
and the guest deterministically clears SPEC_CTRL/IBPB/STIBP feature bits with a boot
warning; with the hypervisor bit set the check short-circuits and 0x1 is inert. Decided
value stands (behavior is deterministic either way) — should a later revision instead pin
a value above the blacklist threshold for the chosen SKU so the speculation-class feature
bits survive guest feature detection?

**Decision (was a [question]) — MSR_IA32_UCODE_WRITE (0x79) deny-gp write is bound boot-safe
by a task-04 image constraint.** With the hypervisor bit hidden (CPUID.1:ECX[31]=0) the
early loader does not self-disable on the hypervisor signature (core.c:111), so the deny-gp
write is safe **iff** the guest never reaches an unguarded `native_wrmsrq(MSR_IA32_UCODE_WRITE, …)`.
Two facts of the pinned task-04 build make that hold, and the contract **binds** them
(violating either is a contract defect, re-triaged per §1):
1. **No microcode in the kernel config.** The pinned config is `tinyconfig`
   (allnoconfig + tiny.config) plus `guest/linux/config-fragment`, and neither selects
   `CONFIG_MICROCODE` — so the in-kernel early/late microcode loader is not compiled in at
   all, and `load_ucode_bsp()`/`microcode_init()` do not exist in the image. (This supersedes
   the earlier worry that `CONFIG_MICROCODE` is unconditionally `def_bool y`: it is *not* set
   under the pinned tinyconfig base, verified against the merged `guest/linux/config-fragment`.)
2. **No microcode blob in the initramfs.** The task-04 `guest/linux/build-initramfs.sh`
   spec packs exactly `dir /dev /proc /sys /bin`, `file /bin/busybox`, and `file /init` — it
   contains **no `kernel/x86/microcode/GenuineIntel.bin` cpio entry**, so even a
   built-in loader (were one ever enabled) would find no update blob.

The contract therefore binds the task-04 image manifest to **"no `kernel/x86/microcode/`
cpio entries and `CONFIG_MICROCODE` unset"**; under that binding the deny-gp write is never
reached at boot, and a deny-gp (not KVM's silent drop) is the loud failure §1 demands if a
future image violates the binding. The alternative — mandating hypervisor bit = 1 so
`microcode_loader_disabled()` short-circuits regardless of image contents — is **rejected**
because exposing CPUID.1:ECX[31] reopens §7's kvmclock probe vector (guests read
0x4000_00xx only when the hypervisor bit is set), which the frozen model closes at its root.

### 3.11 Class `entropy` — MSR-borne host-event counters (SMI count)

Class `entropy` covers MSR-borne nondeterministic host-event counters — entropy side
doors outside the RDRAND/RDSEED instructions, which PLAN.md's trap table already routes
to the seeded PRNG stream over the hypercall channel (the port-I/O doorbell, INTEGRATION.md
§1). Its single member is `MSR_SMI_COUNT`
(0x34): a model-specific (Nehalem+, no CPUID enumeration bit) read-only counter of System
Management Interrupts, i.e. asynchronous host firmware events whose arrival is pure
real-world nondeterminism — exactly the kind of free-running host-activity counter
turbostat reads to monitor the host, and an entropy/timing channel if it ever reached the
guest. Match rule: every name matching `MSR_SMI_COUNT` in
`arch/x86/include/asm/msr-index.h` at v6.18.35 (one MSR, msr-index.h:913); it is in the
reference set via `emulated_msrs_all` (`arch/x86/kvm/x86.c:430`, behind
`KVM_GET_MSR_INDEX_LIST`). KVM's own emulation already decouples it from the host — RDMSR
returns `vcpu->arch.smi_count`, the count of *virtual* SMIs KVM injected (x86.c:4502),
and guest WRMSR #GPs (host-initiated writes only, x86.c:4134) — and this VMM never
delivers SMM/SMIs to the guest at all, so the only deterministic readback would be a
constant 0 the guest has no enumerable need for. Under the contract's default-deny
posture (hide/deny unless explicitly justified) the class is denied in both directions;
because the MSR is model-specific with no CPUID feature bit, probing guests already use
`rdmsr_safe`-style access and #GP is the behavior they are built to absorb. Per the
contract's §1 policy, `deny-gp` means: `KVM_X86_SET_MSR_FILTER` +
`KVM_CAP_X86_USER_SPACE_MSR` with `KVM_MSR_EXIT_REASON_FILTER` exit to userspace, log MSR
index and guest RIP, then inject #GP — never a silent in-kernel fault.

| MSR | Index | Read | Write | Rationale | Citation |
|---|---|---|---|---|---|
| MSR_SMI_COUNT | 0x34 | deny-gp | deny-gp | Closes §7's power/frequency host-event-counter vector (generalized to SMIs): the count of host firmware SMIs is asynchronous real-world nondeterminism usable as an entropy/timing channel; no SMM is ever delivered to the guest so the deterministic value is a constant with no enumerable consumer — default-deny; write-#GP also matches silicon (read-only counter) and KVM (guest writes rejected, x86.c:4134) | linux-6.18.35 arch/x86/kvm/x86.c:430 (emulated_msrs_all), 4502 (RDMSR returns vcpu->arch.smi_count), 4134 (guest WRMSR #GP unless host-initiated); arch/x86/include/asm/msr-index.h:913; Intel SDM Vol. 4 Table 2-2 (MSR_SMI_COUNT, 34H, Nehalem+); tools/power/x86/turbostat/turbostat.c:1789 (host SMI counting); INTEGRATION.md §7 (default-deny, power/frequency); PLAN.md trap table (entropy → seeded stream) |

### 3.12 Class `x2apic` — x2APIC MSR surface (0x800–0x8FF)

Class `x2apic` covers INTEGRATION.md §7's "x2APIC MSR surface" vector — defined exactly as
every MSR index `I` with `0x800 ≤ I ≤ 0x8FF` (the architecturally reserved x2APIC MSR
address space; register at `I = 0x800 + (xAPIC offset >> 4)`, `APIC_BASE_MSR` in
`arch/x86/kvm/lapic.c` at v6.18.35), with the defined-register subset taken from Intel SDM
Vol.4 Table 2-2 / x2APIC spec 318148 (802H–83FH) and cross-checked against
`kvm_lapic_readable_reg_mask` plus the write-only EOI/SELF-IPI cases in `lapic.c` at the
same tag; the rows below partition the full range with no gaps. The disposition is forced
by two documented pinned-kernel facts: (1) `Documentation/virt/kvm/api.rst` at v6.18.35 —
"Enabling x2APIC in KVM_SET_CPUID2 requires KVM_CREATE_IRQCHIP as KVM doesn't support
forwarding x2APIC MSR accesses to userspace"; and (2) the `KVM_X86_SET_MSR_FILTER` caveat —
"x2APIC MSR accesses cannot be filtered (KVM silently ignores filters that cover any x2APIC
MSRs)". So if x2APIC were exposed, every register in this block would be serviced by the
in-kernel LAPIC with no userspace interposition possible — and that LAPIC's timer is host
real time: TMCCT reads are computed from `ktime_get()`/hrtimer-remaining
(`lapic.c apic_get_tmcct`) and TMICT/LVT-timer writes arm host hrtimers whose expiry
injects interrupts at host-determined instants — exactly what §7 "Timer devices" forbids
("no KVM in-kernel timer devices unless proven V-time-driven") and what PLAN.md's
interrupt-timing row forbids (interrupts only host-injected at exact V-time). The contract
therefore hides x2APIC: the frozen CPUID model clears CPUID.1:ECX[21], the guest's only
APIC is the userspace-emulated xAPIC MMIO page backed by `TimerQueue`/V-time (no
`KVM_CREATE_IRQCHIP`; per-register semantics in the rationales below; the MMIO sub-table
itself belongs to the timer/time-device section — §5, now present), and
`IA32_APIC_BASE.EXTD` is never settable. The contract services
`IA32_APIC_BASE` (0x1b) in userspace as `deny-ignore-write` (§3.13), so a guest WRMSR 0x1b
setting EXTD is **dropped and logged** — the value stays EXTD=0, enforced **by value**, so
x2APIC mode can never be entered. (Note: this means an EXTD-set write does not itself #GP — it
is dropped — which is deterministic; the in-kernel `lapic.c kvm_apic_set_base` reserved-bit
#GP, which folds `X2APIC_ENABLE` into `reserved_bits` when guest CPUID lacks X2APIC, is the
defense-in-depth fallback were the userspace handler ever bypassed.) Either way, every
RDMSR/WRMSR in 0x800–0x8FF takes the architectural #GP (SDM: the block is accessible only
when EXTD=1), which EXTD=0 guarantees. The deny is still
loud despite the filter carve-out: with `KVM_MSR_EXIT_REASON_INVAL` enabled in
`KVM_CAP_X86_USER_SPACE_MSR`, the failed access exits as `KVM_EXIT_X86_RDMSR`/`WRMSR`
(reason INVAL, `x86.c kvm_msr_reason`), is logged with index and RIP, then completes with
`error = 1` so KVM injects #GP — never a silent zero. **TSC-deadline is hidden**
(CPUID.1:ECX[24]=0, §2) and MSR 0x6E0 is `deny-gp` (§3.3): although api.rst *permits* the CPUID
bit with userspace emulation, that does **not** make the WRMSR serviceable — under
`KVM_IRQCHIP_NONE` the in-kernel `MSR_IA32_TSC_DEADLINE` WRMSR fastpath runs **before** the MSR
filter and no-ops with no in-kernel apic, so `emulate-timerqueue` would never run (round-7 fix,
Ruling R1). The LAPIC timer is therefore the xAPIC LVT **one-shot/periodic** MMIO model (§5),
not TSC-deadline mode. The task-04 guest kernel drops `CONFIG_X86_X2APIC` as defense in depth.

| MSR | Index | Read | Write | Rationale | Citation |
|---|---|---|---|---|---|
| IA32_X2APIC_APICID | 0x802 | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface": x2APIC hidden (CPUID.1:ECX[21]=0, APIC_BASE.EXTD reserved) so the MSR alias #GPs; the logical APIC ID is served at xAPIC MMIO 020H as frozen 0 — single-vCPU topology (PLAN.md: one vCPU, period), no host topology leak. | Intel SDM Vol.4 Table 2-2 (802H) + Vol.3A ch.11; x2APIC spec 318148; linux-6.18.35 arch/x86/kvm/lapic.c (kvm_apic_set_base reserved_bits); INTEGRATION.md §7 (x2APIC MSR surface) |
| IA32_X2APIC_VERSION | 0x803 | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface": alias #GPs; xAPIC 030H returns the frozen constant 0x00050014 (version 14H, max-LVT 5, no directed-EOI) from the userspace model — mirrors KVM's APIC_VERSION so no host APIC revision leaks and the LVT-CMCI row stays architecturally absent. | Intel SDM Vol.4 Table 2-2 (803H); linux-6.18.35 arch/x86/kvm/lapic.c (APIC_VERSION 0x14, kvm_apic_set_version); INTEGRATION.md §7 (x2APIC MSR surface, CPUID stability) |
| IA32_X2APIC_TPR | 0x808 | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface": alias #GPs; TPR is pure guest-written priority state at xAPIC 080H in the userspace LAPIC, captured in vm_state (§4) — architectural state, no time content. | Intel SDM Vol.4 Table 2-2 (808H); INTEGRATION.md §7 (x2APIC MSR surface) + §4 (vm_state LAPIC state) |
| IA32_X2APIC_PPR | 0x80a | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface": alias #GPs (architecturally read-only besides); xAPIC 0A0H PPR is computed deterministically as a pure function of captured TPR/ISR state — never a host-priority artifact. | Intel SDM Vol.4 Table 2-2 (80AH) + Vol.3A ch.11 (PPR computation); INTEGRATION.md §7 (x2APIC MSR surface) + §4 |
| IA32_X2APIC_EOI | 0x80b | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface": alias #GPs (reads #GP even in x2APIC — write-only register); xAPIC 0B0H writes retire the highest in-service ISR bit in the userspace model — the single deterministic EOI path that the kvmclock fragment's MSR_KVM_PV_EOI_EN deny preserves. | Intel SDM Vol.4 Table 2-2 (80BH, WO); linux-6.18.35 arch/x86/kvm/lapic.c (kvm_lapic_readable_reg_mask: EOI not readable); INTEGRATION.md §7 (x2APIC MSR surface); fragments/msr-kvmclock.md (PV_EOI row) |
| IA32_X2APIC_LDR | 0x80d | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface": alias #GPs; xAPIC 0D0H LDR (with DFR) is guest-writable logical-destination state captured in vm_state — consulted only by deterministic userspace delivery, trivial on one vCPU. | Intel SDM Vol.4 Table 2-2 (80DH); INTEGRATION.md §7 (x2APIC MSR surface) + §4 |
| IA32_X2APIC_SIVR | 0x80f | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface": alias #GPs; the software-enable bit and spurious vector live in the userspace model (vm_state §4), and spurious-interrupt delivery occurs only at deterministic emulation points — never on a host-timed race. | Intel SDM Vol.4 Table 2-2 (80FH); INTEGRATION.md §7 (x2APIC MSR surface) + §4 |
| IA32_X2APIC_ISR0–ISR7 | 0x810-0x817 | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface": aliases #GP (read-only registers); in-service bitmaps at xAPIC 100H–170H are a pure function of InjectionPlanner injections (interrupts only host-injected at exact V-time — PLAN.md interrupt-timing row) and guest EOIs, serialized as §4's pending/in-service interrupt state. | Intel SDM Vol.4 Table 2-2 (810H–817H); PLAN.md (interrupt timing row); INTEGRATION.md §7 (x2APIC MSR surface) + §4; consonance/vtime/src/planner.rs (InjectionPlanner); antithesis.com/blog/deterministic_hypervisor/ (APIC delivery + virtual time) |
| IA32_X2APIC_TMR0–TMR7 | 0x818-0x81f | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface": aliases #GP (read-only); trigger-mode bitmaps at xAPIC 180H–1F0H are set deterministically at userspace-IOAPIC delivery time and captured in vm_state — no host edge/level race exists. | Intel SDM Vol.4 Table 2-2 (818H–81FH); INTEGRATION.md §7 (x2APIC MSR surface, Timer devices) + §4 |
| IA32_X2APIC_IRR0–IRR7 | 0x820-0x827 | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface": aliases #GP (read-only); request bitmaps at xAPIC 200H–270H mutate only on planner-scheduled V-time injections and ICR self-IPIs — never on host-timed events, so polling IRR cannot observe real time. | Intel SDM Vol.4 Table 2-2 (820H–827H); PLAN.md (interrupt timing row); INTEGRATION.md §7 (x2APIC MSR surface) + §4 |
| IA32_X2APIC_ESR | 0x828 | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface": alias #GPs (x2APIC additionally #GPs non-zero writes); xAPIC 280H follows the write-then-read protocol over error state generated only by deterministic emulation events (e.g. illegal-vector writes), never host conditions. | Intel SDM Vol.4 Table 2-2 (828H); linux-6.18.35 arch/x86/kvm/lapic.c (kvm_lapic_reg_write APIC_ESR x2apic non-zero reject); INTEGRATION.md §7 (x2APIC MSR surface) |
| IA32_X2APIC_LVT_CMCI | 0x82f | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface" plus the host-event family of §7 "PMU"/"Power/frequency": CMCI reports host-physical corrected-machine-check events — nondeterministic by nature; the frozen version value (max-LVT 5) makes even xAPIC 2F0H reserved (KVM analog: LVT_CMCI exists only with MCG_CMCI_P, which is never set). | Intel SDM Vol.4 Table 2-2 (82FH); linux-6.18.35 arch/x86/kvm/lapic.c (kvm_apic_calc_nr_lvt_entries: MCG_CMCI_P gate); INTEGRATION.md §7 (x2APIC MSR surface) |
| IA32_X2APIC_ICR | 0x830 | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface": the 64-bit x2APIC ICR is unreachable; xAPIC 300H/310H IPIs on the single vCPU are self/fixed-only, queued to IRR at deterministic emulation points with delivery-status always idle — interrupt arrival stays planner-controlled, never asynchronous. | Intel SDM Vol.4 Table 2-2 (830H); linux-6.18.35 arch/x86/kvm/lapic.c (kvm_x2apic_icr_write, X2APIC_ICR_RESERVED_BITS); kernel.org KVM errata + KVM_CAP_X2APIC_API (x2APIC ICR/dest-ID quirks — moot with x2APIC hidden); INTEGRATION.md §7 (x2APIC MSR surface); antithesis.com/blog/deterministic_hypervisor/ |
| IA32_X2APIC_LVT_TIMER | 0x832 | deny-gp | deny-gp | Closes §7 "Timer devices": alias #GPs; xAPIC 320H mode/mask/vector writes deterministically arm, rearm, or cancel the TimerQueue entry — **one-shot/periodic only; TSC-deadline mode (10b) is unavailable** (CPUID.1:ECX[24]=0, MSR 0x6E0 deny-gp, §3.3) — never a KVM hrtimer on host real time. | Intel SDM Vol.3A ch.11 (LVT timer modes) + Vol.4 Table 2-2 (832H); linux-6.18.35 arch/x86/kvm/lapic.c (lapic_timer hrtimer — the avoided path); INTEGRATION.md §7 (Timer devices); consonance/vtime/src/queue.rs (TimerQueue); §3.3 (0x6e0 deny-gp) |
| IA32_X2APIC_LVT_THERMAL | 0x833 | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface" via §7 "Power/frequency": alias #GPs; xAPIC 330H is writable state in vm_state, but thermal events are host-physical and no thermal model exists, so the LVT never fires — programming it is inert and deterministic. | Intel SDM Vol.4 Table 2-2 (833H); INTEGRATION.md §7 (Power/frequency, x2APIC MSR surface) + §4 |
| IA32_X2APIC_LVT_PMI | 0x834 | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface" via §7 "PMU": alias #GPs; xAPIC 340H is writable state in vm_state, but no vPMU is exposed (host owns the PMU; RDPMC traps), so no counter-overflow PMI can ever be generated toward the guest — the LVT never fires. | Intel SDM Vol.4 Table 2-2 (834H); INTEGRATION.md §7 (PMU, x2APIC MSR surface); PLAN.md (RDPMC trap row) |
| IA32_X2APIC_LVT_LINT0 | 0x835 | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface": alias #GPs; xAPIC 350H is the ExtINT wiring for the userspace PIC — every interrupt it can carry originates from TimerQueue-backed device models injected at exact V-time, never a physical pin. | Intel SDM Vol.4 Table 2-2 (835H); INTEGRATION.md §7 (Timer devices, x2APIC MSR surface) + §5 adapter map (PIT/PIC backed by TimerQueue) |
| IA32_X2APIC_LVT_LINT1 | 0x836 | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface": alias #GPs; xAPIC 360H's NMI pin is never pulsed by any host event — an NMI, if ever used, is a planner decision at an exact V-time, not a watchdog (PLAN.md guest config: no watchdogs). | Intel SDM Vol.4 Table 2-2 (836H); PLAN.md (guest config: no watchdogs; interrupt timing row); INTEGRATION.md §7 (x2APIC MSR surface) |
| IA32_X2APIC_LVT_ERROR | 0x837 | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface": alias #GPs; xAPIC 370H holds the vector for APIC errors that arise only from deterministic emulation (illegal vectors etc.), so error-interrupt timing is replayable by construction. | Intel SDM Vol.4 Table 2-2 (837H); INTEGRATION.md §7 (x2APIC MSR surface) + §4 |
| IA32_X2APIC_INIT_COUNT | 0x838 | deny-gp | deny-gp | Closes §7 "Timer devices" (the arming half): with the in-kernel LAPIC this write would arm a host hrtimer KVM fires on real time; alias dead, and the xAPIC 380H write converts count × divide × frozen bus period to an absolute V-ns deadline on the TimerQueue (0 disarms; periodic mode re-arms deterministically), read returning the stored initial count from vm_state. | Intel SDM Vol.3A ch.11 (timer) + Vol.4 Table 2-2 (838H); linux-6.18.35 arch/x86/kvm/lapic.c (lapic_timer hrtimer arming — the avoided path); INTEGRATION.md §7 (Timer devices) + §3 (idle-skip via TimerQueue::peek_next); consonance/vtime/src/queue.rs (TimerQueue) |
| IA32_X2APIC_CUR_COUNT | 0x839 | deny-gp | deny-gp | Closes §7 "Timer devices" — the countdown leak named for this class: KVM computes this read from ktime_get()/hrtimer-remaining (host real time); alias dead, and xAPIC 390H is served emulate-vtime: remaining = ticks((deadline_vns − VClock::vns(work)) / tick_vns), 0 when unarmed or expired — monotone in retired-branch work and bit-identical on replay. | linux-6.18.35 arch/x86/kvm/lapic.c (apic_get_tmcct: ktime_get); Intel SDM Vol.4 Table 2-2 (839H, RO); INTEGRATION.md §7 (Timer devices); consonance/vtime/src/clock.rs (VClock::vns); antithesis.com/blog/deterministic_hypervisor/ (virtual time) |
| IA32_X2APIC_DIV_CONF | 0x83e | deny-gp | deny-gp | Closes §7 "Timer devices": alias #GPs; the xAPIC 3E0H divide value (bits 0,1,3) is stored state in vm_state feeding the INIT_COUNT/CUR_COUNT tick conversions — a divide rewrite deterministically recomputes the armed TimerQueue deadline, with no hrtimer to cancel. | Intel SDM Vol.4 Table 2-2 (83EH); linux-6.18.35 arch/x86/kvm/lapic.c (APIC_TDCR write → hrtimer restart — the avoided path); INTEGRATION.md §7 (Timer devices) + §4 |
| IA32_X2APIC_SELF_IPI | 0x83f | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface": SELF IPI is x2APIC-only with no xAPIC alias (offset 3F0H is reserved), so with x2APIC hidden the register exists nowhere — self-IPIs use the ICR path, which is deterministic per its row. | Intel SDM Vol.4 Table 2-2 (83FH, WO, x2APIC-only); linux-6.18.35 arch/x86/kvm/lapic.c (APIC_SELF_IPI rejected outside x2apic mode); INTEGRATION.md §7 (x2APIC MSR surface) |
| X2APIC reserved (defined-register gaps) | 0x800-0x801, 0x804-0x807, 0x809, 0x80c, 0x80e, 0x829-0x82e, 0x831, 0x83a-0x83d | deny-gp | deny-gp | Architectural: reserved x2APIC addresses #GP even in x2APIC mode (APR, RRD, DFR, and ICR2 have no x2APIC counterpart) — doubly dead here with EXTD unreachable; matches kvm_lapic_readable_reg_mask leaving these bits clear. | Intel SDM Vol.3A ch.11 (x2APIC address space, reserved entries) + x2APIC spec 318148; linux-6.18.35 arch/x86/kvm/lapic.c (kvm_lapic_readable_reg_mask: ARBPRI/DFR/ICR2 invalid in x2APIC) |
| X2APIC reserved (tail) | 0x840-0x8ff | deny-gp | deny-gp | Architectural: reserved tail of the block ("available for future Intel extensions"); the blanket row keeps registers added by future silicon closed by default and makes the class a gapless partition of 0x800–0x8FF. | Intel x2APIC spec 318148 + Intel SDM Vol.3A ch.11 (reserved x2APIC MSR space); INTEGRATION.md §7 (x2APIC MSR surface); scope-versioning fragment (default-deny) |

#### Questions

- [question] Vocabulary clash to resolve at merge: the scope/enforcement fragment
  ("Enforcement carve-outs") anticipates `emulate-apic` rows in this sub-table enforced "by
  the APIC virtualization configuration itself (split irqchip / userspace timer
  emulation)", and INTEGRATION.md §7 names split irqchip as the default plan — but at
  v6.18.35 that configuration cannot satisfy §7: split irqchip keeps the LAPIC in the
  kernel, its x2APIC MSRs are unfilterable (api.rst silently ignores filters over
  0x800–0x8FF) and unforwardable to userspace, and its timer runs on host hrtimers
  (`apic_get_tmcct` reads `ktime_get()`). This fragment therefore takes `deny-gp` across
  the block, hides CPUID.1:ECX[21], and moves the deterministic per-register semantics to
  the userspace xAPIC MMIO sub-table (§5, now present). The master contract and the
  scope fragment's carve-out wording must be updated to "userspace LAPIC, x2APIC hidden" —
  confirm that update, or produce the proof §7 demands ("revisit only with proof") that an
  in-kernel LAPIC path can be V-time-driven.
  **[resolved at assembly]** §1's x2APIC carve-out now states the adopted stance
  (userspace xAPIC, x2APIC hidden, `deny-gp` across 0x800–0x8FF).
- [question] If vmm-core ever wants real x2APIC (to retire the xAPIC MMIO page or for
  exit-cost reasons), the only route at the pinned tag is a carried kernel patch — a
  V-time-driven in-kernel LAPIC timer plus filterable/forwardable x2APIC MSR accesses —
  per INTEGRATION.md §6's deferred kernel-work item. Does the project accept that patch
  burden, or is xAPIC-only frozen for v1? Until answered, every row above stands as
  `deny-gp` (safe by default; loosening is a contract version bump).

### 3.13 Class `arch-stateful` — architectural guest-writable state

The `arch-stateful` class is the architecturally guest-writable CPU state with no time, entropy, or host-hardware content: its determinism story is capture-and-restore — every `allow-stateful` value below is serialized into the `vm_state` blob per INTEGRATION.md §4 ("vCPU: ... relevant MSRs") — rather than value synthesis, and every deny below follows the §1 mechanism (MSR-filter trap to userspace via `KVM_CAP_X86_USER_SPACE_MSR`/`KVM_MSR_EXIT_REASON_FILTER`, logged with MSR index and RIP, then #GP injected — `deny-ignore-write` likewise logs loudly before dropping). Class membership is mechanically checkable as the union of: (i) the exact names `MSR_EFER`, `MSR_STAR`, `MSR_LSTAR`, `MSR_CSTAR`, `MSR_SYSCALL_MASK` (IA32_FMASK), `MSR_IA32_SYSENTER_CS/ESP/EIP`, `MSR_FS_BASE`, `MSR_GS_BASE`, `MSR_KERNEL_GS_BASE`, `MSR_IA32_CR_PAT` (IA32_PAT), and `MSR_TSC_AUX` in `arch/x86/include/asm/msr-index.h` at v6.18.35 (`MSR_TSC_AUX` matches the rule but is disposed in the TSC-plumbing fragment because of its RDTSCP coupling — not duplicated here); (ii) the non-time, non-speculation, non-PT, non-debug members of `arch/x86/kvm/x86.c:msrs_to_save_base` (x86.c:331–356) — the SYSENTER/SYSCALL block, IA32_PAT, `MSR_VM_HSAVE_PA`, `MSR_IA32_BNDCFGS`, `MSR_IA32_XFD/XFD_ERR/XSS`, and the CET block; `IA32_TSC`, `MSR_TSC_AUX`, `MSR_IA32_FEAT_CTL`, `MSR_IA32_SPEC_CTRL`, `MSR_IA32_TSX_CTRL`, `MSR_IA32_RTIT_*`, `MSR_IA32_UMWAIT_CONTROL`, `MSR_IA32_DEBUGCTLMSR`, and the LASTBRANCH/LASTINT MSRs in that same array belong to sibling fragments; (iii) from `emulated_msrs_all` (x86.c:394–460): `MSR_IA32_MISC_ENABLE`, the `MSR_IA32_MCG_*` trio, `MSR_IA32_SMBASE`, `MSR_MISC_FEATURES_ENABLES`, `MSR_K7_HWCR`; and (iv) two ranges expanded by stated match rules — machine-check banks via `MSR_IA32_MCx_CTL(x) = 0x400+4x` and `MSR_IA32_MCx_CTL2(x) = 0x280+x` (msr-index.h:580, 589) for the frozen bank count, and MTRRs via every name matching `MSR_MTRRfix*` or `MSR_MTRRdefType` (msr-index.h:382–393) plus the variable-range pairs `MTRRphysBase_MSR(n)/MTRRphysMask_MSR(n) = 0x200+2n/0x201+2n` (n=0..7, the eight pairs enumerated by `MSR_MTRRcap` VCNT below) and the capability register `MSR_MTRRcap` (0xfe); and (v) two CPUID-implied MSRs the frozen model advertises and the guest therefore reads — `MSR_IA32_APICBASE` (0x1b, implied by CPUID.1:EDX[9] APIC) and `MSR_MTRRcap` (0xfe, implied by CPUID.1:EDX[12] MTRR) — both added at this revision to close the gate-5 contradictions GPT-5.5's review identified (a CPUID-advertised feature whose MSR had no row). The reference tag v6.18.35 was cross-checked against `guest/linux/versions.lock` (`KERNEL_VERSION=6.18.35`) — they agree. Dispositions use the §3 vocabulary verbatim, one token per direction; `x86.c` and `msr-index.h` citations below are to those two files at v6.18.35, and `SDM` is the Intel Software Developer's Manual.

| MSR | Index | Read | Write | Rationale | Citation |
|---|---|---|---|---|---|
| MSR_EFER | 0xc0000080 | allow-stateful | allow-stateful | architectural: long-mode/NX enable state, captured in vm_state (INTEGRATION §4); reserved-bit and LME-while-paging writes #GP per SDM | msr-index.h:10; x86.c:4029 (set_efer); SDM Vol.4 Tbl 2-2; INTEGRATION.md §4 |
| MSR_STAR | 0xc0000081 | allow-stateful | allow-stateful | architectural: legacy-mode SYSCALL target, vm_state-captured (§4) | x86.c:333 (msrs_to_save_base); msr-index.h:11; SDM Vol.4 Tbl 2-2 |
| MSR_LSTAR | 0xc0000082 | allow-stateful | allow-stateful | architectural: long-mode SYSCALL target, vm_state-captured (§4) | x86.c:335 (msrs_to_save_base, CONFIG_X86_64 block); msr-index.h:12; SDM Vol.4 Tbl 2-2 |
| MSR_CSTAR | 0xc0000083 | allow-stateful | allow-stateful | architectural: compat-mode SYSCALL target, vm_state-captured (§4) | x86.c:335 (msrs_to_save_base, CONFIG_X86_64 block); msr-index.h:13; SDM Vol.4 Tbl 2-2 |
| MSR_SYSCALL_MASK | 0xc0000084 | allow-stateful | allow-stateful | architectural: SYSCALL EFLAGS mask (IA32_FMASK), vm_state-captured (§4) | x86.c:335 (msrs_to_save_base, CONFIG_X86_64 block); msr-index.h:14; SDM Vol.4 Tbl 2-2 |
| MSR_IA32_SYSENTER_CS | 0x174 | allow-stateful | allow-stateful | architectural: SYSENTER target CS, vm_state-captured (§4) | x86.c:332 (msrs_to_save_base); msr-index.h:243; SDM Vol.4 Tbl 2-2 |
| MSR_IA32_SYSENTER_ESP | 0x175 | allow-stateful | allow-stateful | architectural: SYSENTER target ESP, vm_state-captured (§4) | x86.c:332 (msrs_to_save_base); msr-index.h:244; SDM Vol.4 Tbl 2-2 |
| MSR_IA32_SYSENTER_EIP | 0x176 | allow-stateful | allow-stateful | architectural: SYSENTER target EIP, vm_state-captured (§4) | x86.c:332 (msrs_to_save_base); msr-index.h:245; SDM Vol.4 Tbl 2-2 |
| MSR_FS_BASE | 0xc0000100 | allow-stateful | allow-stateful | architectural: FS segment base, vm_state-captured (§4) | msr-index.h:15; SDM Vol.4 Tbl 2-2 |
| MSR_GS_BASE | 0xc0000101 | allow-stateful | allow-stateful | architectural: GS segment base, vm_state-captured (§4) | msr-index.h:16; SDM Vol.4 Tbl 2-2 |
| MSR_KERNEL_GS_BASE | 0xc0000102 | allow-stateful | allow-stateful | architectural: SwapGS GS shadow, vm_state-captured (§4) | x86.c:335 (msrs_to_save_base, CONFIG_X86_64 block); msr-index.h:17; SDM Vol.4 Tbl 2-2 |
| MSR_IA32_CR_PAT | 0x277 | allow-stateful | allow-stateful | architectural: page-attribute state, vm_state-captured (§4); invalid memtype encodings #GP per SDM; behavior-neutral readback under EPT-all-WB since KVM ignores guest MTRRs | x86.c:337 (msrs_to_save_base); msr-index.h:395; SDM Vol.3A (Memory Cache Control, PAT); kernel.org KVM errata (MTRRs, CR0.CD) |
| MSR_IA32_MISC_ENABLE | 0x1a0 | allow-fixed(0x0000000000001801) | deny-ignore-write | §7 CPUID-stability/PMU vectors: frozen masked value, **not** raw `allow-stateful` — an `allow-stateful` row enters the KVM in-kernel filter allow-list, where a guest WRMSR to bit 18 (MWAIT/MONITOR_FSM) makes upstream KVM flip CPUID.1:ECX[3] (x86.c:4082–4104), shifting the frozen CPUID model (the hazard GPT-5.5 flagged). Reads return the constant 0x1801 (bit 0 fast-strings, bits 11/12 BTS/PEBS-unavailable matching no-vPMU; bit 18 MWAIT=0 matching CPUID MONITOR=0; bit 22 limit-CPUID=0; bit 34 XD_DISABLE=0 so NX stays available); writes are dropped and loudly logged so no bit — least of all 18/22 — can ever move. verify_cpu.S's XD_DISABLE read-modify-write reads XD_DISABLE=0 and so makes no effective change; its write is harmlessly dropped. Nothing here enters `vm_state` (frozen config, not state). | x86.c:425 (emulated_msrs_all), 4082–4104 (MWAIT/CPUID coupling — the avoided in-kernel path), 12977 (BTS/PEBS-unavail init); linux-6.18.35 arch/x86/kernel/verify_cpu.S:79-101; SDM Vol.4 Tbl 2-2 |
| MSR_MISC_FEATURES_ENABLES | 0x140 | deny-gp | deny-gp | gate-5 consistency + §7 CPUID-stability: the enumeration bit MSR_PLATFORM_INFO[31] (CPUID-fault capability) is **0** in the frozen model (§3.14), so the guest's X86_FEATURE_CPUID_FAULT is clear and this MSR architecturally does not exist — #GP both directions. This was `allow-stateful` and is the hazard GPT-5.5 flagged: were it allow-listed, a guest WRMSR 0x140 bit 0 (CPUID_FAULT) would make CPL3 CPUID #GP, contradicting §2's "CPUID always returns the frozen model." (KVM in-kernel already #GPs the enabling write when the guest lacks X86_FEATURE_CPUID_FAULT — `kvm_set_msr_common` MSR_MISC_FEATURES_ENABLES — but the contract denies the MSR outright rather than relying on that gate.) The pinned guest never reads it: `init_cpuid_fault()` runs only under X86_FEATURE_CPUID_FAULT, which is unset. | x86.c:432 (emulated_msrs_all), 4285–4291 (CPUID_FAULT guarded by guest cap); arch/x86/kernel/cpu/intel.c (init_cpuid_fault from MSR_PLATFORM_INFO[31]); msr-index.h:1074; §3.14 (MSR_PLATFORM_INFO bit 31 = 0) |
| MSR_IA32_APICBASE (IA32_APIC_BASE) | 0x1b | allow-fixed(0x00000000FEE00900) | deny-ignore-write | §7 x2APIC MSR surface + boot-critical: CPUID.1:EDX[9]=1 advertises the LAPIC, so the guest's APIC bring-up RDMSRs 0x1b — denying it outright (#GP) would both break early APIC setup and contradict the advertised feature (the gap GPT-5.5 flagged). Read returns the frozen value: base 0xFEE00000, **BSP**=1 (bit 8), **EN** (xAPIC global enable)=1 (bit 11), **EXTD** (x2APIC enable)=0 (bit 10) — the guest sees a fixed, already-enabled xAPIC at the canonical MMIO base, never a host-derived value. Writes are dropped and loudly logged: the base/enable are frozen (no relocation, no disable), and an attempt to set EXTD is dropped so x2APIC can never be entered — which keeps the §3.12 carve-out's "EXTD reserved, x2APIC unreachable" claim enforced **by value** (not by the unfilterable-MSR path), and the 0x800–0x8FF block independently #GPs regardless. The frozen value is config (hashed per §6), not `vm_state`. | Intel SDM Vol.3A ch.11 (IA32_APIC_BASE layout) + Vol.4 Table 2-2 (1BH); linux-6.18.35 arch/x86/kvm/x86.c (kvm_get_apic_base/kvm_set_apic_base — serviced from vcpu->arch.apic_base, reachable via the MSR filter as a normal MSR, *not* in the 0x800–0x8FF unfilterable carve-out); arch/x86/kvm/lapic.c (kvm_apic_set_base folds X2APIC_ENABLE into reserved_bits when CPUID lacks x2APIC); INTEGRATION.md §7 (x2APIC MSR surface); §3.12 (x2APIC hidden) |
| MSR_MTRRcap (IA32_MTRRCAP) | 0xfe | allow-fixed(0x0000000000000508) | deny-gp | architectural + gate-5 consistency: CPUID.1:EDX[12]=1 advertises MTRR, so the guest RDMSRs 0xfe for the variable-range count — a missing row would leave it `deny-gp` and contradict the advertised feature (the gap GPT-5.5 flagged), and a passthrough would leak host MTRR topology. Frozen value: VCNT=8 (bits 7:0 = 0x08, matching the eight PHYSBASE/PHYSMASK pairs 0x200–0x20F below), **FIX**=1 (bit 8, fixed-range MTRRs present), **WC**=1 (bit 10, write-combining), **SMRR**=0 (bit 11, no SMM). Read-only register — write #GPs architecturally. KVM reports the analogous value (`KVM_NR_VAR_MTRR`=8, FIX+WC). Config, not `vm_state`. | msr-index.h:381 (MSR_MTRRcap); arch/x86/kvm/mtrr.c (KVM_NR_VAR_MTRR=8, FEATURE_MTRR/FIX/WC); SDM Vol.3A (MTRR capability register) + Vol.4 Table 2-2 (FEH); §2 leaf-1 EDX row (MTRR=1) |
| MTRRs (PHYSBASE/PHYSMASK pairs 0x200–0x20F, fixed-range 0x250/0x258/0x259/0x268–0x26F, MTRRdefType 0x2FF) | `0x200–0x20F, 0x250, 0x258, 0x259, 0x268–0x26F, 0x2FF` (28 explicit indices — **not** the contiguous block 0x200–0x2FF, so no overlap with MCi_CTL2 0x280–0x289; the TOML carries them as `index-members`) | allow-stateful | allow-stateful | architectural, emulated-state-only: KVM ignores guest MTRR settings under EPT (all guest RAM WB), so the values are pure vm_state readback with no behavioral divergence possible; reset values frozen all-zero (MTRRdefType.E=0); IA32_PAT 0x277 is governed by its own row above; assumes CPUID.1:EDX[12]=1 in the frozen model (gate-5 cross-check at fragment assembly) | kernel.org/doc/html/latest/virt/kvm/x86/errata.html (MTRRs, CR0.CD); arch/x86/kvm/mtrr.c (pure-state vMTRR); msr-index.h:382–393; arch/x86/include/asm/mtrr.h MTRRphysBase_MSR(n); x86.c:4059 |
| MSR_IA32_SMBASE | 0x9e | deny-gp | deny-gp | architectural: host-initiated-only even in upstream KVM (guest access #GPs); this VMM has no SMM, so the register must not exist | x86.c:429 (emulated_msrs_all), 4106–4109 (wrmsr), 4497 (rdmsr); msr-index.h:948 |
| MSR_VM_HSAVE_PA | 0xc0010117 | deny-gp | deny-gp | architectural default-deny: AMD SVM host-save-area PA; AMD and nested virtualization are out of scope (task 06 non-goals) and SVM is never enumerated | x86.c:337 (msrs_to_save_base); msr-index.h:1258; AMD APM Vol.2 (SVM) |
| MSR_K7_HWCR | 0xc0010015 | deny-gp | deny-gp | §7 TSC-plumbing + default-deny: AMD-only config MSR whose bit 24 (TscFreqSel) is a TSC-readback knob; must not exist on the frozen Intel model | x86.c:458 (emulated_msrs_all); msr-index.h:860 |
| MSR_IA32_BNDCFGS | 0xd90 | deny-gp | deny-gp | architectural default-deny: MPX hidden in the frozen CPUID model (CPUID.7.0:EBX[14]=0; Intel-deprecated feature); keeps the §4 XSAVE capture minimal | x86.c:338 (msrs_to_save_base), 7669; msr-index.h:925 |
| MSR_IA32_XFD | 0x1c4 | deny-gp | deny-gp | architectural default-deny: AMX/XFD hidden (CPUID.D.1:EAX[4]=0); no dynamic-XSTATE arming surface; upstream KVM also #GPs without X86_FEATURE_XFD | x86.c:348 (msrs_to_save_base), 4293–4299; msr-index.h:929 |
| MSR_IA32_XFD_ERR | 0x1c5 | deny-gp | deny-gp | architectural default-deny: follows MSR_IA32_XFD (#NM error reporting for a hidden feature) | x86.c:348 (msrs_to_save_base); msr-index.h:930 |
| MSR_IA32_XSS | 0xda0 | deny-gp | deny-gp | architectural default-deny: XSAVES not enumerated (CPUID.D.1:EAX[3]=0) and no supervisor XSTATE exposed; upstream KVM #GPs identically without XSAVES; see [question] | x86.c:348 (msrs_to_save_base), 4123–4131; msr-index.h:931 |
| MSR_IA32_U_CET | 0x6a0 | deny-gp | deny-gp | architectural default-deny: CET hidden (CPUID.7.0:ECX[7]=0, EDX[20]=0); shadow-stack state would grow vm_state for no benefit; see [question] | x86.c:350 (msrs_to_save_base); msr-index.h:517 |
| MSR_IA32_S_CET | 0x6a2 | deny-gp | deny-gp | architectural default-deny: CET hidden; see [question] | x86.c:350 (msrs_to_save_base); msr-index.h:518 |
| MSR_IA32_PL0_SSP | 0x6a4 | deny-gp | deny-gp | architectural default-deny: CET hidden; see [question] | x86.c:351 (msrs_to_save_base); msr-index.h:529 |
| MSR_IA32_PL1_SSP | 0x6a5 | deny-gp | deny-gp | architectural default-deny: CET hidden; see [question] | x86.c:351 (msrs_to_save_base); msr-index.h:530 |
| MSR_IA32_PL2_SSP | 0x6a6 | deny-gp | deny-gp | architectural default-deny: CET hidden; see [question] | x86.c:351 (msrs_to_save_base); msr-index.h:531 |
| MSR_IA32_PL3_SSP | 0x6a7 | deny-gp | deny-gp | architectural default-deny: CET hidden; see [question] | x86.c:352 (msrs_to_save_base); msr-index.h:532 |
| MSR_IA32_INT_SSP_TAB | 0x6a8 | deny-gp | deny-gp | architectural default-deny: CET hidden (interrupt SSP table); see [question] | x86.c:352 (msrs_to_save_base); msr-index.h:533 |
| IA32_MCG_CAP | 0x179 | deny-gp | deny-gp | architectural + gate-5 consistency: MCE[7] and MCA[14] are **cleared** in the frozen CPUID.1:EDX (§2), so the entire machine-check MSR surface architecturally does not exist and every access #GPs — exposing a fixed bank count while CPUID hides MCE was the contradiction GPT-5.5 flagged. The task-04 guest builds without `CONFIG_X86_MCE`, so it never probes these; host machine-check topology never reaches the guest. | msr-index.h:247; arch/x86/kvm/x86.c (kvm_set_msr_common/kvm_get_msr_common MCG handling); SDM Vol.3B ch.15 (Machine-Check Architecture); §2 leaf-1 EDX row (MCE/MCA=0) |
| MSR_IA32_MCG_STATUS | 0x17a | deny-gp | deny-gp | architectural + gate-5 consistency: MCE/MCA hidden in CPUID.1:EDX[7,14]=0, so MCG_STATUS does not exist and #GPs both directions; no machine-check event is ever injected and the no-X86_MCE guest never reads it | x86.c:426 (emulated_msrs_all), 3595; msr-index.h:248; SDM Vol.3B ch.15; §2 leaf-1 EDX row |
| MSR_IA32_MCG_CTL | 0x17b | deny-gp | deny-gp | architectural: frozen MCG_CAP.CTL_P=0 (and MCE/MCA hidden in CPUID), so MCG_CTL access #GPs per SDM | x86.c:427 (emulated_msrs_all); msr-index.h:249; SDM Vol.3B ch.15 |
| MSR_IA32_MCG_EXT_CTL | 0x4d0 | deny-gp | deny-gp | architectural: frozen MCG_CAP.LMCE_P=0 (and MCE/MCA hidden in CPUID), so the register does not exist; no LMCE delivery surface | x86.c:428 (emulated_msrs_all); msr-index.h:251; SDM Vol.3B ch.15 |
| IA32_MCi_CTL/STATUS/ADDR/MISC (banks 0–9) | 0x400–0x427 | deny-gp | deny-gp | architectural + gate-5 consistency: MCE/MCA cleared in CPUID.1:EDX[7,14]=0 (§2), so the per-bank MSRs architecturally do not exist and #GP both directions — host machine-check events are real-hardware nondeterminism and must never reach the guest; the no-X86_MCE task-04 guest never touches these banks | msr-index.h:431 (MSR_IA32_MC0_CTL), 580 (MSR_IA32_MCx_CTL match rule); SDM Vol.3B ch.15; §2 leaf-1 EDX row (MCE/MCA=0) |
| IA32_MCi_CTL2 (CMCI, banks 0–9) | 0x280–0x289 | deny-gp | deny-gp | architectural: frozen MCG_CAP.CMCI_P=0 and MCE/MCA hidden in CPUID, so CTL2 registers do not exist; forecloses CMCI threshold/interrupt timing entirely | msr-index.h:588–589 (MSR_IA32_MCx_CTL2 match rule); SDM Vol.3B ch.15 |

[question] MSR_IA32_XSS (0xda0): denied on the assumption that the frozen CPUID model clears CPUID.(EAX=0xD,ECX=1):EAX[3] (XSAVES) and enumerates no supervisor XSTATE bits; if the CPUID fragment exposes XSAVES, this row must flip to allow-stateful with a frozen XSS valid-bit mask and IA32_XSS added to the INTEGRATION.md §4 FPU/XSAVE vm_state capture (contract version bump).

[question] MSR_IA32_U_CET / MSR_IA32_S_CET / MSR_IA32_PL0–PL3_SSP / MSR_IA32_INT_SSP_TAB (0x6a0–0x6a8): denied on the assumption that CET stays hidden (CPUID.7.0:ECX[7]=0 SHSTK, CPUID.7.0:EDX[20]=0 IBT); exposing CET later requires flipping all seven rows to allow-stateful and revisiting the MSR_IA32_XSS row (CET_U/CET_S supervisor states) in the same contract version bump.

**Reference-set note — IA32_PKRS (0x6e1) and IA32_PASID (0xd93) are intentionally *not* rows
(default-deny catch-all applies).** Verified against v6.18.35: **IA32_PKRS is not even defined**
in `arch/x86/include/asm/msr-index.h` (Protected-Keys-for-Supervisor support was reverted
upstream), so it is in none of the KVM static arrays and not in the reference set; and
**IA32_PASID (0xd93) is defined but appears in none** of `msrs_to_save_*` /
`emulated_msrs_all` / `msr_based_features_all_except_vmx` (the token `PASID` does not occur in
`arch/x86/kvm/x86.c`). Neither is named in INTEGRATION.md §7 nor matched by any §3 class rule,
so by the reference-set definition they get **no explicit row** — the §1 default-deny filter
denies-and-logs them (`deny-gp`) by construction. They are also **unreachable** in the frozen
model: PKS is hidden (CPUID.7.0:ECX[31]=0) and KVM treats **CR4.PKS as permanently reserved**
(it is absent from KVM's known-CR4 mask), so the guest can never enable PKRS; ENQCMD/PASID has
no CR4 bit and is gated by the (denied) `MSR_IA32_PASID` with ENQCMD hidden in CPUID. If a
future kernel adds either MSR to a KVM static array, it enters the reference set and gets an
explicit `deny-gp/deny-gp` row in a version bump.

### 3.14 Class `boot-baseline` — frozen machine identity & feature MSRs

The `boot-baseline` class covers feature-identification and capability MSRs that the
task-04 pinned guest kernel (or KVM's own MSR-list machinery) treats as part of the
machine's frozen identity: the feature-control lock, platform/microcode identification,
topology counts, platform frequency info, and the nested-VMX capability range that KVM
enumerates via `KVM_GET_MSR_FEATURE_INDEX_LIST` (`kvm_init_msr_lists` probes
`KVM_FIRST_EMULATED_VMX_MSR..KVM_LAST_EMULATED_VMX_MSR`, x86.c:7764/7791, x86.h:94–95).
None of these MSRs may ever reflect host values: every readable row is `allow-fixed` with
a constant baked into the versioned baseline (hashed into the determinism gate per
§4), and every row in this class is architecturally read-only, so all write
dispositions are `deny-gp` (trapped via the MSR filter + `KVM_MSR_EXIT_REASON_FILTER`,
logged with index and RIP, then #GP injected — never silent). The VMX capability range
0x480–0x491 is denied outright in both directions: the frozen CPUID model hides VMX
(CPUID.1:ECX[5]=0; nested virtualization is out of scope), and on a VMX-less CPU reads of
these MSRs #GP architecturally — exposing them would both leak host VMX capabilities and
violate the CPUID↔MSR consistency gate. Column grammar: dispositions are drawn verbatim
from task 06 §3 (`allow-fixed(value)`, `allow-stateful`, `emulate-vtime`,
`emulate-timerqueue`, `emulate-apic`, `deny-gp`, `deny-ignore-write`); Rationale names the
INTEGRATION.md §7 leak vector closed (or `architectural`); kernel citations are
`file:line` at the pinned tag v6.18.35.

| MSR | Index | Read | Write | Rationale | Citation |
|---|---|---|---|---|---|
| IA32_PLATFORM_ID | 0x17 | allow-fixed(0x0) | deny-gp | CPUID stability: host platform-id/microcode-flags leak; frozen (platform bits 52:50 = 0) in the versioned baseline; microcode loading is disabled in the pinned guest config | Intel SDM Vol.4 Table 2-2 (IA32_PLATFORM_ID); msr-index.h:910 (v6.18.35) |
| MSR_CORE_THREAD_COUNT | 0x35 | allow-fixed(0x0001_0001) | deny-gp | CPUID stability: host core/thread topology leak; pinned to 1 core / 1 thread per PLAN.md ("one vCPU, period"); not in msr-index.h at v6.18.35 — included via the SDM model-specific table | Intel SDM Vol.4 model-specific MSR table (MSR_CORE_THREAD_COUNT); PLAN.md sources-of-nondeterminism table |
| MSR_IA32_FEAT_CTL | 0x3a | allow-fixed(0x1) | deny-gp | CPUID stability: frozen feature surface — lock bit (bit 0) set, VMX-in/outside-SMX and SGX enable bits clear; with the lock set, write-#GP is the architectural behavior, so deny-gp is faithful | x86.c:338 (msrs_to_save_base); msr-index.h:916; Intel SDM Vol.4 Table 2-2 (IA32_FEATURE_CONTROL) |
| MSR_PLATFORM_INFO | 0xce | allow-fixed(0x0000000000001400) — bits 15:8 = 0x14, the frozen max non-turbo ratio = 2.0 GHz / 100 MHz, all other bits 0 | deny-gp | Power/frequency: hides host base/turbo ratios; ratio field is pinned to the same frozen TSC frequency as CPUID 0x15/0x16; bit 31 = 0 (no CPUID-fault support advertised, keeping MISC_FEATURES_ENABLES unimplied); turbo/TDP fields zeroed | x86.c:431 (emulated_msrs_all), x86.c:475 (msr_based_features_all_except_vmx); msr-index.h:98; Intel SDM Vol.4 Table 2-2 (MSR_PLATFORM_INFO) |
| MSR_IA32_VMX_BASIC | 0x480 | deny-gp | deny-gp | CPUID stability: VMX hidden in frozen model (CPUID.1:ECX[5]=0), so #GP is architectural; host VMX capability values never reach the guest | x86.c:445 (emulated_msrs_all), x86.c:7791 (kvm_init_msr_lists VMX probe); x86.h:94; msr-index.h:1216; Intel SDM Vol.3D Appendix A.1 |
| MSR_IA32_VMX_PINBASED_CTLS | 0x481 | deny-gp | deny-gp | CPUID stability: VMX hidden in frozen model; #GP architectural on VMX-less CPU; in reference set only via the kvm_init_msr_lists VMX probe range | x86.c:7791 (kvm_init_msr_lists VMX probe range); x86.h:94–95; msr-index.h:1217; Intel SDM Vol.3D Appendix A.3.1 |
| MSR_IA32_VMX_PROCBASED_CTLS | 0x482 | deny-gp | deny-gp | CPUID stability: VMX hidden in frozen model; #GP architectural on VMX-less CPU; in reference set only via the kvm_init_msr_lists VMX probe range | x86.c:7791 (kvm_init_msr_lists VMX probe range); x86.h:94–95; msr-index.h (0x482); Intel SDM Vol.3D Appendix A.3.2 |
| MSR_IA32_VMX_EXIT_CTLS | 0x483 | deny-gp | deny-gp | CPUID stability: VMX hidden in frozen model; #GP architectural on VMX-less CPU; in reference set only via the kvm_init_msr_lists VMX probe range | x86.c:7791 (kvm_init_msr_lists VMX probe range); x86.h:94–95; msr-index.h (0x483); Intel SDM Vol.3D Appendix A.4 |
| MSR_IA32_VMX_ENTRY_CTLS | 0x484 | deny-gp | deny-gp | CPUID stability: VMX hidden in frozen model; #GP architectural on VMX-less CPU; in reference set only via the kvm_init_msr_lists VMX probe range | x86.c:7791 (kvm_init_msr_lists VMX probe range); x86.h:94–95; msr-index.h (0x484); Intel SDM Vol.3D Appendix A.5 |
| MSR_IA32_VMX_MISC | 0x485 | deny-gp | deny-gp | CPUID stability + timer devices: VMX hidden; additionally carries the VMX preemption-timer rate (a host-TSC-derived timing parameter), which must never reach the guest | x86.c:450 (emulated_msrs_all), x86.c:7791; msr-index.h (0x485); Intel SDM Vol.3D Appendix A.6 |
| MSR_IA32_VMX_CR0_FIXED0 | 0x486 | deny-gp | deny-gp | CPUID stability: VMX hidden in frozen model; #GP architectural on VMX-less CPU | x86.c:451 (emulated_msrs_all), x86.c:7791; msr-index.h (0x486); Intel SDM Vol.3D Appendix A.7 |
| MSR_IA32_VMX_CR0_FIXED1 | 0x487 | deny-gp | deny-gp | CPUID stability: VMX hidden in frozen model; #GP architectural on VMX-less CPU; in reference set only via the kvm_init_msr_lists VMX probe range | x86.c:7791 (kvm_init_msr_lists VMX probe range); x86.h:94–95; msr-index.h (0x487); Intel SDM Vol.3D Appendix A.7 |
| MSR_IA32_VMX_CR4_FIXED0 | 0x488 | deny-gp | deny-gp | CPUID stability: VMX hidden in frozen model; #GP architectural on VMX-less CPU | x86.c:452 (emulated_msrs_all), x86.c:7791; msr-index.h (0x488); Intel SDM Vol.3D Appendix A.8 |
| MSR_IA32_VMX_CR4_FIXED1 | 0x489 | deny-gp | deny-gp | CPUID stability: VMX hidden in frozen model; #GP architectural on VMX-less CPU; in reference set only via the kvm_init_msr_lists VMX probe range | x86.c:7791 (kvm_init_msr_lists VMX probe range); x86.h:94–95; msr-index.h (0x489); Intel SDM Vol.3D Appendix A.8 |
| MSR_IA32_VMX_VMCS_ENUM | 0x48a | deny-gp | deny-gp | CPUID stability: VMX hidden in frozen model; #GP architectural on VMX-less CPU | x86.c:453 (emulated_msrs_all), x86.c:7791; msr-index.h (0x48a); Intel SDM Vol.3D Appendix A.9 |
| MSR_IA32_VMX_PROCBASED_CTLS2 | 0x48b | deny-gp | deny-gp | CPUID stability: VMX hidden in frozen model; #GP architectural on VMX-less CPU | x86.c:454 (emulated_msrs_all), x86.c:7791; msr-index.h (0x48b); Intel SDM Vol.3D Appendix A.3.3 |
| MSR_IA32_VMX_EPT_VPID_CAP | 0x48c | deny-gp | deny-gp | CPUID stability: VMX hidden in frozen model; #GP architectural on VMX-less CPU | x86.c:455 (emulated_msrs_all), x86.c:7791; msr-index.h (0x48c); Intel SDM Vol.3D Appendix A.10 |
| MSR_IA32_VMX_TRUE_PINBASED_CTLS | 0x48d | deny-gp | deny-gp | CPUID stability: VMX hidden in frozen model; #GP architectural on VMX-less CPU | x86.c:446 (emulated_msrs_all), x86.c:7791; msr-index.h:1229; Intel SDM Vol.3D Appendix A.3.1 |
| MSR_IA32_VMX_TRUE_PROCBASED_CTLS | 0x48e | deny-gp | deny-gp | CPUID stability: VMX hidden in frozen model; #GP architectural on VMX-less CPU | x86.c:447 (emulated_msrs_all), x86.c:7791; msr-index.h:1230; Intel SDM Vol.3D Appendix A.3.2 |
| MSR_IA32_VMX_TRUE_EXIT_CTLS | 0x48f | deny-gp | deny-gp | CPUID stability: VMX hidden in frozen model; #GP architectural on VMX-less CPU | x86.c:448 (emulated_msrs_all), x86.c:7791; msr-index.h:1231; Intel SDM Vol.3D Appendix A.4 |
| MSR_IA32_VMX_TRUE_ENTRY_CTLS | 0x490 | deny-gp | deny-gp | CPUID stability: VMX hidden in frozen model; #GP architectural on VMX-less CPU | x86.c:449 (emulated_msrs_all), x86.c:7791; msr-index.h:1232; Intel SDM Vol.3D Appendix A.5 |
| MSR_IA32_VMX_VMFUNC | 0x491 | deny-gp | deny-gp | CPUID stability: VMX hidden in frozen model; #GP architectural on VMX-less CPU | x86.c:456 (emulated_msrs_all), x86.c:7791; msr-index.h:1233; x86.h:95; Intel SDM Vol.3D Appendix A.11 |

[question] MSR_PLATFORM_INFO (0xce): the frozen max non-turbo ratio in bits 15:8 must equal
the frozen TSC frequency chosen by the CPUID 0x15/0x16 rows of the CPUID-model fragment
(ratio = frozen-TSC-Hz / 100 MHz). The disposition (allow-fixed read / deny-gp write) is
decided; the concrete numeric constant must be filled in at fragment-merge time from the
CPUID model's frozen frequency and then hashed into the versioned baseline per §6.
**[resolved at assembly]** Filled in from §2's frozen frequencies (CPUID 0x15/0x16:
TSC = 2.0 GHz, bus = 100 MHz): ratio = 20 = 0x14 in bits 15:8, all other bits 0, so
the frozen value is `0x0000000000001400`.

[question] MSR_CORE_THREAD_COUNT (0x35) is absent from arch/x86/include/asm/msr-index.h at
the pinned v6.18.35 tag (it is SDM-documented but model-specific), so it falls outside the
mechanically-checkable reference-set definition in task 06 §3 (KVM arrays + §7 names +
msr-index.h-matched classes). The row is kept with a safe allow-fixed(0x0001_0001) /
deny-gp disposition; confirm at merge whether it stays in the contract (recommended — the
guest may probe it on the frozen Intel model) or is dropped to keep the reference set
strictly mechanical.

### 3.15 Class `other` — host-identity inventory (PPIN)

Host-identity / silicon-inventory MSRs that fit none of the named time, power, or perf
classes: the Protected Processor Inventory Number pair. Match rule against
`arch/x86/include/asm/msr-index.h` @ v6.18.35: every name matching `MSR_PPIN*` — exactly
`MSR_PPIN_CTL` (0x4E, msr-index.h:92) and `MSR_PPIN` (0x4F, msr-index.h:93). The AMD twins
`MSR_AMD_PPIN_CTL`/`MSR_AMD_PPIN` (0xC00102F0/F1, msr-index.h:635-636) do not match the
rule and AMD is a task-06 non-goal; they remain covered by the contract §1 default-deny
filter. Reference-set membership is via clause (c) only: neither index appears in any of
KVM's static arrays at v6.18.35 (`msrs_to_save_base`, `msrs_to_save_pmu`,
`emulated_msrs_all`, `msr_based_features_all_except_vmx` — verified by grep of
`arch/x86/kvm/x86.c`) nor in INTEGRATION.md §7's named list. Blanket policy: **deny-gp on
both directions for both entries**, logged loudly per contract §1. Rationale for the
class: PPIN is a fused, per-silicon serial number — the one MSR whose value is by
definition unique to the physical host — so any exposure, even read-only, plants an
unfreezable host-identifying value in the guest-visible surface: the same guest run on two
hosts diverges, and §7's CPUID-stability mandate (one frozen, versioned, hashed model —
never inherit the host's values) is broken; PPIN_CTL additionally reflects host-BIOS
enable/lockout posture (bit 0 = LockOut, bit 1 = Enable_PPIN), which varies across
machines. Deny is architecturally faithful — the frozen CPUID model hides PPIN
(CPUID.(EAX=07H,ECX=1):EBX[0] = 0, the bit Linux's scattered.c:29 reads) and the
boot-baseline fragment freezes MSR_PLATFORM_INFO (0xCE) with bit 23 (PPIN_CAP) = 0, so on
a CPU that enumerates no PPIN these MSRs #GP (gate-5 consistency: no half-exposed
feature). It is also boot-safe and necessary independent of CPUID hiding: Linux
model-matches legacy Xeons with *no* CPUID enumeration (`ppin_cpuids`, common.c:120) and
probes with `rdmsrq_safe`/`wrmsrq_safe` in `ppin_init` (common.c:140), cleanly clearing
the feature on #GP — so the injected #GP, not leaf masking, is the enforcement that holds
for any frozen model choice, and the task-04 guest boots unaffected. The guest can never
establish state in this class, so nothing is captured in `vm_state` (INTEGRATION.md §4).

| MSR | Index | Read | Write | Rationale | Citation |
|---|---|---|---|---|---|
| MSR_PPIN_CTL | 0x4E | deny-gp | deny-gp | §7 CPUID stability: enable/lockout control for the host silicon serial — readback leaks host-BIOS PPIN posture (varies per machine, unfreezable), and a write could arm PPIN reads; denying both directions keeps PPIN permanently unreachable, and #GP is what Linux's rdmsrq_safe/wrmsrq_safe probe expects on a non-PPIN part, so the task-04 guest boots clean. | msr-index.h:92 @ v6.18.35; absent from arch/x86/kvm/x86.c static MSR arrays @ v6.18.35 (clause-c entry); arch/x86/kernel/cpu/common.c:120,140 (ppin_cpuids, ppin_init; bit 0 LockOut / bit 1 Enable) @ v6.18.35; Intel SDM Vol 4 Table 2-2 (MSR_PPIN_CTL); lwn.net/Articles/880824; intel/ModernFW Ppin.c |
| MSR_PPIN | 0x4F | deny-gp | deny-gp | §7 CPUID stability: the Protected Processor Inventory Number is a unique per-silicon serial — reading it hands the guest the physical host's identity, a value that differs across hosts and therefore can never be part of a frozen, hashed determinism surface; writes are architecturally meaningless (read-only MSR) and likewise #GP. Architecturally consistent with PPIN_CAP = 0 (CPUID.(07H,1):EBX[0] hidden; MSR_PLATFORM_INFO[23] = 0 per the boot-baseline fragment's 0xCE row). | msr-index.h:93 @ v6.18.35; absent from arch/x86/kvm/x86.c static MSR arrays @ v6.18.35 (clause-c entry); arch/x86/kernel/cpu/scattered.c:29 (CPUID.(07H,1):EBX[0] enumeration) @ v6.18.35; Intel SDM Vol 4 Table 2-2 (MSR_PPIN, MSR_PLATFORM_INFO[23] PPIN_CAP); lwn.net/Articles/880824; intel/ModernFW Ppin.c; cross-ref fragment msr-boot-baseline.md (MSR_PLATFORM_INFO 0xCE, bit 23 = 0) |

## 4. Instruction & VMX-control dispositions

CPUID hiding does not stop a kernel-mode guest from *executing* an instruction: the silicon
honors an instruction by its **physical** CPUID/enable state, not the virtualized
`KVM_SET_CPUID2` model, so every timing/entropy/perf instruction needs a real enforcement
decision — a VMX execution control that exits, a #UD/#GP that the architecture produces when
the relevant enable is off, or permitted-with-emulation — and a stated result. This section
is that table. It is consistent with PLAN.md's trap table (RDTSC → f(V-time),
RDRAND/RDSEED → seeded stream, RDPMC → trap, HLT → idle-skip per INTEGRATION.md §3) and with
the §2 CPUID model (every feature bit's instructions are dispositioned here, gate 5).

**Mechanism vocabulary (normative — closed set, identical in markdown, §6, and the TOML).**
Each row's mechanism is exactly one of these six tokens; the per-row determinism class
(§1.1: `arch`/`fault-absent`/`intercept`/`invisible`/`scope`) follows from the mechanism as
noted:

- `vmx-exit(<control>)` — a VMX execution control / unconditional exit forces a VM-exit; the
  VMM services it and supplies the result. Controls referenced: RDTSC-exiting, RDPMC-exiting,
  HLT-exiting, MONITOR-exiting, MWAIT-exiting, RDRAND-exiting, RDSEED-exiting (SDM Vol.3C
  §25.6.2), the CPUID and VMCALL unconditional exits (§25.1.2), and the XSETBV unconditional
  exit (§25.1.3). ⇒ class `intercept`. **Note:** the `RDTSC-exiting`, `RDRAND-exiting`, and
  `RDSEED-exiting` controls are **not surfaced to userspace by stock KVM** — those rows depend
  on the §1/[question]-Backend VMX backend; the rest (CPUID/HLT/RDPMC/MONITOR/MWAIT/XSETBV)
  are stock-serviceable. **VMCALL's `vmx-exit(vmcall-unconditional)` dispatch is the
  determinism-backend (patched/direct-VMX) disposition, *not* stock-serviceable** — stock KVM
  services `VMCALL` in-kernel (`-ENOSYS` for our magic, no userspace exit). On stock KVM the
  hypercall channel instead rides a port-I/O **doorbell** (`KVM_EXIT_IO`, INTEGRATION.md §1,
  task 20), which carries no contract row (it is a transport detail, not a hashed insn). The
  determinism/conformance corpus's **report channel** (`OUT 0x0CA2`, INTEGRATION.md §1.1, task 28)
  is likewise transport/observability only — a documentary `[ports]` row in
  `docs/cpu-msr-contract.toml`, **not** a §6-hashed row (it carries no per-host input;
  `contract_hash` is unchanged by it).
- `host-pin(<knob>)` — not interceptable per-instruction; a host-side MSR pin set before
  `KVM_RUN` forces a deterministic result (only `host-pin(tsx-ctrl-rtm-disable)`: TSX). The
  pin is a **hashed `host-assert`** (§6), not prose. ⇒ class `intercept`.
- `permit-native` — executes natively; result is a pure function of architectural guest state
  (XGETBV ECX=0; plain XSAVE/XRSTOR/FXSAVE/FXRSTOR given the §2 pinned XCR0 and MXCSR_MASK).
  ⇒ class `arch`.
- `fault-absent` — the **baseline host physically lacks** the instruction, so it `#UD`s; vmm-core
  asserts host absence (a `host-assert host-absent <op>` record, §6). Used for RDPID, SERIALIZE,
  SHA, HRESET, PCONFIG, UMWAIT/TPAUSE/UMONITOR and — on the TSX-absent Coffee Lake-S baseline —
  **XBEGIN/XEND/XTEST/XABORT**. (TSX's host-absence assertion is the `host-assert rtm-disabled`
  record, which the probe verifies by reading CPUID.7.0:EBX[11]=0, rather than a per-opcode
  `host-absent` entry — the probe recognizes RTM by its CPUID bit, not by mnemonic.) ⇒ class
  `fault-absent`.
- `cr4-pin(<bit>=0)` — a control-register invariant makes the instruction `#UD` unconditionally,
  for **any** guest. Used for RDPKRU/WRPKRU via `cr4-pin(pke=0)`: hiding PKU (CPUID.7.0:ECX[3]=0)
  makes `CR4.PKE` a KVM-reserved bit (`__cr4_reserved_bits`), so a guest MOV-CR4 setting PKE=1
  `#GP`s and CR4.PKE stays 0, and RDPKRU/WRPKRU `#UD` when CR4.PKE=0 (SDM). A **hard** closure
  (not cooperative-scoped), enforced by `host-assert cr4-force-reserved` (§6). ⇒ class `intercept`.
- `native-uninterceptable` — executes natively, is **not** VMX-trappable and **not** absent on
  the baseline, and its result depends on hidden µarch state; there is no enforcement
  mechanism, so determinism holds only under the §1.2 cooperative-guest model
  (XGETBV ECX=1; XSAVEOPT/XSAVEC/XSAVES/XRSTORS). ⇒ class `scope`.

(The earlier `ud-by-control`, `gp-by-cpuid`, and `permit-emulate` tokens are **removed**: on
the Coffee Lake-S baseline UMWAIT/TPAUSE and RDPID `fault-absent` instead, and XGETBV ECX=1 is
`native-uninterceptable` — the `gp-by-cpuid` claim was the false-fault the review flagged.)

**The determinism domain and per-instruction determinism classes are defined normatively in
§1.1** (Host-homogeneity assumption). Each row below that could observe host or hidden-µarch
state carries its §1.1 class — (a) `arch`, (b) `fault-absent`, (c) `intercept`, (d)
`invisible`, or (e) `scope` — in its Mechanism cell; the class is part of the hashed `insn`
record (§6). The key results §1.1 establishes and this table applies: CPUID-hiding is **not**
an enforcement mechanism (an op that does not consult CPUID runs natively on a host that
implements it); the Coffee Lake-S baseline **physically lacks** RDPID/SERIALIZE/SHA/HRESET/PCONFIG/WAITPKG
**and TSX (RTM/HLE)** (so they are (b) deterministic `#UD`, asserted at VM start); and XGETBV(ECX=1) and the
XSAVE-optimization variants are **present on Coffee Lake-S, uninterceptable, hidden-µarch** → class (e),
governed by the §1.2 cooperative-guest model (the CPUID-respecting payload never emits them);
an adversarial guest executing one is a documented residual risk, **out of scope**. (There is
no opcode scan — see §1.2 for why that mechanism was unsound and removed.)

Column grammar: `| Instruction | Mechanism | Result | Rationale | Citation |`. The canonical
serialized form carries `insn <MNEMONIC> <mechanism> <result> <determinism-class>` per §6.

| Instruction | Mechanism | Result | Rationale | Citation |
|---|---|---|---|---|
| CPUID | `vmx-exit(cpuid-unconditional)` | every (leaf,subleaf) serviced from the §2 frozen model only — never KVM defaults, never host passthrough | CPUID VM-exits unconditionally under VMX; §2 is installed via `KVM_SET_CPUID2` and `KVM_GET_SUPPORTED_CPUID` is never consulted — closes §7 "CPUID stability" | SDM Vol.3C §25.1.2 (CPUID always exits); §2 (frozen model); INTEGRATION.md §7 (CPUID stability) |
| VMCALL | `vmx-exit(vmcall-unconditional)` [class (c) `intercept`] | the instruction VM-exits unconditionally; under the **determinism backend** (patched/direct-VMX) it is dispatched to the hypercall handler (`RAX=0x31504348` + request/response page GPAs → `Dispatcher::dispatch`; a valid frame returns the response length in RAX, a bad magic/GPA returns RAX=0 = `Transport::Error`); never a host-time-dependent result. **On stock KVM `VMCALL` is *not* surfaced** — KVM services it in-kernel (`-ENOSYS` for our magic) and never exits to userspace, so the stock hypercall channel is the port-I/O **doorbell** (INTEGRATION.md §1) instead | VMCALL **exits unconditionally in VMX non-root regardless of CPUID.1:ECX[31]=0** (the hypervisor bit gates discovery, not the instruction), so it must be dispositioned explicitly. The hypercall channel — the in-channel for all guest I/O including the RDRAND/RDSEED entropy stream — is the **port-I/O doorbell** on stock KVM (task 20), or this `VMCALL` variant carrying the same frame semantics on the patched/direct-VMX backend (task 21). Malformed input is serviced as `Transport::Error` (length 0), not #UD, so behavior is fixed. The doorbell port/page GPAs live in INTEGRATION.md §1, **not** here — they carry no hashed contract input | SDM Vol.3C §25.1.2 (VMCALL → VM exit in non-root); linux-6.18.35 arch/x86/kvm/x86.c (kvm_emulate_hypercall → -ENOSYS for unknown nr); INTEGRATION.md §1 (hypercall-doorbell ABI); task 20/21; §4 RDRAND row (entropy channel) |
| VMX/EPT group: VMXON, VMXOFF, VMLAUNCH, VMRESUME, VMPTRLD, VMPTRST, VMCLEAR, VMREAD, VMWRITE, INVEPT, INVVPID | `vmx-exit(vmx-instr-unconditional)` [class (c) `intercept`] | **#UD** injected | VMX is hidden (CPUID.1:ECX[5]=0), so the guest can never enter VMX operation — `CR4.VMXE` is a reserved bit (MOV-to-CR4 setting it #GPs when guest CPUID lacks VMX), and these opcodes VM-exit unconditionally in non-root; KVM does not expose nested VMX (no `KVM_CAP_NESTED_STATE`/`nested=0` for this guest), so it injects **#UD**. The MSR 0x480–0x491 `deny-gp` rows cover the VMX *capability MSRs*; **this row covers the raw opcodes** a CPL0 guest could execute — without it the result would depend on host KVM nested-VMX config. (SVM/`VMRUN`&c are AMD, out of scope; they #UD on the Intel baseline.) | SDM Vol.3C §25.1.2 (VMX instructions VM-exit in non-root) + Vol.3C §23.7 (CR4.VMXE); linux-6.18.35 arch/x86/kvm/vmx/nested.c (nested_vmx_check_permission → #UD when nested/guest-VMX off); §3.14 (MSR_IA32_VMX_* 0x480–0x491 deny-gp); §2 leaf-1 ECX[5]=0 |
| RDTSC | `vmx-exit(RDTSC-exiting)` | `EDX:EAX = VClock::tsc(work)` = 2 × V-ns (frozen 2.0 GHz, §2) | closes §7 "TSC plumbing"; host TSC never reaches the guest — PLAN.md trap table (RDTSC → f(V-time)). **Backend-dependent:** RDTSC-exiting is not surfaced by stock KVM (which would give host-time TSC-offset, not V-time) — requires the [question]-Backend VMX backend (§1). | SDM Vol.3C §25.6.2 (RDTSC exiting); PLAN.md trap table; §1/[question] Backend; consonance/vtime/src/clock.rs (VClock::tsc); §2 (frozen TSC) |
| RDTSCP | `vmx-exit(RDTSC-exiting)` (with "enable RDTSCP"=1, required for the CPUID bit) | `EDX:EAX = VClock::tsc(work)`; `ECX = IA32_TSC_AUX` from `vm_state` (never a host core id) | closes §7 "TSC plumbing"; AUX is `allow-stateful` (§3.3), so even the auxiliary half is deterministic | SDM Vol.3C §25.6.2; CPUID 0x80000001:EDX[27] row (§2); §3.3 (MSR_TSC_AUX allow-stateful); PLAN.md trap table |
| RDPID | `fault-absent` [class (b), §1.1] → **#UD** (no VMX control exists; virtual CPUID.7.0:ECX[22]=0 does **not** itself force #UD — the silicon checks physical RDPID support) | **#UD** | the `det-cfl-v1` baseline is Coffee Lake-S, which **does not implement RDPID** (RDPID was introduced in Ice Lake) — box CPUID.7.0:ECX[22]=0 — so on the homogeneous fleet RDPID #UDs deterministically — the same answer as the hidden CPUID bit. vmm-core asserts at VM start that the host lacks RDPID (host-homogeneity); if a future baseline includes RDPID, this row flips to "executes → `r64 = IA32_TSC_AUX` from `vm_state`" (still deterministic because TSC_AUX is `allow-stateful`, never host-derived) in the same version bump | felixcloutier.com/x86/rdpid; SDM Vol.2 (RDPID — #UD if not supported by the physical CPU; no VMX RDPID control); Intel uarch history (RDPID = Ice Lake+, absent on Coffee Lake-S); §2 leaf-7 ECX row + [question] 4; §3.3 (MSR_TSC_AUX); §4 host-homogeneity assumption |
| RDRAND | `vmx-exit(RDRAND-exiting)` | bytes from the task-01 **seeded PRNG stream** over the port-I/O doorbell hypercall channel (INTEGRATION.md §1; the entropy service is reached through the same doorbell as all guest I/O); CF=1 (success) | closes §7 entropy side door; CPUID.1:ECX[30]=1 (exposed-but-trapped) per PLAN.md — hiding cannot stop kernel-mode execution, trapping can. **Backend-dependent:** RDRAND-exiting is not surfaced by stock KVM (which lets it hit the hardware RNG) — requires the [question]-Backend VMX backend (§1); under stock-KVM-only this would drop to class (e). | SDM Vol.3C §25.6.2 (RDRAND exiting); PLAN.md trap table (RDRAND → seeded stream); §1/[question] Backend; hypercall-proto (entropy service); §2 leaf-1 ECX row |
| RDSEED | `vmx-exit(RDSEED-exiting)` | bytes from the task-01 seeded PRNG stream; CF=1 | closes §7 entropy side door; CPUID.7.0:EBX[18]=1 (exposed-but-trapped); RDSEED executes even with its CPUID bit masked, so the exiting control is the real enforcement. **Backend-dependent:** RDSEED-exiting is not surfaced by stock KVM — requires the [question]-Backend VMX backend (§1). | SDM Vol.3C §25.6.2 (RDSEED exiting); PLAN.md trap table; §1/[question] Backend; §2 leaf-7 EBX row |
| RDPMC | `vmx-exit(RDPMC-exiting)` | **#GP** injected — no vPMU exists (CPUID leaf 0xA version 0) | closes §7 "PMU": the host owns the PMU as the V-time instrument; the guest never sets CR4.PCE and every counter read faults | SDM Vol.3C §25.6.2 (RDPMC exiting); PLAN.md trap table (RDPMC → trap); §2 leaf-0xA row; §3.6 (pmu class) |
| RDPKRU / WRPKRU | `cr4-pin(pke=0)` [class (c) `intercept`] | **#UD** unconditionally (CR4.PKE = 0) | **Hard closure** — *not* cooperative-scoped: PKU is hidden (CPUID.7.0:ECX[3,4]=0), which makes `CR4.PKE` a **KVM-reserved bit** (verified: `__cr4_reserved_bits` adds `X86_CR4_PKE` when the guest lacks `X86_FEATURE_PKU`), so a guest `MOV CR4` setting PKE=1 `#GP`s and CR4.PKE stays 0; RDPKRU/WRPKRU `#UD` whenever CR4.PKE=0 (SDM). Thus the **physical PKRU is never reachable** by any guest — it is also **not** an XCR0 component here (the §2 XCR0 menu is {x87,SSE,AVX}, no PKRU bit 9), so it is never in the XSAVE image / `vm_state`. Hashed as `host-assert cr4-force-reserved [PKE, PKS]` (§6). This is the recommended hard fix the round-7 review asked for. | SDM Vol.2 (RDPKRU/WRPKRU #UD if CR4.PKE=0) + Vol.1 §13 (PKRU = XCR0 bit 9); linux-6.18 arch/x86/kvm/x86.h `__cr4_reserved_bits` (X86_CR4_PKE gated on guest PKU); §2 leaf-7 ECX (PKU/OSPKE=0) + leaf-0xD (XCR0 menu); §6 `host-assert cr4-force-reserved` |
| MONITOR | `vmx-exit(MONITOR-exiting)` → VMM injects **#UD** | #UD (feature hidden: CPUID.1:ECX[3]=0, leaf 5 zeroed) | closes §7 power/timing: MONITOR address-watch is a real-time channel; the exit lets the VMM inject the #UD that matches the hidden feature | SDM Vol.3C §25.6.2 (MONITOR exiting) + Vol.2 (MONITOR #UD when CPUID.1:ECX[3]=0); §2 leaf-1 ECX / leaf-5 rows |
| MWAIT | `vmx-exit(MWAIT-exiting)` → VMM injects **#UD** | #UD (feature hidden) | closes §7 power/timing: MWAIT C-state hints idle on real time; #UD matches the hidden MONITOR/MWAIT feature and the frozen MISC_ENABLE bit 18=0 | SDM Vol.3C §25.6.2 (MWAIT exiting); §2 leaf-1 ECX row; §3.13 (MISC_ENABLE 0x1a0) |
| UMWAIT / TPAUSE / UMONITOR | `fault-absent` [class (b), §1.1] → **#UD** | **#UD** | WAITPKG is **physically absent on Coffee Lake-S** (introduced Tremont/Alder Lake; box CPUID.7.0:ECX[5]=0), so these #UD deterministically and vmm-core asserts host absence; user-level waits would otherwise arm real-TSC deadlines and leak timeout-vs-wake. On a future WAITPKG-capable baseline this becomes class (c): vmm-core must clear the VMX "enable user wait and pause" control so they #UD. | SDM Vol.3C §25.6 (enable user wait and pause) + Vol.2 (TPAUSE/UMWAIT #UD); Intel uarch (WAITPKG = Tremont/Alder Lake+, absent on Coffee Lake-S); §1.1 (determinism classes); §2 leaf-7 ECX row; §3.4 (IA32_UMWAIT_CONTROL) |
| XGETBV (ECX=0) | `permit-native` | `EDX:EAX = XCR0` (guest XCR0 ∈ {0x1,0x3,0x7}) — deterministic | architectural: XGETBV(0) reads guest XCR0, a pure function of guest state | SDM Vol.1 §13.3; felixcloutier.com/x86/xgetbv; §2 leaf-0xD rows |
| XGETBV (ECX=1) | `native-uninterceptable` [class (e), §1.2] (XGETBV does **not** VM-exit; hiding XGETBV1 in virtual CPUID does not force #UD — Coffee Lake-S **physically implements** XGETBV1 (box CPUID.0xD.1:EAX[2]=1), so `mov ecx,1; xgetbv` executes and returns XINUSE) | XINUSE — **out of scope for adversarial guests** | XINUSE is live µarch init/modified-tracking state: not made deterministic by homogeneity (it varies run-to-run on one core), not interceptable, not absent on Coffee Lake-S → no enforcement mechanism. **Cooperative-guest scope (§1.2):** the pinned Linux kernel reads XCR0 with ECX=0 and only calls `xgetbv(1)` behind the XGETBV1/XSAVES feature it reads from the *virtual* CPUID.0xD.1 (cleared, §2), so it never emits it; busybox does not. No opcode scan is claimed (XGETBV1 shares opcode `0f 01 d0` with XGETBV0). An adversarial guest executing it is a documented residual risk, out of scope. | SDM Vol.1 §13.4.3 (XGETBV ECX=1 → XINUSE); SDM Vol.3C (no XGETBV VM-exit control; only XSETBV exits); §1.2 (cooperative-guest threat model); §2 leaf-0xD.1 row |
| XSETBV | `vmx-exit(XSETBV-unconditional)` | VMM permits `XCR0 ∈ {0x1,0x3,0x7}` (bit 0 set; AVX⇒SSE), else **#GP** | closes §7: pins the XSAVE state menu so the §4 (INTEGRATION) FPU/XSAVE image is canonical | SDM Vol.3C §25.1.3 (XSETBV exits); §2 leaf-0xD.0 row; INTEGRATION.md §4 |
| HLT | `vmx-exit(HLT-exiting)` | **idle-skip** per INTEGRATION.md §3: if `TimerQueue::peek_next()` is `Some`, advance V-time to the deadline and inject; if empty (IF=1) or IF=0, terminal | closes §7 timer vector: HLT must not stall or consume host real time; V-time jumps deterministically to the next scheduled event | SDM Vol.3C §25.6.2 (HLT exiting); INTEGRATION.md §3 (idle-skip); consonance/vtime/src/queue.rs (TimerQueue::peek_next) |
| XBEGIN / XEND / XABORT / XTEST (TSX/RTM) | `fault-absent` [class (b) `fault-absent`, §1.1] → **#UD** | **#UD** — TSX is **physically absent on the Coffee Lake-S baseline** (box CPUID.7.0:EBX[4 HLE, 11 RTM]=0; `IA32_TSX_CTRL` 0x122 `#GP`s — `docs/fragments/cfl-baseline/`), so RTM/HLE opcodes are not decodable and `#UD` natively, deterministically, for **any** guest. No host pin is installed or needed. | **Robust against any guest by silicon absence**, not a pin. The hashed `host-assert rtm-disabled true` (§6) is satisfied by **physical absence** — the probe reads CPUID.7.0:EBX[11] and passes when RTM is absent (XBEGIN `#UD`s); no `IA32_TSX_CTRL` write occurs (the MSR does not exist on the box). *(The previous TSX-present SKX baseline was class (c): it pinned `IA32_TSX_CTRL = RTM_DISABLE\|TSX_CPUID_CLEAR` to force a deterministic always-abort with a fixed EAX — `#UD` was **not** the SKX result. The re-baseline to the TSX-absent box replaces the pin with native `#UD`; the determinism **outcome** — TSX non-usable, deterministically, for any guest — is invariant, only the mechanism changed.)* Resolves §3.9's TSX [question]. | SDM Vol.2 (XBEGIN/XEND/XTEST/XABORT `#UD` when RTM unsupported); box CPUID.7.0:EBX[4,11]=0 + `rdmsr 0x122` #GP (`docs/fragments/cfl-baseline/`); §6 `host-assert rtm-disabled`; §3.9 (MSR_IA32_TSX_CTRL 0x122); rr arXiv:1705.05937; SDM Vol.1 ch.16 (RTM) |
| XSAVEOPT / XSAVEC / XSAVES (+XRSTORS) | `native-uninterceptable` [class (e), §1.2] | save image — **out of scope for adversarial guests** | **Verified present on Coffee Lake-S** (box CPUID.0xD.1:EAX[0..3]=0xf); the init/modified/compaction optimizations make the saved bytes depend on hidden µarch tracking that varies run-to-run on one core — **not** fixed by homogeneity, **not** interceptable (no VMX control), **not** absent on the baseline → no enforcement mechanism. **Cooperative-guest scope (§1.2):** the kernel selects its save instruction at `fpu__init_system_xstate()` from the *virtual* CPUID.0xD.1 it reads (cleared by §2) and thereafter only emits plain `XSAVE`/`FXSAVE`; busybox makes no XSAVE-family calls. No opcode scan is claimed (Linux carries these as CPUID-gated alternatives a scan cannot soundly reason about). An adversarial guest executing one is a documented residual risk, out of scope. | SDM Vol.1 §13.7–13.11 (XSAVEOPT/XSAVEC/XSAVES #UD checks consult *physical* CPUID); §1.2 (cooperative-guest threat model); §2 leaf-0xD.1 row; rr src/RecordSession.cc (CPUID_XSAVEOPT_FLAG disabled) |
| FXSAVE / XSAVE / XRSTOR / FXRSTOR (plain) | `permit-native` | pure function of architectural FPU state given the §2 pinned XCR0 and **MXCSR_MASK = 0x0000FFFF** (asserted host-equal at VM start) | the only nondeterministic byte is MXCSR_MASK at offset 28, pinned in §2's FPU/XSAVE note; otherwise the image is fully determined | SDM Vol.1 §13.4–13.5 + §10.5.1.2 (MXCSR_MASK); §2 (FPU/XSAVE save-image pin) |
| SERIALIZE | `fault-absent` [class (b), §1.1] → **#UD** | **#UD** | SERIALIZE is **physically absent on Coffee Lake-S** (introduced Alder Lake/Sapphire Rapids; box CPUID.7.0:EDX[14]=0) → deterministic #UD; vmm-core asserts host absence. Robustness note: even on a future baseline that has it, it is class (d) `invisible` — pure serialization with **no architectural output**, only a timing/µarch effect V-time hides. | Intel ISE ref 319433 (SERIALIZE = Alder Lake+); §1.1 (determinism classes); §2 leaf-7 EDX row |
| SHA1/SHA256 (SHA-NI) | `fault-absent` [class (b), §1.1] → **#UD** | **#UD** | SHA-NI is **physically absent on Coffee Lake-S** (introduced Ice Lake on the client line; box CPUID.7.0:EBX[29]=0) → deterministic #UD; vmm-core asserts host absence. Robustness note: even on a future baseline that has it, it is class (a) `arch` — the digest is a pure function of the input operands, no hidden µarch input. | SDM Vol.2 (SHA-NI = Ice Lake+); §1.1 (determinism classes); §2 leaf-7 EBX row |
| PCONFIG | `fault-absent` [class (b), §1.1] → **#UD** | **#UD** on the MKTME-free Coffee Lake-S baseline (box CPUID.7.0:EDX[18]=0); if a host implements MKTME, PCONFIG is outside scope and the baseline excludes such hosts | CPUID.7.0:EDX[18]=0 + leaf 0x1B zeroed; MKTME platform-key state is unvirtualizable-deterministically — denied by exclusion | Intel ISE ref 319433 (PCONFIG); §2 leaf-7 EDX / leaf-0x1B rows |
| HRESET | `fault-absent` [class (b), §1.1] → **#UD** | **#UD** | HRESET (history reset) was introduced on Alder Lake / Sapphire Rapids and is **not implemented on Coffee Lake-S** (box has no CPUID leaf 7.1 — max subleaf 0 — so 7.1:EAX[22]=0), so on the homogeneous fleet it #UDs deterministically — matching the hidden CPUID.7.1:EAX[22]=0 and the `IA32_HRESET_ENABLE` (0x17DA) deny (§3.5). vmm-core asserts the host lacks HRESET at VM start; were a future baseline to include it, HRESET resets µarch predictor/Thread-Director history (µarch-state-affecting) and would need a deny/exclusion, not permit | Intel ISE ref 319433/843860 (HRESET = Alder Lake+); §2 leaf-7.1 EAX[22] / leaf-0x20 rows; §3.5 (IA32_HRESET_ENABLE 0x17DA); §4 host-homogeneity assumption |

**Coverage (gate-1 / gate-5 walk).** Every instruction the §2 model exposes or hides as a
timing/entropy/perf vector has a row: CPUID, **VMCALL** (the instruction; the stock-KVM
hypercall channel is the port-I/O doorbell of INTEGRATION.md §1, not VMCALL — see the VMCALL
row), RDTSC/RDTSCP/RDPID
(TSC), RDRAND/RDSEED (entropy), RDPMC (PMU), MONITOR/MWAIT + UMWAIT/TPAUSE (idle/wait timing),
XGETBV(0/1)/XSETBV + the XSAVE family (FPU image), HLT (idle-skip), XBEGIN&c (TSX),
SERIALIZE/SHA/PCONFIG/**HRESET** (presence fingerprints). Each is consistent with PLAN.md's
trap table and with the matching CPUID bit (gate 5: an exposed bit implies its instruction is
dispositioned here; a hidden bit is backed by a control, by baseline-absence #UD, or by the
scoped host-homogeneity assumption).

#### Questions

[question] Instruction-presence residual — the **class-(e) set only: XSAVEOPT/XSAVEC/XSAVES/
XRSTORS and XGETBV(ECX=1)** (verified present on Coffee Lake-S, uninterceptable, hidden-µarch). These
are not interceptable and the silicon does not honor the virtualized CPUID for their #UD
checks, so determinism depends on the §1.2 cooperative-guest model (the CPUID-respecting
payload never emits them). **RDPID/SERIALIZE/SHA/HRESET/PCONFIG/UMWAIT and TSX (XBEGIN/&c) are
NOT in this set — they are class (b) `fault-absent` (#UD by physical absence on Coffee Lake-S),
robust against any guest**; they are listed here only to contrast. Accept the cooperative-guest
boundary for v1 (recommended), or move to a baseline that physically lacks XSAVES/XGETBV1 (turning
the class-(e) set into `fault-absent`)? The safe-by-default posture (no exposure, cooperative
scope) stands until answered.

[question] TSX enforcement (XBEGIN row) — **resolved by the det-cfl-v1 re-baseline.** The
Coffee Lake-S box **physically lacks TSX** (RTM/HLE absent; `IA32_TSX_CTRL` 0x122 `#GP`s), so
XBEGIN/XEND/XTEST/XABORT `#UD` natively — class (b) `fault-absent`, robust against any guest,
**no host pin needed**. vmm-core's `rtm-disabled` host-assert is satisfied by physical absence
(it reads CPUID.7.0:EBX[11]). *(The earlier SKX-baseline default — pin `IA32_TSX_CTRL =
RTM_DISABLE|TSX_CPUID_CLEAR` for a deterministic always-abort — applied only to a TSX-present
host; it is moot on the TSX-absent box.)* This jointly resolves §3.9's TSX [question].

[question] **Backend — the VMX/KVM trap backend (load-bearing; the most consequential open
item).** Two instruction traps the determinism design relies on — `RDTSC/RDTSCP → f(V-time)`
(needs **RDTSC-exiting**) and `RDRAND/RDSEED → seeded stream` (needs **RDRAND/RDSEED-exiting**)
— use VMX execution controls that **stock upstream KVM does not surface to a userspace VMM**
(see §1 "Enforcement backend dependency"). Stock KVM virtualizes the TSC in-kernel via an
offset on the **host** TSC (host real time, not V-time) and lets RDRAND/RDSEED hit the hardware
RNG. PLAN.md asserts these VMX controls are available but names a stock `kvm-ioctls` backend
and never says how the exits are surfaced; INTEGRATION.md §6 defers the kernel-patch question.
The integrator must pick the backend, because it is load-bearing:
- **(a) patched KVM** that exposes RDTSC/RDRAND/RDSEED-exiting to userspace (as the MSR filter
  is exposed) — *recommended; the contract's §4 `vmx-exit(...)` rows for these instructions are
  serviced through that patched userspace path.*
- **(b) direct-VMX / VMCS backend** that owns the VMCS and programs the controls itself — also
  preserves the contract as written.
- **(c) stock KVM only** — then `RDTSC = f(V-time)` is **impossible** (in-kernel TSC offset
  tracks host time) and RDRAND/RDSEED **cannot be intercepted** (they would drop to class (e)
  cooperative-residual, and RDTSC's core determinism guarantee fails). This option **breaks**
  the V-time/entropy determinism and would force a redesign.
**Resolved — Ruling R-Backend (`docs/R-BACKEND.md`): option (a).** `PatchedKvmBackend` is the
ratified determinism backend, decoupled behind a `Backend` trait; stock `KvmBackend` is
bring-up-only (the RDTSC/RDRAND/RDSEED surfaces above are its fail-closed, enumerated holes —
the determinism gate fails loudly on them rather than shipping "determinism with holes"), and
direct-VMX (b) is the preserved max-isolation option. (Originally raised here because PLAN.md
was underspecified on the surfacing mechanism.) The contract is written assuming (a) or (b);
option (c) stock-KVM-only is rejected by the ruling. If a future revision were to revisit it,
§1/§4's RDTSC and RDRAND/RDSEED rows would change. (CPUID/HLT/RDPMC/MONITOR/MWAIT/XSETBV/VMX-opcodes are
all stock-serviceable and carry no such dependency. **`VMCALL` is the exception among the
"intercept" instructions:** its dispatch is patched/direct-VMX-backend-only, so on stock KVM
the hypercall channel rides the port-I/O doorbell of INTEGRATION.md §1 instead — that doorbell
needs no patch and carries no contract row.)

## 5. Timer/time-device surface

Per §7's timer vector — "PIT/HPET/LAPIC-timer state must be fully V-time-driven … no KVM
in-kernel timer devices unless proven V-time-driven" — the contract must dispose of the
**whole** guest-visible time-source surface, not just the timer MSRs of §3. Every device
below is either routed to `TimerQueue`/V-time-backed userspace emulation or made
unreachable; **none is ever backed by a host clock** (`ktime_get()`, host hrtimers, host
TSC, host CMOS). The split-irqchip/in-kernel-LAPIC path is rejected for exactly this reason
(§3.12): its timer runs on host hrtimers. The reference for "what time sources the pinned
guest can touch" is the task-04 kernel config (`guest/linux/config-fragment`) plus the x86
architectural device set.

**Device table.** Column grammar: `| Device | Port / MMIO | Read | Write | Result | Rationale | Citation |`.
Canonical form: `timer <pit|rtc-cmos|hpet|acpi-pm|lapic-timer> <disposition>` per §6.

| Device | Port / MMIO | Read | Write | Result | Rationale | Citation |
|---|---|---|---|---|---|---|
| **PIT (8254) channel 0** | I/O `0x40` | emulate-timerqueue:pit.ch0 | emulate-timerqueue:pit.ch0 | port-I/O VM-exit → userspace 8254 model; `in 0x40` returns the channel-0 count derived from V-time at the frozen 1.193182 MHz input; mode/reload set via 0x43; channel-0 OUT arms a `TimerQueue` deadline injecting IRQ0 at an exact V-time | closes §7 "Timer devices": the PIT is the boot tick source (`CONFIG_HZ_PERIODIC`, no HPET/PM-timer); ticks on V-time, not host time; consistent with the 0x15 crystal chain (§2) | SDM / 8254 datasheet; INTEGRATION.md §7 (Timer devices) + §5 adapter map; guest config-fragment (HZ_PERIODIC, HZ_100); consonance/vtime/src/queue.rs |
| **PIT (8254) channel 1** | I/O `0x41` | emulate-device:pit.ch1 | emulate-device:pit.ch1 | legacy DRAM-refresh channel: counts at 1.193182 MHz of V-time; not used by the pinned guest, but `in 0x41` is bound to the V-time count so it can never read a host-timed value | closes §7 "Timer devices": even the unused channel is V-time-backed, never host time | SDM / 8254 datasheet; INTEGRATION.md §7 (Timer devices) |
| **PIT (8254) channel 2** | I/O `0x42` | emulate-device:pit.ch2 | emulate-device:pit.ch2 | speaker/counter channel: gated by port-0x61 bit 0, mode/reload via 0x43; `in 0x42` returns the V-time-derived count and its OUT (read via 0x61 bit 5) is computed from V-time — this is the channel-2 model the port-0x61 row depends on | closes §7 "Timer devices": channel-2 OUT/count are a classic real-time reference (`outb $0xb0,0x43; outb count,0x42; inb 0x61`); binding them to V-time removes the leak | SDM / 8254 datasheet; INTEGRATION.md §7 (Timer devices); §5 pit-portb row |
| **PIT (8254) command / read-back** | I/O `0x43` | emulate-device:pit.cmd | emulate-device:pit.cmd | the mode/command + read-back register: writes select channel/access-mode/BCD and latch counters; the read-back command (`0xC0`-class) latches count/status of the selected channels from V-time state, so a subsequent `in 0x40/0x41/0x42` returns a V-time-consistent snapshot | closes §7 "Timer devices": read-back/latch must reflect V-time, never a host counter | SDM / 8254 datasheet (read-back command); INTEGRATION.md §7 (Timer devices) |
| **PIT port B / NMI-status (0x61)** | I/O `0x61` | emulate-vtime:pit.portb | emulate-device:pit.portb | **own canonical record `timer pit-portb`** (not collapsed into the `pit` 0x40–0x43 record). port-I/O VM-exit → userspace model; **bit 4 (refresh-clock toggle)** flips every **`pit-refresh-ns` = 15085 ns of V-time** (a §6 header constant), **initial phase 0 at reset** — both hashed, so the toggle sequence is bit-identical across runs (this is the leak GPT-5.5 named: `inb(0x61)` is a classic real-time reference); **bit 5 (timer-2 OUT)** is computed from the V-time channel-2 model; reads return bit 0 (timer-2 gate)/bit 1 (speaker) as last written; writes set the channel-2 gate (bit 0, gating the V-time channel-2 countdown) and speaker-enable (bit 1), parity-error bits 6/7 forced 0 (no host hardware errors) | closes §7 "Timer devices": legacy "System Control Port B" exposes PIT channel-2 output and the refresh-clock toggle, both real-time references on bare metal; binding the toggle period+phase to a hashed V-time constant removes the leak. The pinned guest may probe 0x61 during early timer calibration even though it skips PIT-vs-LAPIC measurement (§2 0x15 chain) | SDM / PC port-B (0x61) reference; INTEGRATION.md §7 (Timer devices); §6 `pit-refresh-ns`; consonance/vtime/src/queue.rs |
| **RTC / CMOS** | I/O `0x70` (index+NMI) / `0x71` (data) | emulate-device:cmos.subtable | emulate-device:cmos.subtable | port-I/O VM-exit → userspace MC146818 model. **The index write must take effect** (the round-1 blanket `deny-ignore-write` was wrong — it broke the `out 0x70,idx; in 0x71` read protocol GPT-5.5 named): a write to **0x70 latches the 7-bit register index + NMI-disable bit into emulated state** (stateful, honored), and the paired 0x71 access is then routed by that latched index per the CMOS register sub-table. Time-of-day registers derive from a **frozen boot epoch + V-time**, never host CMOS; control/status regs are emulated so `out 0x70,0x0C; in 0x71` cannot read a host/KVM RTC. | closes §7 "Timer devices" — the gap the foreman review flagged: with `CONFIG_ACPI=y` and the CMOS RTC not disabled, x86 Linux reads the CMOS clock at boot via `read_persistent_clock64()`→`mach_get_cmos_time()` **independent of the RTC driver**, a direct real-time leak; emulating it against V-time makes the seeded wall-clock deterministic | SDM / MC146818 datasheet; linux-6.18.35 arch/x86/kernel/rtc.c (`mach_get_cmos_time`, `read_persistent_clock64`); INTEGRATION.md §7 (Timer devices); guest config-fragment (CONFIG_ACPI=y) |
| **HPET** | MMIO `0xFED00000` (incl. main counter `+0xF0`, comparators) | deny-gp | deny-gp | region **not mapped** into the guest; any access faults (EPT violation → logged → #GP/abort); no HPET advertised in ACPI (no HPET table) and the boot gate passes `-machine hpet=off` / `hpet=disable` | closes §7 "Timer devices": the HPET main counter is a free-running real-time counter; `CONFIG_HPET_TIMER` is `def_bool y` and cannot be configured out, so the device is kept out **at runtime** by not exposing it — the main-counter MMIO read (`+0xF0`) GPT-5.5 named never reaches a host counter | linux-6.18.35 config-fragment comment (HPET kept out at runtime, `-machine hpet=off`); Intel HPET spec 1.0a; INTEGRATION.md §7 (Timer devices) |
| **ACPI PM timer** | I/O `PM_TMR_BLK` (FADT-advertised) | deny-gp | deny-gp | **not advertised**: the FADT's `PM_TMR_BLK`/`X_PM_TMR_BLK` is 0 so no port is enumerated, and the pinned kernel has `# CONFIG_X86_PM_TIMER is not set` so it is never probed; if accessed anyway the port-I/O exit denies-and-logs | closes §7 "Timer devices": the ACPI PM timer is a 3.579545 MHz free-running real-time counter; config-disabled **and** not advertised removes it doubly | guest config-fragment (`CONFIG_X86_PM_TIMER is not set`); ACPI spec (FADT PM_TMR_BLK); INTEGRATION.md §7 (Timer devices) |
| **LAPIC timer** | xAPIC MMIO page `0xFEE00000` (see sub-table) | emulate-vtime:apic.tmcct | emulate-timerqueue:apic.timer-arm | the whole page is trapped MMIO served by the **userspace xAPIC** (no `KVM_CREATE_IRQCHIP`, x2APIC hidden per §3.12); the timer registers arm/read `TimerQueue` deadlines on V-time | closes §7 "Timer devices": an in-kernel LAPIC timer runs on host hrtimers (`apic_get_tmcct`→`ktime_get()`); the userspace model ticks at exactly 25 MHz of V-time (§2 0x15 chain) | linux-6.18.35 arch/x86/kvm/lapic.c (`apic_get_tmcct` ktime_get — the avoided path); INTEGRATION.md §7 (Timer devices); §2 leaf-0x15 row; §3.12 (x2APIC hidden) |

**CMOS / RTC register sub-table.** The MC146818 model is served entirely from emulated
state; the index write (port 0x70) is honored so the paired data access (port 0x71) reads
the selected register. Dispositions use the §3 vocabulary; `allow-fixed` reads return the
16-hex constant. The pinned guest reads (never writes) the CMOS clock at boot
(`mc146818_get_time` reads Status A UIP, the time registers, and Status B `RTC_DM_BINARY`),
so writes are dropped+logged by default. **IRQ8** (periodic/alarm/update) is never delivered:
no host RTC interrupt reaches the guest; should a future guest enable PIE/AIE/UIE via Status
B, the userspace RTC arms a V-time `TimerQueue` deadline for IRQ8 — never a host-timed event.
Column grammar: `| Register | Index/Port | Read | Write | Rationale |`. Canonical form:
`cmos <port:0xNN|idx:0xNN> <read-token> <write-token>` per §6.

| Register | Index/Port | Read | Write | Rationale |
|---|---|---|---|---|
| Index + NMI-disable port | port `0x70` | emulate-device:cmos.index-latch | emulate-device:cmos.index-latch | **write must take effect**: latches the 7-bit register index (bits 0–6) and the NMI-disable bit (bit 7) into emulated state; read returns the latched index. This is what makes the `out 0x70,idx; in 0x71` protocol work (the round-1 blanket deny-ignore-write broke it) |
| Data port | port `0x71` | emulate-device:cmos.data-window | emulate-device:cmos.data-window | routed by the index latched via port 0x70 |
| Time-of-day | idx `0x00, 0x02, 0x04, 0x06, 0x07, 0x08, 0x09` | emulate-vtime:cmos.tod | deny-ignore-write | seconds/minutes/hours/day-of-week/day/month/year = the **hashed `rtc-epoch` constant + V-time**, in BCD (matching Status B `RTC_DM_BINARY=0`): `rtc-epoch = 1577836800` (2020-01-01T00:00:00Z) is a §6 header constant, so `read_persistent_clock64()` reads a value that is bit-identical across implementations and runs; writes dropped+logged (the boot path never writes) |
| Alarm registers | idx `0x01, 0x03, 0x05` | allow-fixed(0x0000000000000000) | deny-ignore-write | seconds/minutes/hours alarm-match registers read a frozen **0** (no alarm is ever armed; AIE=0 in Status B) — this resolves the round-2 table/Rationale mismatch by making the hashed row `allow-fixed(0)`, not `emulate-vtime` |
| Status Register A | idx `0x0A` | allow-fixed(0x0000000000000026) | deny-ignore-write | **UIP (bit 7) = 0** always (the model is never "mid-update", so `mc146818_get_time`'s UIP spin returns immediately and deterministically); frozen 32.768 kHz time base (bits 6:4=010) + rate 6 (bits 3:0); no periodic IRQ armed |
| Status Register B | idx `0x0B` | allow-fixed(0x0000000000000002) | deny-ignore-write | 24-hour (bit 1=1) + **BCD** (`RTC_DM_BINARY` bit 2 = 0); PIE/AIE/UIE (bits 6/5/4)=0 so no RTC interrupt is ever enabled — the only bit the boot read consumes is `RTC_DM_BINARY` |
| Status Register C | idx `0x0C` | allow-fixed(0x0000000000000000) | deny-ignore-write | IRQ-flag register (read-only, read-clears): PF/AF/UF never set because no RTC interrupt is ever delivered, so it reads a constant 0 — `out 0x70,0x0C; in 0x71` exposes no host/KVM timer state |
| Status Register D | idx `0x0D` | allow-fixed(0x0000000000000080) | deny-ignore-write | VRT (bit 7)=1: RAM and time valid; read-only |
| CMOS RAM | idx `0x0E–0x7F` | allow-fixed(0x0000000000000000) | deny-ignore-write | scratch/config RAM frozen to 0 (not used by the pinned no-RTC-driver guest); default-deny — see [question] CMOS-RAM below if a guest ever needs writable scratch |

[question] CMOS-RAM (idx 0x0E–0x7F): frozen read-0 / drop-write is safe for the pinned guest
(it never uses CMOS scratch). If a future guest needs writable CMOS RAM, flip those indices
to `allow-stateful` and capture them in `vm_state` (contract version bump).

**xAPIC MMIO sub-table.** The §3.12 x2APIC rows deny the MSR aliases (0x800–0x8FF); the
guest's only APIC is this trapped MMIO page at `0xFEE00000`, served by the userspace LAPIC.
**Every guest-observable register is normative and hashed** (the round-1 draft listed only
the four timer offsets in prose, which §6 excludes from the hash — GPT-5.5's finding). All
state-bearing registers are captured in `vm_state` per INTEGRATION.md §4. Column grammar:
`| Register | Offset | Read | Write | Rationale |`. Canonical form: `mmio xapic.<offset>
<read-token> <write-token>` per §6.

| Register | Offset | Read | Write | Rationale |
|---|---|---|---|---|
| APIC ID | `0x020` | allow-fixed(0x0000000000000000) | deny-ignore-write | single-vCPU frozen ID 0 (xAPIC ID in bits 31:24 = 0); never the host's running-core ID (the one nondeterministic value rr pins a core for); writes to the ID are dropped (the ID is frozen) |
| Version (LVR) | `0x030` | allow-fixed(0x0000000000050014) | deny-ignore-write | frozen 0x00050014 = version 14H, max-LVT entries 5 (no CMCI; MCG_CMCI_P=0) — mirrors KVM's `APIC_VERSION \| ((nr_lvt-1)<<16)` with our 6 LVTs (no CMCI), so no host APIC revision leaks; read-only |
| TPR | `0x080` | allow-stateful | allow-stateful | task-priority is pure guest-written state in `vm_state`; no time content |
| PPR | `0x0A0` | emulate-device:apic.ppr | deny-ignore-write | processor-priority is computed deterministically from captured TPR/ISR (read-only) — never a host artifact |
| APR | `0x090` | allow-fixed(0x0000000000000000) | deny-ignore-write | arbitration-priority register: legacy/obsolete on modern LAPIC, reads a frozen 0; writes dropped+logged |
| RRD | `0x0C0` | allow-fixed(0x0000000000000000) | deny-ignore-write | remote-read register: obsolete, reads frozen 0; writes dropped+logged |
| EOI | `0x0B0` | allow-fixed(0x0000000000000000) | emulate-device:apic.eoi | EOI is write-only: a **read returns frozen 0** (not a write token — fixing the round-2 invalid `deny-ignore-write` read), a write retires the highest in-service ISR bit in the userspace model (the one deterministic EOI path) |
| LDR | `0x0D0` | allow-stateful | allow-stateful | logical-destination state in `vm_state`; consulted only by deterministic userspace delivery (trivial on one vCPU) |
| DFR | `0x0E0` | allow-stateful | allow-stateful | destination-format state in `vm_state`; no time content |
| SIVR | `0x0F0` | allow-stateful | allow-stateful | spurious-vector + software-enable in `vm_state`; spurious delivery only at deterministic emulation points |
| ISR | `0x100–0x170` | allow-stateful | deny-ignore-write | in-service bitmaps: a pure function of planner-scheduled V-time injections + guest EOIs, captured in `vm_state` (read-only registers) |
| TMR | `0x180–0x1F0` | allow-stateful | deny-ignore-write | trigger-mode bitmaps set deterministically at delivery time, captured in `vm_state` (read-only) |
| IRR | `0x200–0x270` | allow-stateful | deny-ignore-write | request bitmaps mutate only on planner V-time injections / self-IPIs — never host-timed events (read-only) |
| ESR | `0x280` | emulate-device:apic.esr | emulate-device:apic.esr | error state from deterministic emulation events only (write-then-read protocol) |
| LVT CMCI | `0x2F0` | allow-fixed(0x0000000000000000) | deny-ignore-write | the model's max-LVT is 5 (no CMCI; MCG_CMCI_P=0, §3.12), so this LVT does **not** exist — reads a frozen 0, writes dropped+logged; no corrected-machine-check interrupt is ever delivered |
| ICR low/high | `0x300` / `0x310` | allow-stateful | emulate-device:apic.icr | IPIs on the single vCPU are self/fixed-only, queued to IRR at deterministic points, delivery-status always idle |
| LVT Timer | `0x320` | allow-stateful | emulate-timerqueue:apic.timer-arm | mode (**one-shot/periodic only — TSC-deadline mode unavailable**, CPUID.1:ECX[24]=0)/mask/vector write arms, re-arms, or cancels the `TimerQueue` entry; never a host hrtimer (the `apic_get_tmcct`/`hrtimer_start` path is the one avoided) |
| LVT Thermal | `0x330` | allow-stateful | allow-stateful | writable state; no thermal model exists so the LVT never fires — inert and deterministic |
| LVT PMI | `0x340` | allow-stateful | allow-stateful | writable state; no vPMU (host owns PMU, RDPMC traps) so no counter PMI can ever target the guest |
| LVT LINT0 | `0x350` | allow-stateful | allow-stateful | ExtINT wiring for the userspace PIC; every interrupt it carries is TimerQueue-backed at exact V-time |
| LVT LINT1 | `0x360` | allow-stateful | allow-stateful | NMI pin never pulsed by a host event (no watchdog); an NMI, if ever used, is a planner decision at exact V-time |
| LVT Error | `0x370` | allow-stateful | allow-stateful | vector for errors that arise only from deterministic emulation |
| Initial Count (TMICT) | `0x380` | allow-stateful | emulate-timerqueue:apic.timer-arm | write converts `count × divide × frozen 25 MHz period` to an absolute V-ns deadline on `TimerQueue` (0 disarms; periodic re-arms deterministically); read returns the stored initial count from `vm_state` |
| Current Count (TMCCT) | `0x390` | emulate-vtime:apic.tmcct | deny-ignore-write | **the countdown leak GPT-5.5 named**: KVM computes it from `ktime_get()`; instead `remaining = ticks((deadline_vns − VClock::vns(work)) / tick_vns)`, 0 when unarmed/expired — monotone in retired-branch work, bit-identical on replay; TMCCT is read-only (writes dropped+logged) |
| Divide Config (TDCR) | `0x3E0` | allow-stateful | emulate-timerqueue:apic.timer-arm | divide value (bits 0,1,3) feeds the TMICT/TMCCT tick conversion; a rewrite deterministically recomputes the armed deadline — no hrtimer to restart |
| Self-IPI | `0x3F0` | allow-fixed(0x0000000000000000) | deny-ignore-write | the self-IPI register is **x2APIC-only**; at this xAPIC offset it is reserved (x2APIC hidden, §3.12), so reads a frozen 0 and writes are dropped+logged — self-IPIs use the ICR path |

**Normative MMIO-page default (hashed).** Every xAPIC-page offset **not** listed above is
covered by the canonical record `mmio-default allow-fixed(0x0000000000000000) deny-ignore-write`
(read frozen 0, write dropped+logged) — the default-deny rule applied to the whole 4 KiB
page, matching `kvm_lapic_readable_reg_mask` leaving reserved bits clear. This is a normative,
hashed record (§6), not prose, so the reserved surface is bound by the contract hash.

**TSC-deadline mode is unavailable (round-7, Ruling R1).** CPUID.1:ECX[24]=0 hides
TSC-deadline and MSR `IA32_TSC_DEADLINE` (0x6E0) is `deny-gp` (§3.3) — because the in-kernel
WRMSR fastpath under `KVM_IRQCHIP_NONE` swallows a 0x6E0 write before the MSR filter, so
`emulate-timerqueue` could never service it (Linux's TSC-deadline clockevent would arm but
never fire). The LAPIC timer is the xAPIC LVT **one-shot/periodic** model above (TMICT 0x380 →
`apic.timer-arm`, a V-time-absolute deadline); nothing is lost.

**Coverage (gate-1).** §7 "Timer devices" maps to: PIT, RTC/CMOS, HPET, ACPI PM timer, and
LAPIC timer rows above, plus the §3.12 x2APIC MSR rows and §3.3 TSC-deadline MSR — the whole
guest-visible time-source surface is either V-time-driven or unreachable, none host-clock-backed.

## 6. Versioning & hashing

The contract is a **config artifact** (INTEGRATION.md §7: "one frozen, versioned CPUID
model (a config artifact, hashed into the determinism gate)"). Two runs are only
meaningfully comparable if they ran under bit-identical contracts, so the contract has a
canonical byte representation, a hash, and a version discipline.

**Canonical serialized form (normative).** The canonical form is a UTF-8 byte string,
LF line endings, no comments, no trailing whitespace, derived from the normative tables
only — `Rationale` and `Citation` columns and all prose are excluded (they explain, they
do not bind). It consists of, in order:

1. Header records: `contract-version=<N>`, `kernel-tag=v6.18.35`,
   `cpuid-baseline=<name from §2>`, then the frozen scalar constants (each a decimal or
   16-hex-digit value): `tsc-hz=2000000000`, `crystal-hz=25000000`, `bus-hz=100000000`,
   `mxcsr-mask=0x0000ffff`, `rtc-epoch=1577836800` (the CMOS/RTC boot wall-clock seed,
   2020-01-01T00:00:00Z — §5 makes `read_persistent_clock64()` reproducible), and
   `pit-refresh-ns=15085` (the §5 port-0x61 refresh-toggle half-period in V-ns, with initial
   phase 0 at reset) — one per line, in exactly that order. The frequency constants
   are the single source the CPUID 0x15/0x16, brand string, and MSR_PLATFORM_INFO values
   derive from (§2 [question] 3); `mxcsr-mask` is the FPU/XSAVE save-image pin (§2). They
   are hashed so a half-updated frequency/FPU set changes the bytes.
2. CPUID records: one line per listed (leaf, subleaf):
   `cpuid <leaf>.<subleaf> <eax> <ebx> <ecx> <edx>`. **`<leaf>` and `<subleaf>`** are each
   8 lowercase hex digits (single), `lo-hi` (an inclusive range, both 8-hex — used only for
   the contiguous extended/hypervisor zero blocks), `*` (all subleaves of the leaf), or
   `N+` (subleaf N and above). Comma-separated leaf lists are **expanded to one record per
   leaf** before serialization (no comma-in-field). **Each register field** is either an
   8-lowercase-hex constant **or a dynamic token** for the three cells that are pure
   functions of guest state (so the hash binds the *rule*, not an unknowable value):
   - `dyn:osxsave:<base8hex>` — leaf 1 ECX: `base | (CR4.OSXSAVE << 27)`.
   - `dyn:level-echo:<type8hex>` — leaf 0xB/0x1F past-the-end ECX: `(input_subleaf & 0xff)
     | (type << 8)`.
   - `dyn:xcr0-xsavesize` — leaf 0xD.0 EBX: the XSAVE-area size for the guest's enabled
     XCR0 (0x240 for XCR0∈{0x1,0x3}, 0x340 for 0x7).

   These three rules are a closed, enumerated set; KVM recomputes all three from guest
   state alone (`kvm_update_cpuid_runtime`), so they are deterministic and replay-stable,
   and the canonical form serializes the **rule token verbatim** (it is part of the hashed
   bytes) rather than attempting to resolve a single value. Every other register field is
   resolved to its concrete frozen value. Records sorted ascending by (leaf, subleaf);
   followed by one `cpuid-default zeroed` record (the unlisted-leaf rule: in-range gaps are
   explicit all-zero, out-of-range redirects to the all-zero max-basic-leaf 0x20, §2).
3. MSR records: one line per MSR index:
   `msr <index> <read-token>[:<read-formula>] <write-token>[:<write-formula>]` — index 8
   lowercase hex digits; tokens verbatim from the §3 vocabulary. **A formula id is part of
   the hashed record** (not Rationale): `allow-fixed` carries its 16-hex constant as the
   formula; every `emulate-*` token carries a formula id from the closed, enumerated set
   below, so the *semantics* — including the TSC/TSC_ADJUST coupling — are bound by the hash.
   Range rows are **expanded to one record per index** before serialization. Sorted ascending
   by index; exactly one record per index — a duplicate or conflicting row is a serialization
   **error**, never last-wins.

   **Enumerated emulate formula ids (normative — these definitions are the hashed semantics):**
   - `vclock.tsc` (read) — `EDX:EAX = VClock::tsc(work)` = `tsc_base + floor(vns·tsc_hz/10⁹)`.
   - `vclock.tsc.write` (IA32_TSC write) — rebase `tsc_base = value − floor(vns·tsc_hz/10⁹)`
     and **add the delta `(value − tsc_before)` to `IA32_TSC_ADJUST`** (SDM coherence, gate-5
     with CPUID.7.0:EBX[1]). (No TSC-deadline `TimerQueue` entries exist to recompute —
     IA32_TSC_DEADLINE 0x6e0 is `deny-gp`, §3.3; the LAPIC timer is V-time-absolute one-shot/
     periodic via the xAPIC LVT, §5, independent of the TSC base.)
   - `vclock.tsc_adjust` (read) — `= IA32_TSC_ADJUST` from vm_state.
   - `vclock.tsc_adjust.write` — set `IA32_TSC_ADJUST = value`; shift the TSC offset by
     `(value − adjust_before)` so IA32_TSC moves by the same delta (the reverse coupling).
   - *(removed: `timerqueue.tsc_deadline` / `.write` — TSC-deadline mode is hidden and 0x6e0 is
     `deny-gp` as of round 7, so these formula ids no longer exist.)*

   The **timer / xAPIC-MMIO / CMOS** `emulate*` cells carry a formula id from the **same closed
   set** (a generic bare `emulate` token is **not** permitted — the semantics must be hashed):
   - `pit.ch0` (PIT 0x40) — 8254 channel-0 counter clocked at **1193182 Hz of V-time**,
     mode/access from the `out 0x43` command latch; `in 0x40` returns the latched count derived
     from V-time; terminal count arms an IRQ0 `TimerQueue` deadline. (`emulate-timerqueue`.)
   - `pit.ch1` (PIT 0x41) — channel-1 (legacy DRAM-refresh) counter at 1193182 Hz of V-time;
     unused by the guest but V-time-backed so `in 0x41` never reads host time. (`emulate-device`.)
   - `pit.ch2` (PIT 0x42) — channel-2 counter at 1193182 Hz of V-time, gated by port-0x61 bit 0;
     `in 0x42` returns the V-time count, and its OUT (visible at port-0x61 bit 5) is computed
     from V-time. This is the channel-2 model `pit.portb` reads. (`emulate-device`.)
   - `pit.cmd` (PIT 0x43) — mode/command + read-back register: writes set channel/access/BCD
     and latch counters; the read-back command latches count/status of the selected channels
     from V-time state so a following `in 0x40/0x41/0x42` is V-time-consistent. (`emulate-device`.)
   - `pit.portb` (port 0x61) — bit 4 = refresh-clock toggle flipping every `pit-refresh-ns`
     (15085) of V-time, initial phase 0; bit 5 = channel-2 OUT from `pit.ch2`; bits 0/1
     last-written (bit 0 gates `pit.ch2`); bits 6/7 = 0. (read `emulate-vtime`, write `emulate-device`.)
   - `cmos.index-latch` (port 0x70) — latch register index bits[6:0] + NMI-disable bit[7].
     (`emulate-device`.)
   - `cmos.data-window` (port 0x71) — the data port: reads/writes route to the CMOS register
     selected by the `cmos.index-latch`ed index (the per-index `cmos` records define each
     register's disposition). Replaces the former open `route-by-index` meta-token. (`emulate-device`.)
   - `cmos.subtable` (the `rtc-cmos` timer-device record) — meta-pointer: the RTC/CMOS surface
     is dispositioned by the `cmos` records, not by a single timer disposition. (`emulate-device`.)
   - `cmos.tod` (CMOS idx 0x00/02/04/06–09) — BCD time-of-day = `rtc-epoch + V-time` (header
     `rtc-epoch`). (`emulate-vtime`.)
   - `apic.tmcct` (xAPIC 0x390) — `remaining = ticks((deadline_vns − VClock::vns(work)) /
     tick_vns)`, 0 if unarmed/expired. `apic.timer-arm` (0x320/0x380/0x3E0) — count × divide ×
     frozen 25 MHz-of-V-time period → absolute V-ns `TimerQueue` deadline.
   - `apic.ppr` (xAPIC 0x0A0) — `PPR = max(TPR, ISRV & 0xF0)` from captured TPR/ISR (SDM
     Vol.3A ch.11). `apic.eoi` (0x0B0 write) — clear the highest-priority in-service ISR bit.
     `apic.esr` (0x280) — write-then-read latch of error bits set only by deterministic
     emulation events. `apic.icr` (0x300/0x310 write) — self/fixed IPI queued to IRR at the
     emulation point, delivery-status always idle.
   These ids are a closed set; adding one is a contract change (version bump). The coupling,
   deadline-recompute, PIT/CMOS/APIC math therefore live in the **hashed** canonical form, not
   in Rationale prose (which §6 excludes from the hash).

   **Formula-id immutability rule (normative — closes the hash-vs-semantics gap).** The
   canonical record hashes a formula *id* (e.g. `vclock.tsc.write`, `cmos.tod`,
   `apic.timer-arm`), **not** its definition text, which lives in this §6 prose and is excluded
   from the hashed bytes. To keep `contract_hash` a faithful anchor of guest-visible behaviour,
   **any semantic change to a formula's definition MUST introduce a new formula id (and a
   version bump)** — a formula id, once published, is **immutable in meaning**. Equivalently:
   `<id> → <semantics>` is a frozen, append-only mapping; you never redefine an existing id, you
   add `<id>.v2`/a new name. Editorial/wording-only clarifications of a definition that provably
   do not change the computed value are permitted (like any prose edit) and need no new id. This
   makes the hashed id a sound proxy for the unhashed definition: a behaviour change cannot occur
   under a fixed id, so a fixed `contract_hash` implies fixed formula semantics. (The same
   immutability applies to the `dyn:*` CPUID rule ids and the `insn` `<mechanism>`/`<result>`
   tokens: a meaning change requires a new token.)
4. Instruction records (§4): `insn <MNEMONIC> <mechanism> <result> <determinism-class>` —
   uppercase mnemonics, `<mechanism>` and `<result>` from the §4 vocabulary, and
   `<determinism-class>` from the closed §1.1 set `{arch, fault-absent, intercept, invisible,
   scope}`; sorted lexicographically by mnemonic. The class is hashed, so a change to how an
   op's determinism is justified changes the contract bytes.
5. Timer-device records (§5): `timer <device> <read-token>[:<formula>]
   <write-token>[:<formula>]`, device ∈ the fixed order `pit-ch0, pit-ch1, pit-ch2, pit-cmd,
   pit-portb, rtc-cmos, hpet, acpi-pm, lapic-timer`. Every `emulate*` token **carries a closed
   §6 formula id** (`pit.ch0/ch1/ch2/cmd/portb`, `cmos.subtable`, `apic.tmcct`,
   `apic.timer-arm`) — a bare `emulate` with no formula is invalid. **The PIT is one record per
   port** (0x40/0x41/0x42/0x43) plus `pit-portb` (0x61), so each 8254 channel, the
   command/read-back register, and the port-0x61 refresh-toggle/ch-2-OUT bits are hash-bound
   independently (not collapsed into a single `pit` record); `rtc-cmos` carries `cmos.subtable`
   meaning its surface is dispositioned by the `cmos` records (item 7).
6. xAPIC MMIO sub-table records (§5): `mmio xapic.<offset> <read-token>[:<param>]
   <write-token>[:<param>]` — offset 3 lowercase hex digits; `<param>` is the 16-hex constant
   for `allow-fixed` or a closed §6 `apic.*` formula id for `emulate*` cells (`apic.ppr`,
   `apic.eoi`, `apic.esr`, `apic.icr`, `apic.tmcct`, `apic.timer-arm`); range registers
   (ISR/TMR/IRR) are expanded to one record per 16-byte offset; sorted ascending by offset;
   followed by one `mmio-default <read> <write>` record (`allow-fixed(0) deny-ignore-write`)
   for every page offset not explicitly listed. (The x2APIC MSR aliases 0x800–0x8FF are
   ordinary `msr` records under item 3; there is no separate `apic` record type.)
7. CMOS/RTC records (§5): `cmos <port:0xNN | idx:0xNN | idx:0xLO-0xHI> <read-token>[:<param>]
   <write-token>[:<param>]` — the two ports (0x70/0x71) then the register indices, sorted with
   ports before indices and each group ascending. The **`idx:0xLO-0xHI` range form** (e.g.
   `idx:0x06-0x09`, `idx:0x0e-0x7f`) is the documented CMOS range token: it **expands to one
   record per index** in [LO,HI] inclusive (exactly like an MSR `index-lo`/`index-hi` range,
   item 3), all sharing the row's disposition; the expanded indices must stay pairwise
   disjoint. `<param>` is the 16-hex `allow-fixed` constant or a closed §6 formula id
   (`cmos.index-latch` for port 0x70, `cmos.data-window` for port 0x71, `cmos.tod` for the
   time registers). Every token is closed — no bare `emulate`, `route-by-index`, or
   `see-cmos-subtable`.
8. Host-baseline assertion records (§1.1/§1.2): `host-assert <key> <value>` — the homogeneity
   requirements vmm-core checks at VM start and refuses to run on mismatch, hashed so the
   asserted domain is part of contract identity. **Fixed key order, with the concrete pinned
   values (no placeholders):**
   1. `family-model-stepping 06_9e_0c` — Coffee Lake-S (i9-9900K); box `cpuid-raw.txt` leaf 1 EAX.
   2. `host-microcode-rev 0x00000000000000f8` — the *physical* host's microcode revision,
      fleet-pinned (the det-cfl-v1 box's kernel-recorded `0xf8`; the deployment's actual fleet
      revision replaces this in a version bump). **Distinct from** `guest-ucode-rev` below — the
      guest-visible fake must never be reused as the host assertion.
   3. `guest-ucode-rev 0x0000000100000000` — the guest-visible BIOS_SIGN_ID (MSR 0x8b, §3.10),
      recorded here so the split is explicit and hashed.
   4. `mxcsr-mask 0x0000ffff`
   5. `maxphyaddr-min 39`
   6. `rtm-disabled true` — RTM must be non-usable by the guest. On the det-cfl-v1 baseline this
      holds by **physical absence**: RTM/HLE are not implemented (box CPUID.7.0:EBX[4,11]=0;
      `IA32_TSX_CTRL` 0x122 `#GP`s), so XBEGIN/XEND/XTEST/XABORT `#UD` natively — no host pin is
      installed or needed. The probe satisfies this assertion by reading CPUID.7.0:EBX[11] and
      passing when RTM is absent. (The key name and value are unchanged from the SKX baseline,
      where the same `rtm-disabled true` was satisfied instead by pinning
      `IA32_TSX_CTRL = RTM_DISABLE | TSX_CPUID_CLEAR` on the TSX-present part — the assertion is
      satisfiable two ways, §1.1/§4; only the mechanism changed. Renamed long ago from
      `rtm-absent`, which had wrongly implied a *specific* mechanism.) §4 TSX row.
   7. `cr4-force-reserved [PKE, PKS]` — vmm-core keeps these CR4 bits reserved in the guest
      CR4 mask so the features stay unreachable: CR4.PKE=0 ⇒ RDPKRU/WRPKRU `#UD` (§4 row) and
      PKRU is never in the XSAVE image; CR4.PKS reserved ⇒ IA32_PKRS unreachable (§3.13 note).
      (With PKU/PKS hidden in the frozen CPUID, KVM already reserves these; this records the
      invariant in the hash.)
   8. `host-absent <MNEMONIC>` — one per instruction the contract relies on faulting by
      absence: RDPID, SERIALIZE, SHA, HRESET, PCONFIG, UMWAIT, TPAUSE, UMONITOR (sorted).

   A host failing any assertion is refused. **There is no `image-scan-forbid` / opcode-scan
   record: it was unsound (XGETBV1 shares an opcode with XGETBV0; Linux carries the XSAVE
   encodings as CPUID-gated alternatives) and is removed** — the class-(e) ops are governed by
   the §1.2 cooperative-guest threat model (documented residual risk), not a host/image
   assertion. This canonical order and these values match `[host-assert]` in the TOML exactly.

The serializer is deterministic by construction (sorted, fixed-width, no maps with
iteration-order dependence — conventions rule 4) and lives with vmm-core's contract
parser: the same code that loads the tables emits the canonical form, so what is hashed
is what is enforced, with no second hand-maintained copy.

**What is hashed (normative).** `contract_hash` = SHA-256 over the full canonical form,
including the header records — 32 bytes, rendered as lowercase hex. SHA-256 keeps the
32-byte width of `unison::Machine::state_hash` and stays inside the dependency
whitelist (`sha2`). The hash binds in three places:

- **Startup**: vmm-core computes `contract_hash` from the artifact it actually parsed
  and refuses to run if it does not equal the expected hash recorded for the pinned
  contract-version. A VMM can therefore never silently run a drifted contract.
- **The determinism gate**: `contract_hash` is part of run identity. The gate's claim is
  "same seed ⇒ bit-identical execution" *under the same contract*: the harness records
  `(seed, contract_hash, kernel pin from versions.lock, vmm-core build)` in every run
  header, and `state_hash` comparisons between runs whose `contract_hash` differs are
  **invalid comparisons** (rejected by the harness), not divergences. The per-exit
  `state_hash` itself remains a pure function of guest state (INTEGRATION.md §5);
  the contract hash gates which state hashes may be compared rather than being mixed
  into each one.
- **Snapshots**: `contract_hash` is stored in the `vm_state` blob (INTEGRATION.md §4 —
  it influences future guest-visible behavior, so it is captured). Restore refuses a
  snapshot taken under a different `contract_hash`: a restored guest that suddenly
  observes different CPUID/MSR behavior is undefined, and refusing loudly is the only
  deterministic answer.

**Version-bump rule (normative).** `contract-version` is a strictly increasing integer.
Let the **body** be the canonical form minus the `contract-version=` line. The rule:
**any change to the body — any disposition token, any frozen value, any parameter, any
reference-set membership change, a kernel-tag change, a baseline change — requires a new
version.** Equivalently: version bumps if and only if the body bytes change. Prose,
rationale, and citation edits leave the body unchanged and require no bump (and provably
cannot change the hash, since they are excluded from serialization). There are no
in-place value edits under an existing version, ever — a wrong value is fixed by a new
version whose changelog says so.

**Registry status (live as of v3).** The enforcement is now mechanical and committed: the §6
canonical serializer exists in vmm-core (`consonance/vmm-core/src/contract/{canonical,parse}.rs`),
emits the byte string specified above from the parsed `cpu-msr-contract.toml`, and
`contract_hash` = SHA-256 of those bytes. The hash of the **v3 (det-cfl-v1)** contract is

> **`contract_hash` (v3, det-cfl-v1) = `e01f0835576444c269c6603fc4984d0b425785373f4c49613d75ce896565c832`**

committed in `cpu-msr-contract.toml` `[contract] contract_hash` and pinned by the live gate
`vmm_core::contract::tests::contract_hash_matches_committed_registry` (computed-from-parsed ==
committed) plus the byte-exact golden `src/contract/testdata/canonical-v3.txt`. vmm-core startup
re-serializes, re-hashes, and refuses a mismatch. Off-contract MSR accesses observed at runtime
(§1) feed back into the version rule: the triaged new row changes the body, so it arrives as a
new version, and every run header names the version + hash it executed under. *(v1 and v2 were
never frozen/registered — the serializer did not exist while they were drafted; v3 is the first
contract whose body-hash is computed and committed.)*

**Version history.**
- **v1** — initial frozen contract.
- **v2** (round-7) — **body changes** (hash changes): CPUID leaf-1 ECX `0x77DA3203 → 0x76DA3203`
  (clear bit 24, **TSC-Deadline hidden**); MSR `IA32_TSC_DEADLINE` (0x6e0) `emulate-timerqueue
  → deny-gp/deny-gp`; the `timerqueue.tsc_deadline[.write]` formula ids are removed. Rationale:
  the stock-KVM in-kernel WRMSR fastpath swallows 0x6e0 before the MSR filter under
  `KVM_IRQCHIP_NONE`, so deadline-mode emulation is unbacked; the LAPIC timer runs as xAPIC LVT
  one-shot/periodic (§5). Aligns with Ruling R1 (PR #21). (Round-6 DOITM-clear / 0x10a / 0x1a0 /
  0x140 / SynIC / round-7 RDPKRU + cr4-force-reserved + host-assert changes are also part of the
  v2 body relative to the original v1 draft.)
  - **v2 finalization (round-8, still pre-freeze — v2 was never frozen/registered):** the `insn`
    records for the TSX opcodes were corrected to the **always-abort** result on the TSX-capable
    SKX baseline (`XBEGIN`/`XABORT` `deterministic-abort`, `XEND` `gp`, `XTEST` `zero`) — not
    `#UD`; and 0x6e0's `deny-gp` is documented as backend-dependent (enforced under
    patched-KVM/direct-VMX; silently swallowed for an out-of-scope adversarial guest under stock
    KVM). These are part of the final v2 body. Because v2 has **no** registered body-hash yet
    (the serializer does not exist — see Registry status above), these pre-freeze corrections
    finalize v2 rather than constituting in-place edits to a released version; v2's body-hash is
    computed once, from this finalized contract, when the serializer lands.
- **v3** (task 11) — **re-baseline `det-skx-v1` → `det-cfl-v1`**: the determinism box is an Intel
  Core i9-9900K (Coffee Lake-S, `06_9e_0c`, microcode `0xf8`), not the synthetic Skylake-SP the
  frozen contract modeled, so the host-non-trapping surface (identity, XSAVE layout, MAXPHYADDR,
  the microcode-fingerprint MSR) must reflect the real silicon or the §1.1 host-assert correctly
  refuses to run. **Body changes** (hash changes; every value derived from and cited to the box
  dump under `docs/fragments/cfl-baseline/`): `cpuid-baseline det-skx-v1 → det-cfl-v1`; CPUID
  leaf-1 EAX `0x00050654 → 0x000906ec`; leaf-4 L2 EBX `0x03c0003f → 0x00c0003f` (1 MiB/16-way →
  256 KiB/4-way) and L3 EBX/ECX `0x0280003f/0x000007ff → 0x03c0003f/0x00003fff` (1.375 MiB/11-way
  → 16 MiB/16-way); leaf-7.0 EBX `0x019c27eb → 0x009c27ab` (drop FDP_EXCPTN_ONLY[6] + CLWB[24],
  both absent on CFL client); brand string `(SKX-class) → (CFL-class)`; 0x80000006 ECX
  `0x04008040 → 0x01004040` (mirrors leaf-4 L2); 0x80000008 EAX `0x0000302e → 0x00003027`
  (MAXPHYADDR 46 → 39); MSR `IA32_ARCH_CAPABILITIES` (0x10a) `0x400000000d10e171 → 0x000000000a000c09`
  (the 9900K/µcode-0xf8 fingerprint, read from the box). **TSX reclassification:** XBEGIN/XEND/
  XTEST/XABORT move from class (c) `host-pin(tsx-ctrl-rtm-disable)` → class (b) `fault-absent`/`#UD`
  (RTM/HLE physically absent on CFL; `IA32_TSX_CTRL` `#GP`s — no host pin needed). host-assert
  `family-model-stepping 06_55_04 → 06_9e_0c`, `host-microcode-rev → 0x...f8`, `maxphyaddr-min
  46 → 39`. **First version with a committed `contract_hash`** (= `e01f0835…565c832`, above) and
  a live registry-match gate. Carried over unchanged (synthetic/SDM-architectural, not host-forced):
  the single-thread topology, XCR0={x87,SSE,AVX} / leaf-0xD layout, the 2.0 GHz/25 MHz/100 MHz
  frequency scalars, MXCSR_MASK `0x0000ffff` (box-confirmed), `guest-ucode-rev`, the MSR
  partition (1043 indices), and the §1/§3/§5 dispositions.

## 7. Citations

Citations are carried per row, in each table's `Citation` column — there is no separate
bibliography. Conventions, normative for future edits: every non-obvious disposition
cites a primary source — Intel SDM volume/chapter, Linux `Documentation/virt/kvm/`
(api.rst, x86/msr.rst, x86/cpuid.rst, x86/errata.html), kernel source as `file:line` at
the pinned tag v6.18.35 (`arch/x86/kvm/x86.c`, `arch/x86/include/asm/msr-index.h`, and
friends; tarball sha256 in the header table), the rr paper/source, the Antithesis
deterministic-hypervisor write-up, or a RESEARCH.md entry — matching RESEARCH.md's
citation discipline. `Rationale` and `Citation` columns explain but do not bind: they
are excluded from the canonical serialized form (§6), so citation edits never change
`contract_hash` and never bump the version.

The machine-readable mirror of the §2 and §3 tables is `docs/cpu-msr-contract.toml`,
generated at assembly time from this document's tables (normative columns only). If the
two ever disagree, this document wins and the TOML is regenerated.
