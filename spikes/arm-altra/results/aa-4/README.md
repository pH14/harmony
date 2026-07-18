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
AA-5 owned kernel/init image, whose static artifact gate also uses this scanner. Level 2's live
W^X rescan-on-exec wiring and level 3 (stage-2 execute-deny + trap/emulate against a planted
exclusive) remain homed to the running AA-5 guest.

**Recommended final ruling (Paul ratifies at PR time):** LL/SC mechanically unreachable in the
shipped guest via level 1 (LSE-only) + complete level 2 (build scan and live rescan-on-exec),
level 3 the runtime backstop; the one cooperative residual — runtime-generated exclusives (JIT /
self-modifying code) — is bounded by W^X rescan-on-exec + the stage-2 backstop. The static half
is complete; the live halves remain open as stated above.
