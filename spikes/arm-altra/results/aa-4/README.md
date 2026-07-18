<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# AA-4 — LL/SC vs LSE ruling: evidence

The characterization and recommended ruling live in `docs/ARM-ALTRA.md` §AA-4. This directory
holds the machine-generated evidence behind (a) the hazard and (b) the LSE invariance, plus (c)
level-2 of the enforcement ladder. All of it rides the AA-3 exact-landing apparatus: the
`llsc-atomics` (LL/SC) and `lse-atomics` (LSE-only, identical algorithm) payloads run under the
identical arm-early `Preempt` + single-step injection schedule.

- **`llsc-characterization.txt`** — `host/aa4-llsc-characterize.py` over the ≥10⁶ sharded run
  and the solo reference. Headline: `llsc-atomics` diverges run-to-run **in the work clock
  itself** (`measured_taken`/`work_end` ±2 retired branches — one spurious-`STXR` monitor-clear
  retry) in **26.3 %** of solo tuples (intrinsic) and **31.6 %** under co-tenant load;
  `lse-atomics` diverges in **0 of 73,150** tuples (count and state). `payload_status = 0`
  throughout — correct computation, non-deterministic branch count.
- **`exclusive-scan.txt`** — `host/aa4-exclusive-scan.py`, the level-2 executable-page
  opcode scan. It walks raw ELF executable-section words and decodes the broad class
  `(insn & 0x3f800000) == 0x08000000` with the o1/size discriminator (monitor exclusives;
  excludes LDAR/STLR and LSE `CAS`/`CASP`), **self-validated against `objdump`** wherever the
  disassembler renders an instruction. The independent byte walk also catches mapping-symbol
  data that `objdump -d` deliberately does not decode.
  Flags the two exclusives in `llsc-atomics` (`0x40080880 ldxr` / `0x40080888 stxr` — the same
  PCs as the AA-2 single-step livelock) and passes every other payload, `lse-atomics` included,
  CLEAN; exits non-zero to reject.

Level 1 (LSE-only build) is demonstrated by `lse-atomics` itself and is now applied to the
AA-5 owned kernel/init image, whose static artifact gate also uses this scanner. The draft
Harmony arm64 KVM execute-guard extension now applies and compiles against pinned 6.18.35;
stock KVM still resolves execute faults internally and exposes no per-GFN XN UAPI. Its userspace
half is wired but unrun: `linux-boot --stage2-exec-guard` mediates clean pages and refuses vacuous
statistics, while `aa4-guard-reject` requires a hash-pinned planted exclusive to be rejected
before its PC advances. Level 2's live W^X rescan-on-exec proof and level 3 therefore remain
blocked on booting that patch and retaining the planted rejection/write/race/invalidation
evidence, not on merely compiling either half.

**Current ruling:** cooperative residual risk on stock KVM. The owned guest disables known
runtime code-generation surfaces and its static image is clean, but a JIT/self-modifying page is
not mechanically intercepted. The stronger mechanically-unreachable ruling is conditional on a
default-XN, pre-execute scan, write-revokes-execute KVM patch plus a planted-exclusive proof;
the kernel and VMM paths exist, but the live proof does not yet.
