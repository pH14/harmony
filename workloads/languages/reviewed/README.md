<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Reviewed base-library instructions

`bookworm-x86_64.txt` approves specific executable digests and instruction
addresses in the pinned Debian Bookworm base. C and Rust copy it only for x86_64.
Admission rejects stale entries and changed digests; rebuilding with a different
base requires a new review.

The loader, libc, ldconfig and libgcrypt counter reads are covered by the
default-on Harmony user-counter trap. Guest-kernel patches 0003 and 0007 set
CR4.TSD at exec and emulate ring-3 RDTSC/RDTSCP from the paravirtual clock.
The process cannot reenable native counters.

libgcrypt's two RDRAND loops are reached only after `_gcry_get_hw_features`
reports `HWF_INTEL_RDRAND`. The inspected disassembly has those tests before
each loop; [rndhw.c](https://github.com/gpg/libgcrypt/blob/libgcrypt-1.10.1/random/rndhw.c)
and [hwf-x86.c](https://github.com/gpg/libgcrypt/blob/libgcrypt-1.10.1/src/hwf-x86.c)
show the flag comes from CPUID leaf 1 ECX bit 30.

libstdc++'s local hardware-random helpers are installed into
`random_device::_M_func` only after CPUID checks. GCC 12's
[random.cc](https://github.com/gcc-mirror/gcc/blob/releases/gcc-12.2.0/libstdc%2B%2B-v3/src/c%2B%2B11/random.cc)
requires leaf 1 ECX bit 30 for RDRAND and leaf 7 EBX bit 18 for RDSEED, including
explicit hardware tokens. Harmony's frozen CPUID hides both bits. The reviewed
helper addresses match the packaged library's disassembly.
