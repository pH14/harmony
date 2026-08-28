# Consonance determinism contract

This is the document of record for Consonance's determinism claim. It replaces
`docs/VM-EXIT-COUNT-VTIME-CLOSURE.md`; historical milestone ledgers remain
evidence, while this file owns the current argument, assumptions, support
matrix, instruction dispositions, and verification backlog. The frozen
machine-readable instruction table is
[`determinism-instructions.toml`](determinism-instructions.toml).

## 1. The argument

For a fixed architecture, seed, guest image, and input stream, Consonance models
a run as one totally ordered stream of VM exits. Virtual time advances only at
those exits, by a function of the normalized exit class and payload; equal
deadlines are delivered in FIFO order at the first exit whose post-advance time
reaches them. These rules and their independent placement oracle are recorded in
the design and ARM evidence ledgers
([clock and delivery design](VM-EXIT-COUNT-VTIME.md#21-the-clock),
[verification discipline](VM-EXIT-COUNT-VTIME.md#3-verification),
[M5 cross-host evidence](VM-EXIT-COUNT-VTIME-STATUS.md#m5--bidirectional-hvfkvm-determinism-and-snapshot-portability)).

The determinism argument is induction over that stream:

1. At event zero, the seed, attested guest bytes, normalized machine state,
   virtual clock, and deterministic entropy position are fixed. The ARM M5
   evidence records byte-attested inputs and canonical initial/restore state
   ([M5](VM-EXIT-COUNT-VTIME-STATUS.md#m5--bidirectional-hvfkvm-determinism-and-snapshot-portability));
   the x86 X3 evidence records one normalized byte sequence across Intel and AMD
   ([X3 at `57b16ce0`](https://github.com/pH14/harmony/blob/57b16ce0/docs/PRESCRIPTIVE-VTIME-STATUS.md#x3--cross-vendor-identity)).
2. Assume the normalized state before event *n* is equal. Execution until the
   next exit is deterministic within the confined ISA subset described in §2.
   Therefore the next normalized exit class and payload are equal.
3. Exit handling, virtual-time advancement, device transitions, entropy draws,
   and due-interrupt ordering are pure functions of the prior normalized state,
   seed, and exit. The full-state checkpoint and independent schedule checks
   exercise this claim rather than accepting final-state equality
   ([verification rules](VM-EXIT-COUNT-VTIME.md#3-verification)).
4. Therefore the normalized state after event *n* is equal, and induction gives
   an equal complete normalized log and checkpoint sequence.

The host is not an input to those transition functions. Portability between
hosts of the same ISA is therefore the same theorem, conditional on the
cross-implementation assumption in §2.2; it is not a separate form of
determinism. Raw backend logs may differ because substrates surface different
mechanical exits, but those exits have no portable ordinal or virtual-time
effect unless they correspond to a guest-visible event
([raw-versus-normalized ruling](VM-EXIT-COUNT-VTIME-STATUS.md#recorded-decisions)).

This is a closed-system claim. Network packets, disk contents, inputs, and
external decisions enter only through deterministic Consonance models and the
recorded input stream. Support for a new external device or asynchronous source
is **untested** until its complete state and ordering join the normalized model.

## 2. Assumptions and confinement

### 2.1 Same-host replay assumption

**Assumption A:** one CPU implementation executes the admitted architectural
subset deterministically from equal architectural state. Consonance confines
the guest to that subset through four layers:

1. the protocol routes time, entropy, and identity through Consonance-owned
   interfaces;
2. image audits reject forbidden executable opcodes from owned images;
3. the owned guest kernel confines unaudited userspace with architectural trap
   controls where they exist; and
4. optional substrate intercepts are fail-loud tripwires, not the portability
   foundation.

The ARM image scanners' planted negatives and real-image results, the HVF
sysreg probe, and the ten-run boot oracle are recorded under M1
([M1 evidence](VM-EXIT-COUNT-VTIME-STATUS.md#m1--the-m1-max-boots-deterministically)).
The x86 X2 ledger records the pvclock-only kernel, CPUID masking, and opcode
accounting used on stock KVM
([x86 decisions 10–20](https://github.com/pH14/harmony/blob/57b16ce0/docs/PRESCRIPTIVE-VTIME-STATUS.md#recorded-decisions-x86)).
The complete hostile JIT and traps-off proof is **untested**; §3 makes it N6
work.

### 2.2 Cross-host portability assumption

**Assumption B:** different implementations of one ISA agree bit-for-bit on the
admitted subset. This is stronger than Assumption A and is required only for
portability. The ARM identity contract pins ID/cache properties and canonicalizes
unsupported PSTATE residue; M5 records byte-identical complete boot and NES
campaign traces across Apple HVF and Linux KVM
([M5](VM-EXIT-COUNT-VTIME-STATUS.md#m5--bidirectional-hvfkvm-determinism-and-snapshot-portability)).
The x86 branch pins CPUID, XSAVE encodings and `MXCSR_MASK`, unusable-segment
residue, exit-time `RFLAGS.RF`, and architecturally undefined `AF` capture; X3
records one complete normalized log across Intel and AMD
([x86 decisions 19–30 and X3](https://github.com/pH14/harmony/blob/57b16ce0/docs/PRESCRIPTIVE-VTIME-STATUS.md#x3--cross-vendor-identity)).

Those measurements establish the named workloads on the named machines; they do
not prove every instruction encoding or future CPU. A complete generated
instruction sweep and every support-matrix cell still marked expected are
**untested**.

## 3. Frozen instruction defenses

The normative rows are in
[`determinism-instructions.toml`](determinism-instructions.toml). Each row names
its architecture, instruction class, closure layers, disposition, evidence, and
current verification state. N6 generates its sweep from those rows; adding a
row without adding its probe must fail the build. `claim` has only two values:
`execute` for an operation whose handling is exercised, and `mask-and-audit`
for an unprivileged entropy operation that stock substrates cannot intercept.

### 3.1 arm64

| Channel | Frozen rows | Defense | Current evidence |
|---|---|---|---|
| Time | `arm64-counter-frequency`, `arm64-virtual-counter`, `arm64-physical-counter`, `arm64-live-timer-programming` | pvclock protocol; complete owned-image opcode scan; EL0 counter access disabled | M1 scanner negatives and same-seed boot; JIT/traps-off proof **untested** |
| Identity | `arm64-cache-identity`, `arm64-id-registers` | frozen backend ID/cache model; audit of unsafe revision reads | M5 HVF↔KVM equality; complete generated listing **untested** |
| PMU | `arm64-pmu` | PMU absent from the frozen identity; EL0 disabled; substrate trap/policy | HVF `PMCCNTR_EL0` trap measured; exhaustive sweep **untested** |
| Entropy | `arm64-entropy` | hide FEAT_RNG and reject `RNDR`/`RNDRRS` from admitted images | mask-and-audit proof **untested** |
| Exclusive monitor | `arm64-exclusive-monitor` | owned kernel remains LSE-only; only side-effect-free EL0 retry loops are admitted under the cooperative posture | image audit proven; convergence and accumulating negative **untested** |

The ARM facts above cite the M1/M5 evidence in the corresponding TOML rows.
HVF was measured not to trap `CNTVCT_EL0`, so no claim relies on it doing so;
the owned image and EL0 trap policy are essential
([M1 probe](VM-EXIT-COUNT-VTIME-STATUS.md#m1--the-m1-max-boots-deterministically)).
The LL/SC relaxation stops at side-effect-free EL0 retry loops: retry-observing
programs and every owned-kernel LL/SC site remain outside the admitted subset.
The accumulating-retry divergence that bounds this ruling is **untested** until
N6.

### 3.2 x86-64

| Channel | Frozen rows | Defense | Current evidence |
|---|---|---|---|
| Identity | `x86-cpuid` | frozen CPUID model with the stock-mode RNG variant | X3 Intel↔AMD equality; exhaustive leaf sweep **untested** |
| Time | `x86-tsc` | pvclock protocol and owned-image exact-accounting scan; `CR4.TSD` handling for unaudited ring 3 | X2 owned image proven; JIT/traps-off proof **untested** |
| PMU | `x86-pmu` | no vPMU, `CR4.PCE=0`, stock-KVM intercept | X-series boot evidence; hostile probe **untested** |
| Entropy | `x86-entropy` | hide RDRAND/RDSEED feature bits and reject/feature-gate every admitted opcode site | kernel scan proven; initramfs scan and hostile probe **untested** |
| Wait/power | `x86-monitor-mwait`, `x86-waitpkg` | feature hiding plus deterministic fault/intercept policy | contract tests exist; consolidated-tree hostile probe **untested** |
| Save encodings | `x86-xsave-image` | canonicalize XSAVE init encodings, `MXCSR_MASK`, and ignored tail bytes at guest/VMM save boundaries | X3 cross-vendor equality; exhaustive form sweep **untested** |
| Exit/flag residue | `x86-exit-rflags-rf`, `x86-undefined-af` | clear unreadable exit-time RF at save; clear undefined AF at the three kernel capture funnels | X3 cross-vendor equality; dedicated generated probes **untested** |

Stock KVM does not expose userspace exits for `RDTSC`, `RDRAND`, or `RDSEED`;
the stock composition therefore depends on protocol, audit, guest confinement,
and feature masking rather than pretending those exits exist
([x86 decisions 12–21](https://github.com/pH14/harmony/blob/57b16ce0/docs/PRESCRIPTIVE-VTIME-STATUS.md#recorded-decisions-x86)).
An unaudited binary that executes `RDRAND`, `RDSEED`, `RNDR`, or `RNDRRS` while
ignoring the hidden feature bit is a documented residual of the cooperative
guest posture.

## 4. Where Consonance runs

“Proven” means the cited committed evidence exists. “Expected” is **untested**.
Within a column, portability means the same seed and image produce the same
normalized bytes; no cross-ISA byte identity is claimed.

| Host | x86-64 Intel | x86-64 AMD | arm64 |
|---|---|---|---|
| Linux KVM, bare metal | expected | expected | **proven** ([M4–M5, msr1](VM-EXIT-COUNT-VTIME-STATUS.md#m4--complete-the-arm64-kvm-backend-on-msr1)) |
| Linux KVM, nested in a VM | **proven** ([X2/X3](https://github.com/pH14/harmony/blob/57b16ce0/docs/PRESCRIPTIVE-VTIME-STATUS.md#x2--linux-boots-to-init-deterministically-on-the-runner)) | **proven** ([X2/X3](https://github.com/pH14/harmony/blob/57b16ce0/docs/PRESCRIPTIVE-VTIME-STATUS.md#x2--linux-boots-to-init-deterministically-on-the-runner)) | expected where the host offers nested virtualization |
| Linux KVM, inside a container with `/dev/kvm` | expected | expected | expected |
| macOS HVF, bare-metal Apple silicon | — | — | **proven** ([M0–M6, M1 Max](VM-EXIT-COUNT-VTIME-STATUS.md#m1--the-m1-max-boots-deterministically)) |
| macOS HVF, nested in a macOS VM | — | — | expected on supported M3+/macOS 15+ hosts |

The requirements exercised by the proven cells are one vCPU, hardware
virtualization at any tested nesting depth, the owned guest image, and no host
performance counter as the virtual-time source
([VM-exit-count design](VM-EXIT-COUNT-VTIME.md#2-design)). Windows hosts and
Intel Macs are out of scope for the consolidation plan
([N0–N6 plan](VTIME-CONSOLIDATION.md#4-out-of-scope)).

## 5. Trust boundary

The hypervisor layer is trusted to preserve the architectural state it exposes,
apply the configured CPUID/sysreg and interrupt policy, and report exits without
inventing guest-visible transitions. Consonance does not trust substrate-local
exit counts or raw logs to match; normalization removes mechanics that have no
guest-visible counterpart
([ARM decision 12](VM-EXIT-COUNT-VTIME-STATUS.md#recorded-decisions)). Whether
every relevant HVF and KVM behavior has been covered by an adversarial probe is
**untested**.

The guest kernel is equally part of the trusted implementation. It routes time
through the pvclock page, confines userspace counter/PMU access, pins
implementation-defined save artifacts, supplies deterministic exit density,
and rejects forbidden opcodes from owned executable images. Therefore an
arbitrary Linux image does not satisfy this contract. ARM M1/M5 and x86 X2/X3
record the image-specific evidence cited above; N4 moves the guest tree under
`consonance/` to make this ownership visible.

Manifest SHA-256 values prove that two runs consumed the same bytes; they do not
prove the source was built reproducibly or that the bytes implement the
argument. The ARM M5 evidence compares hashes and full logs
([M5](VM-EXIT-COUNT-VTIME-STATUS.md#m5--bidirectional-hvfkvm-determinism-and-snapshot-portability)).
Reproducible source-to-image construction is **untested** until N5.

## 6. Decisions carried into consolidation

1. **KVM patches 0004 and 0005 retire in N2.** Force-exit preemption and MTF
   single-step exist only for retired-branch exact-arrival delivery. VM-exit-count
   virtual time never stops the guest between exits
   ([delivery design](VM-EXIT-COUNT-VTIME.md#21-the-clock)).
2. **KVM patches 0001–0003 remain optional tripwires.** Their instruction exits
   may audit protocol/audit/trap closure on a patched Linux host, but no support
   claim depends on them because the proven x86 runner uses stock KVM
   ([X0 stock capability evidence](https://github.com/pH14/harmony/blob/57b16ce0/docs/PRESCRIPTIVE-VTIME-STATUS.md#x0--runner-probe)).
   Their observational inertness and live stray detection are **untested** until
   N6.
3. **LL/SC relaxation is bounded, not global.** Side-effect-free EL0 retry loops
   may be admitted under the cooperative posture. The owned kernel remains
   LSE-only, and retry-observing programs are unsupported because retry count can
   retain host scheduling residue. The empirical boundary is **untested** until
   N6.
4. **New owned kernels, initramfs programs, payloads, dynamic loaders, and shared
   objects are audited at protocol plus executable-image layers.** A new
   unaudited userspace class additionally requires the architecture's kernel
   trap policy and must not contain a residual entropy opcode that ignores
   feature masking. New device or external-I/O workload classes require a
   deterministic model, complete snapshot/hash state, normalized ordering, and
   a planted comparator negative before support is claimed
   ([verification rules](VM-EXIT-COUNT-VTIME.md#3-verification)).

## 7. Verification ledger for N6

The following claims remain **untested** unless a cited earlier milestone
already proves them:

- generated instruction sweeps whose exercised-row count exactly equals the
  frozen table row count on both architectures;
- JIT-emitted counter probes and traps-off negatives for arm64 and x86;
- complete arm64 sysreg enumeration against a committed architectural listing;
- the side-effect-free LL/SC convergence positive and accumulating-retry
  divergence negative;
- optional KVM tripwires being observationally inert when the guest is closed,
  and live when a closure layer is deliberately disabled;
- the arm64 `RNDR`/`RNDRRS` and x86 initramfs `RDRAND`/`RDSEED` opcode audits.

N6 updates these markings from evidence; it does not erase an untested label by
argument alone.
