<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Reviewed instruction sites

Admission rejects any hardware entropy instruction (x86 RDRAND and RDSEED,
arm64 RNDR and RNDRRS) unless the image lists it in
`/etc/harmony/instruction-allowlist`. Each line names the executable's SHA-256,
the instruction address, the instruction, and the reason it is safe. A changed
executable has a new digest, so its old review no longer applies and admission
fails until someone reviews it again. A review entry that matches nothing in the
image also fails admission.

Counter reads (RDTSC, RDTSCP, CNTVCT_EL0 and the like) need no review. The guest
kernel traps them for every user process: x86 emulates them from the
paravirtual clock, and arm64 denies them.

`bookworm-x86_64.txt` reviews the entropy sites in the pinned Debian Bookworm
base. The language images install it through `compose.Dockerfile`; the etcd and
SQLite historical images install it directly. The PostgreSQL CIC image builds
from its own directory, so it keeps an identical copy in
`workloads/bugs/historical/postgres-cic-corruption/image/reviewed-x86_64.txt`.

| Library | Sites | Why they never run |
| --- | --- | --- |
| libgcrypt | 2 RDRAND | Reached only after `_gcry_get_hw_features` reports `HWF_INTEL_RDRAND`, which comes from CPUID leaf 1 ECX bit 30 ([rndhw.c](https://github.com/gpg/libgcrypt/blob/libgcrypt-1.10.1/random/rndhw.c), [hwf-x86.c](https://github.com/gpg/libgcrypt/blob/libgcrypt-1.10.1/src/hwf-x86.c)) |
| libstdc++ | 1 RDRAND, 4 RDSEED | `random_device` installs these helpers only after checking CPUID leaf 1 ECX bit 30 and leaf 7 EBX bit 18 ([random.cc](https://github.com/gcc-mirror/gcc/blob/releases/gcc-12.2.0/libstdc%2B%2B-v3/src/c%2B%2B11/random.cc)) |

The guest's fixed CPUID clears both bits.
