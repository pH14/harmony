# Bounded native arithmetic observations

This corpus measures exact arithmetic observations on Linux x86_64 CPU pairs.
It supports the native-math portability work in
[#733](https://github.com/pH14/harmony/issues/733). Standard math libraries remain
supported and replay CPU warnings remain advisory. A matching CPU identity is
not numerical qualification. Differences are retained results, not reasons to
ban instructions or substitute an arithmetic implementation.

`corpus.c` embeds 10,944 cases: 94 binary64 inputs for each of eight scalar libm
calls (`sin`, `cos`, `tan`, `exp`, `log`, `sqrt`, `log1p`, `expm1`), selected x87
`FSIN`, `FCOS`, `F2XM1`, `FYL2XP1` observations, and 87 binary32 inputs for SSE
`RCPPS`, `RSQRTPS`, `SQRTPS`, and `DIVPS`. Each packed SSE lane receives the same
operand; all four result lanes are retained. x87 results retain the ten stored
binary80 bytes without C padding. `F2XM1` inputs are restricted to absolute value
at most one, and `FYL2XP1` uses a multiplier of one with inputs of absolute value
at most one quarter. Inputs include signed zero, subnormal/normal boundaries,
neighbors of one and pi, large finite numbers, infinities, quiet/signaling NaN
payloads, and a fixed integer-generated sample. Operand hex strings describe
IEEE bit patterns; result hex strings list exact little-endian memory bytes.

Every case establishes x87 extended precision, masks arithmetic exceptions,
clears status, and establishes MXCSR. All four rounding directions (nearest,
down, up, toward zero) run with FTZ/DAZ both off and both on. The output retains
x87 status/control, MXCSR including exception flags, and `errno`, in addition to
input/result bits. Controls must remain as established. No tolerance, NaN
canonicalization, status masking, reference compiler build, or decimal
conversion is used in comparisons.

Build once on Linux x86_64 with GCC and glibc development files:

```sh
python3 scripts/qualify-native-math.py bundle native-math-bundle --source-revision "$(git rev-parse HEAD)"
```

The fully static ELF is compiled for baseline x86-64, without fast-math,
contraction, or compiler builtin math substitution. The retained manifest binds
the exact executable, embedded source, qualifier, source revision, compiler
binary/version, assembler/linker/compiler-pass identities, command, complete
link-map inputs (including libm/libc archives and startup objects), and glibc
version. The link map, ELF inventory, and build observations are retained. A
library archive's presence is provenance, not evidence that every routine in
that archive executed; this corpus does not call libmvec entry points or AVX,
AVX-512, or FP16 estimate instructions.

Distribute that entire bundle without rebuilding. On each host:

```sh
sudo cat /sys/class/dmi/id/product_uuid > host-instance-id
python3 scripts/qualify-native-math.py run native-math-bundle native-math-report --host-id-file host-instance-id
```

The runner pins all three process repetitions to the lowest allowed logical CPU
and records its index, vendor/family/model/stepping/microcode/features, raw CPU
inventory, kernel version, and firmware instance UUID. The three complete outputs
must agree before a receipt is published. A failed process retains stdout and
stderr without a complete receipt. Existing bundle/report directories are
refused to preserve witnesses. Distinct UUIDs identify separate host instances,
not independent physical machines. CPU metadata describes the observed pair;
it does not establish equivalence with another implementation or microcode.

```sh
python3 scripts/qualify-native-math.py compare native-math-bundle qualification.json reports/*/receipt.json
```

The comparison requires a single unchanged retained artifact and inventory,
three agreeing repetitions per host, distinct instance IDs, and at least one
AMD and one Intel observation. Every host pair receives per-operation and mode
case/difference counts and an `observed-exact` or `observed-differences` verdict.
Every different record is retained in full on both sides. A successful command
means the bounded measurements are complete; it may report numerical
differences. Missing diversity, missing cases, tampered artifacts, altered
floating-point controls, or unstable repetitions fail instead of producing a
qualification report. The negative controls plant result and exception changes
while preserving valid provenance/digests and require a divergent verdict.

Exact agreement covers only the listed inputs/modes, retained binary and
observed CPU pair. It proves neither all inputs for an instruction/library nor
all CPUs, guest execution, snapshot continuation, cross-ISA equivalence, or UML
whole-state identity. This evidence does not create a qualified-CPU execution
mode or change execution, snapshot, cache, or admission identities. Those future
policy choices require separate evidence and design. Native arithmetic replay
outside these observations remains unqualified.

`Checks / Consonance / Native Math Qualification` builds one bundle and runs it
on six hosted runners weekly or on manual dispatch, then compares all fifteen
pairs. A pool draw without AMD/Intel diversity is incomplete and fails. Artifacts
retain the exact bundle, all observation trees, and aggregate JSON for 30 days.
Portable comparator controls run in repository CI; the native Linux integration
checks also execute the actual static corpus and failed-process retention path.
