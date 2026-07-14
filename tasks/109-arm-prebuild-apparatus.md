# Task 109 — ARM pre-build apparatus: arm64 oracle payloads, minimal KVM harness, floor-checker schemas, kvm/arm64 patch draft

**Bead:** `hm-2kj` (P2). **Dispatch authority:** the pre-build ruling (Paul, 2026-07-13;
`docs/ARCH-BOUNDARY.md` §Pre-build ruling), queue lane 4-ARM. **Binding context:**
`docs/ARM-ALTRA.md` — you are building the offline apparatus its §Immediate focus and
§Execution constraints already sanction ("payload/oracle construction, harness and schema
scaffolding, kernel-config and patch drafting — all under `spikes/arm-altra/`, all clearly
untested-on-silicon"), so that Altra arrival day is `scp + run`, not scaffolding.

Read first, in full: `docs/ARM-ALTRA.md` (the whole program — especially §Spike architecture,
§Evidence integrity, stages AA-0..AA-3, §Box discipline for what the apparatus must make
possible), `docs/ARM-PORT.md` (hardware facts: BR_RETIRED 0x21 = retired *taken* branches;
the LL/SC hazard; rr evidence), and `tasks/00-CONVENTIONS.md`.

**This is apparatus, not the spike.** You produce no measurements, no dispositions, no
evidence manifests, and you never touch any box. Every artifact is marked
**untested-on-silicon** (a README banner plus per-directory notes). You also write zero
production-crate code — the seam restructure (`hm-b5n`) runs in parallel with no file
overlap, and the ARM backend proper is a separate bead (`hm-cbt`).

## Deliverables (all under `spikes/arm-altra/`, standalone — not a workspace member)

1. **`payloads/` — minimal arm64 bare-metal runtime + analytical oracle payloads.**
   Runtime: boot shim, exception vectors, PL011 console, GIC init — spike-grade, reusing the
   host-derived-golden harness *pattern* from the x86 payloads (pattern, not code; these
   payloads test a different contract). Oracle payloads per class from AA-1/AA-2: straight-line
   loops, branch-dense, syscall (SVC), exception entry/return, WFI/idle, LL/SC and LSE atomic
   pairs (AA-4's a/b payloads), clock-page reads. Each payload's **taken-branch count is known
   by construction**: emit a machine-readable expected-count manifest per payload, with the
   derivation documented and generator-tested. (No PMU access in the payloads themselves —
   the harness owns counting on real silicon.)
2. **`harness/` — the minimal ioctl-level KVM harness** (single vCPU; core-pinning and
   perf-event arming of raw 0x21 as guest-only, structured per AA-1(b/c)) plus run
   orchestration. It must **cross-build for aarch64-linux**; its pure-logic pieces
   (orchestration, schema emission, count bookkeeping) get native tests (this Mac is
   aarch64 — they run natively). The perf/KVM syscalls obviously cannot run on macOS: seam
   them so logic is testable and the syscall layer is thin.
3. **`schemas/` — canonical evidence formats + floor-checker scripts** per §Evidence
   integrity: stable-JSON run-record schemas (the §Spike architecture field list) and the
   checkers that recompute every acceptance floor **from retained per-sample records** (≥10⁶
   armed overflows, per-record exactly-once multiplicity, totality accounting). The checkers
   are arrival-day load-bearing: give them their own test fixtures — synthetic runsets they
   must accept and (each failure mode) reject.
4. **`host/` — the kvm/arm64 0004-analogue patch DRAFT**: guest-mode work-counter overflow →
   in-kernel vCPU kick with a dedicated deterministic exit reason (the
   `KVM_ARM_PREEMPT_EXIT` → `KVM_EXIT_PREEMPT` mirror, per AA-3). Gate = the patch **applies
   and cross-compiles** against a **pinned** arm64 kernel tree — pin the same canonical
   release line the x86 determinism patches target (the 6.18.x canonical port), record
   tree/tag/config and the exact build commands. Plumbing smoke under emulation is
   best-effort; correctness claims are AA-3's, not yours.
5. **`README.md`** — toolchain setup (targets, qemu install), every build/smoke command,
   the untested-on-silicon banner, and a "what is validated here vs what only silicon can
   say" table.

## The TCG smoke — what it may and may not claim

`qemu-system-aarch64` (TCG) is the slow oracle for **liveness and protocol only**: each
oracle payload boots under TCG, runs to completion, and its console/exit protocol round-trips;
the smoke script diffs structure (not counts) and **propagates every constituent gate's RC**
(§Evidence integrity #1 applies to your own scripts: a done-marker is never success). TCG
proves nothing about counts, PMIs, or skid — never label it otherwise. If the Mac
nested-KVM probe (`hm-8l3`) lands GO, a real-KVM harness smoke inside an aarch64 Linux VM is
a welcome bonus gate; do not block on it.

## Constraints

- No invented constants: `0x21`/`BR_RETIRED` and the event semantics come from
  `docs/ARM-PORT.md`/`docs/ARM-ALTRA.md`; skid margins, densities, and count offsets are
  spike deliverables — the apparatus must treat them as unknowns (parameters), never
  defaults.
- GLOSSARY vocabulary in all prose and identifiers (Subject, Moment/Span, V-time); "vendor"
  never "personality"; no "(formerly X)" comment residue.
- Dependency whitelist per conventions for anything cargo-shaped; the payload runtime may
  be `#![no_std]` + raw asm as needed (it is spike apparatus, `unsafe` is expected there —
  keep it out of the checker/orchestration logic).

## Environment

Fully Mac-local: rustup targets (`aarch64-unknown-none`/`aarch64-unknown-linux-gnu` or a
documented equivalent toolchain), `qemu` via Homebrew. **No box, no SSH, no beads-DB
requirements beyond the normal worker flow.** Lands via a normal task PR; the
spike-*execution* branch discipline in `docs/ARM-ALTRA.md` governs the future hardware run,
not this task.

Done = the four directories + README on `task/arm-prebuild-apparatus`, aarch64 builds green,
TCG payload smoke green with RC propagation, floor-checkers fixture-tested, patch draft
applying + compiling against the pinned tree, IMPLEMENTATION.md noting every known
sim-vs-silicon gap.
