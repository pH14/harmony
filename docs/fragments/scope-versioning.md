> **NON-NORMATIVE — construction artifact.** This per-class fragment was used to
> assemble the contract and is **not** parsed or hashed. The authoritative, hashed
> surface is the spine `docs/CPU-MSR-CONTRACT.md` (§1–§6) plus `docs/cpu-msr-contract.toml`.
> Where this fragment and the spine/TOML disagree, **the spine and TOML win.**

# Guest-visible CPU/MSR determinism contract

| Field | Value |
|---|---|
| contract-version | 1 |
| reference kernel | Linux **v6.18.35** — equals `guest/linux/versions.lock` `KERNEL_VERSION=6.18.35` (tarball sha256 `f78602932219125e211c5f5bfd84edcfd4ec5ce88fc944f8248413f665bef236`); all `arch/x86/kvm/x86.c` and `arch/x86/include/asm/msr-index.h` citations are to that tag |
| baseline microarchitecture | the named baseline of the frozen CPUID model (§2) |
| contract hash | `contract_hash` = SHA-256 of the canonical serialized form, computed per §4 from the assembled tables — never hand-written into this document |

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
wrong. The contract changes only by editing this document and bumping the version per §4
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
instruction set (§ instruction table), the x2APIC MSR range, and the timer devices (PIT,
HPET, LAPIC timer). Out of scope: AMD, multi-vCPU, and anything host-side that the guest
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
`deny-gp` + rationale) with a version bump per §4, because the reference set was supposed
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
  covering them. The `emulate-apic` dispositions in §3's x2APIC sub-table are therefore
  enforced by the APIC virtualization configuration itself (split irqchip / userspace
  timer emulation per INTEGRATION.md §7), not by `KVM_X86_SET_MSR_FILTER`. No row in the
  x2APIC range may claim the filter as its mechanism.
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

## 4. Versioning & hashing

The contract is a **config artifact** (INTEGRATION.md §7: "one frozen, versioned CPUID
model (a config artifact, hashed into the determinism gate)"). Two runs are only
meaningfully comparable if they ran under bit-identical contracts, so the contract has a
canonical byte representation, a hash, and a version discipline.

**Canonical serialized form (normative).** The canonical form is a UTF-8 byte string,
LF line endings, no comments, no trailing whitespace, derived from the normative tables
only — `Rationale` and `Citation` columns and all prose are excluded (they explain, they
do not bind). It consists of, in order:

1. Header records: `contract-version=<N>`, `kernel-tag=v6.18.35`,
   `cpuid-baseline=<name from §2>` — one per line, in exactly that order.
2. CPUID records: one line per listed (leaf, subleaf):
   `cpuid <leaf>.<subleaf> <eax> <ebx> <ecx> <edx>`, every field 8 lowercase hex
   digits, masking rules resolved to the concrete frozen values at serialization time
   (the hash covers what the guest can actually observe), sorted ascending by
   (leaf, subleaf); followed by one `cpuid-default <zeroed|fixed:...>` record for the
   unlisted-leaf rule.
3. MSR records: one line per MSR index:
   `msr <index> <read-token> <write-token>[ <param>]` — index 8 lowercase hex digits;
   tokens verbatim from the §3 vocabulary; `<param>` carries the `allow-fixed` constant
   (16 lowercase hex digits) or the named `emulate-*` formula id (e.g. `vclock.tsc`,
   `timerqueue.oneshot`). Range rows are **expanded to one record per index** before
   serialization, so a membership change always changes the bytes. Sorted ascending by
   index; exactly one record per index — a duplicate or conflicting row is a
   serialization **error**, never last-wins.
4. x2APIC sub-table records: `apic <offset> <read-token> <write-token>`, offset in
   lowercase hex, sorted ascending.
5. Instruction records: `insn <MNEMONIC> <mechanism> <result>`, uppercase mnemonics,
   sorted lexicographically.
6. Timer-device records: `timer <pit|hpet|lapic-timer> <disposition>`, in that fixed
   order.

The serializer is deterministic by construction (sorted, fixed-width, no maps with
iteration-order dependence — conventions rule 4) and lives with vmm-core's contract
parser: the same code that loads the tables emits the canonical form, so what is hashed
is what is enforced, with no second hand-maintained copy.

**What is hashed (normative).** `contract_hash` = SHA-256 over the full canonical form,
including the header records — 32 bytes, rendered as lowercase hex. SHA-256 keeps the
32-byte width of `unison::Subject::state_hash` and stays inside the dependency
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
version whose changelog says so. Enforcement is mechanical: the repository carries an
append-only registry of `(contract-version, body-hash)` pairs next to the contract
artifact; CI re-serializes, re-hashes, and fails if the current body hash is not
registered, if it is registered under a different version, or if a version number is
ever reused with a different hash. Off-contract MSR accesses observed at runtime (§1)
feed back into this rule: the triaged new row changes the body, so it arrives as a new
version, and every run header names the version + hash it executed under.
