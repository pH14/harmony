<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# AE-3 — svm.c force-exit at PMI: disposition (ESCALATED)

`docs/AMD-EPYC.md` AE-3. Build a patched `kvm_amd` (`svm.c`) that converts a work-counter
overflow into a deterministic in-kernel vCPU exit, then drive the full landing contract.

## What was verified on-box

- **The `svm.c` hunk is correct against the real pinned kernel.** `linux-source-6.8.0`
  was fetched on the box; its `nmi_interception()` is exactly `{ return 1; }`
  (`arch/x86/kvm/svm/svm.c:2311`), matching the draft's context. `git apply --check` of
  `host/patches/0004-KVM-SVM-KVM_EXIT_PREEMPT-force-exit-analogue.patch` **applies clean**
  to that tree. The mechanism-half — the analogue of VMX 0004's `handle_exception_nmi()`
  arm, in `svm.c`'s `SVM_EXIT_NMI` handler — is sound and content-verified.
- **The favorable AVIC posture holds:** the box baseline already runs `kvm_amd avic=0`,
  and AVIC does not accelerate NMIs, so the perf-overflow NMI reaches this exit path.

## Why the FULL build is blocked (evidence: `build-environment.json`)

The `svm.c` hunk compiles against the **shared vendor-neutral determinism plumbing**
(`KVM_EXIT_PREEMPT`, `kvm->arch.deterministic_intercepts`, `vcpu->arch.preempt_armed`) that
patches **0001/0002/0004** add — and **those patches target kernel ~6.18, not the box's
stock 6.8.0-88**:

| fact (6.8.0-88) | value |
|---|---|
| `uapi/linux/kvm.h` max `KVM_EXIT_*` | `KVM_EXIT_MEMORY_FAULT` = **39** (no TDX/DETERMINISM/PREEMPT) |
| `KVM_EXIT_PREEMPT` present | **no** |
| `deterministic_intercepts` / `preempt_armed` fields | **absent** |
| shared patches 0001/0002/0004 apply to 6.8 | **no** (context drift: `kvm.h:184`, `x86.c:4894`, `kvm_host.h:814`) |
| **`svm.c` hunk applies to 6.8** | **yes** (context matches `nmi_interception`) |

So the mechanism is right, but the tree it must build in does not exist on this box.

## Disposition: ESCALATED (not NO-GO)

Per tasks/123's **escalate-don't-improvise** rule for a *doc-vs-hardware contradiction* and a
*module-build failure against the pinned kernel*, this is escalated rather than worked around.
It is **not a NO-GO** for the Zen work-clock thesis — the mechanism (svm.c `SVM_EXIT_NMI` →
`KVM_EXIT_PREEMPT`) is content-verified and AE-1 already showed the underlying HW behavior it
needs (late-only overflow, exactly-once, bounded skid 5043). The blocker is purely the build
environment version skew.

**The two clean resolutions (a decision above the spike):**
1. Provision the box with a **~6.18-class kernel** matching the determinism patch series
   (`build-kvm-amd.sh` then applies 0001/0002/0004 + the svm.c hunk and builds), or
2. produce an **official 6.8 backport** of patches 0001/0002/0004 (rebased UAPI exit-reason
   numbers + context), after which the svm.c hunk (already 6.8-context-correct) builds as-is.

Improvising a one-off hand-port of the whole determinism series onto 6.8 — or hot-swapping
the box kernel — was deliberately **not** done: it would substitute a different kernel/patch
set than the ruling names (a docs/AMD-EPYC.md §Execution-constraints prohibition) and is the
call to escalate. The apparatus is staged so that either resolution is a one-command build:
`host/build-kvm-amd.sh all` (patches staged under `host/patches/`, recipe validated up to the
version-skew wall).

## Trait-freeze memo (the part AE-3 owes `docs/ARCH-BOUNDARY.md`, answerable now)

Even without the in-kernel exit, AE-1(d) answers the deferred question: the SVM overflow is
**late-only** (skid ∈ [0, 5043], `skid_min = 0`, 0 early over 10⁶ arms), so
`run_until_overflow`'s late-only-stop contract holds on SVM PMI delivery — the `Arch`/
`CpuBackend` trait needs **no structural change**, only the re-parameterized Zen `skid_margin`.
The in-kernel path (once buildable) should only *tighten* the skid, preserving late-only.
