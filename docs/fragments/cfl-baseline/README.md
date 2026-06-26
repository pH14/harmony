# `det-cfl-v1` baseline — raw box evidence (task 11)

Read-only capture of the determinism box — **Intel Core i9-9900K (Coffee Lake-S,
family 6 / model 0x9e / stepping 0xc, microcode `0xf8`)** — that the contract-v3
re-baseline (`det-skx-v1` → `det-cfl-v1`) derives every host-forced constant from.

**Provenance.** Every file here was captured **read-only by the foreman** (the
authoritative box operator) over `ssh <det-box>`, pinned per `docs/BOX-PINNING.md`. The
captured values are **independently validated on the box** by the foreman's live runs —
`host_assert_report` (§1.1, **13/13 PASS**) and the M1/M2 determinism gates (**3/3 PASS**)
— recorded in `consonance/vmm-core/IMPLEMENTATION.md`. That live-gate validation, not a second
capture, is the corroboration the contract relies on. (This supersedes an earlier draft of
this file that described two separate "capture sets"; there is one source — the foreman's
read-only capture — cross-checked by the live gates.)

| File | What | Capture |
|---|---|---|
| `cpuid-raw.txt` | `cpuid -1 -r` — all leaves, raw EAX/EBX/ECX/EDX (the normative §2 input) | foreman `ssh` |
| `cpuid-decoded.txt` | `cpuid -1` — decoded, for bit-name cross-checking | foreman `ssh` |
| `sysinfo.txt` | `uname -r` + microcode + `/proc/cpuinfo` model + `lscpu` | foreman `ssh` |
| `lscpu-microcode.txt` | `lscpu` + sysfs/cpuinfo microcode + `uname -r` (subset of `sysinfo.txt`) | foreman `ssh` |
| `msr-dump.txt` | `rdmsr 0x10a/0x8b/0x48/0x122/0x1b01/0xce/0x10b/0x6c0` (cpu0) | foreman `ssh` |
| `msrs.txt` | `rdmsr -a 0x10a` (all 16 CPUs, homogeneity) + `0x8b/0x48/0x3a/0xce/0xcf/0x122/0x1b01/0x10b/0x6c0` | foreman `ssh` |
| `mxcsr.txt` | `MXCSR_MASK` — **observed by the foreman's `host_assert_report` run** (vmm-core `fxsave_mxcsr_mask`), not a separate file capture | foreman host-assert |

Key cross-checks the contract relies on (each value cited to its file above):

- **Identity** `0x000906ec` → `06_9e_0c`; `Model: 158 (0x9e)`, `Stepping: 12 (0xc)`.
- **Microcode** `0xf8` (sysfs, `/proc/cpuinfo`, and `rdmsr 0x8b` = `0xf800000000` = `0xf8`<<32).
- **MAXPHYADDR** `0x80000008` EAX = `0x00003027` → **39** physical / 48 virtual (`lscpu`: "39 bits physical").
- **MXCSR_MASK** `0x0000ffff` (observed by `host_assert_report`).
- **IA32_ARCH_CAPABILITIES** `rdmsr -a 0x10a` = `0xa000c09`, **identical on all 16 CPUs** (homogeneous).
- **TSX absent**: `rdmsr 0x122` (`IA32_TSX_CTRL`) `#GP`s; CPUID.7.0:EBX[4,11]=0; `lscpu` flags have no `rtm`/`hle`.
- **DOITM absent**: `rdmsr 0x1b01` (`IA32_UARCH_MISC_CTL`) `#GP`s.
- **Caches** (leaf 4): L1 32 KiB/8-way, L2 256 KiB/4-way, L3 16 MiB/16-way.
- **Absent variance insns**: `lscpu` flags lack `sha_ni`, `clwb`, `rdpid`, `serialize`,
  `waitpkg`, `pconfig`.

See `consonance/vmm-core/IMPLEMENTATION.md` (Task 11 section) for the per-constant
change-set table mapping each value to its line of evidence here.
