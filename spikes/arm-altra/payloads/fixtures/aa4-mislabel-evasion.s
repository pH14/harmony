// SPDX-License-Identifier: AGPL-3.0-or-later
//
// aa4-mislabel-evasion — the anti-weakening negative control for the section-aware
// static LL/SC scanner (hm-jth / hm-7o68-F3; foreman ruling on Option A).
//
// A forged bare ELF that MISLABELS executable hazard-bearing code as living in a
// non-executable DATA section (.rodata: SHF_ALLOC, no SHF_EXECINSTR). The ENTRY
// (_start) is placed IN that data-flagged page, and it is a real LDXR/STXR pair.
//
//   * The section-aware STATIC scanner (aa4-exclusive-scan.py, Option A) excludes
//     defined non-exec data sections, so it does NOT see the planted exclusives and
//     PASSES this ELF — the evasion the ruling requires us to demonstrate. (The clean
//     .text section below exists only so the static scan is non-empty and passes on
//     genuine executable words, proving the pass is due to the mislabel.)
//   * The runtime page-granular execute-guard (hm-rfz, aa4guard host) is default-XN
//     and rescans the ACTUAL bytes of the entry page BEFORE the guest executes a
//     single instruction. It decodes the LDXR/STXR and REJECTS at entry (PC never
//     advances) — the load-bearing enforcement.
//
// Net: the static gate is a section-aware PRE-FLIGHT; the runtime guard OWNS W^X.

	// Clean executable section — present ONLY so the section-aware static scanner has
	// executable words to scan and passes on their cleanliness (not on emptiness).
	// Never executed: the entry is in the mislabeled data page below.
	.section .text, "ax"
	.global clean_body
clean_body:
	nop
	ret

	// The ENTRY lives here, in a DATA-flagged (.rodata), page-aligned page carrying the
	// planted exclusives. On first execute the default-XN guard scans THIS page and
	// rejects before the guest advances — PC stays exactly at _start.
	.section .rodata, "a"
	.balign	4096
	.global _start
_start:
	.word	0x885f7c20	// ldxr  w0, [x1]   — planted exclusive #1 (as data)
	.word	0x88027c20	// stxr  w2, w0, [x1] — planted exclusive #2 (as data)
0:
	b	0b		// spin — unreached: the guard rejects this page at entry
