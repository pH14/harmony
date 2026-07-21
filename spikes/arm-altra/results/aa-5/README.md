<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# AA-5 — paravirt work-derived clock: (a)+(b) evidence; (c) is the remaining major build

Full disposition in `docs/ARM-ALTRA.md` §AA-5. This directory holds the AA-5(b) closure-scan
evidence; AA-5(a)'s determinism rides the AA-3 records.

- **`counter-scan.txt`** — `host/aa5-counter-scan.py`, the counter-read closure scan (mirrors
  the harness primitive `scan::decode_counter_read`, unit-tested). Premise from AA-0:
  `ID_AA64MMFR0_EL1.ECV = 0x0` on N1 — FEAT_ECV absent, so the guest's `CNTVCT_EL0` cannot be
  trapped in hardware and raw-counter closure must be contract-level. The scan (raw-opcode
  decode SELF-VALIDATED against `objdump`, with a positive control proving it rejects a
  `CNTVCT_EL0`/`CNTPCT_EL0` read while allowing the constant `CNTFRQ_EL0`) finds **every payload
  counter-clean** — `clock-page` reads time via the materialized page, never the live counter.

**(a) payload determinism — demonstrated (AA-3).** `clock-page` reads a materialized
work-derived clock page (seqlock, no `CNTVCT`); it landed **bit-identical across same-seed
reps** in the ≥10⁶ AA-3 run (replay-identity PASS, and solo-vs-co-tenant MATCH) once the
canonical-landing fix (AA3-F1) was in. The page is presently a *static* placeholder
(`FLAG_WORK_DERIVED` clear — the plumbing, not a live work-derived refresh), so it is trivially
wall-clock-invariant; the value-advances-with-work refresh is `hm-8h8`'s design, which AA-5
validates once landed. The digest also **excludes** the live host-time counters
(`is_host_time_register`), so wall-clock never reaches a compared digest — verified indirectly
in AA-3 (CNTPCT varied 240/240 on a passing payload while replay held).

**(b) closure — premise + scanner demonstrated.** ECV-absent (AA-0) establishes the contract
must close the counter; the counter-read scan (above) is the build/rescan layer and is
validated. Remaining, kernel-dependent: the EL0 `CNTVCT_EL0`-read-undefs-under-`CNTKCTL_EL1`
test and the scan run against the *shipped guest kernel image*.

**(c) the Linux smoke — the remaining major build (blocked on assets).** No arm64 guest kernel
Image / initramfs / DTB is present on the box, and the harness boots tiny bare-metal payloads,
not a full Linux guest (`Payload::LinuxGuest` is a placeholder). Booting our arm64 guest —
paravirt clocksource + `sched_clock` + delay paths on the page, `CNTKCTL_EL1` closure — to
userspace under the spike harness, to steady state, holding a same-seed digest, is a
multi-session build: acquire/build the guest kernel + rootfs, then build the harness Linux-boot
path (Image+DTB+initramfs load, PSCI, GIC, console, entry). This stage also hosts AA-4 level-3's
live planted-exclusive proof. Recommended as a dedicated follow-on.
