# Nested guest-window counter verification

Box: AMD EPYC 7313P (Zen 3), kernel 6.18.35+ (locally patched KVM build), core 3
isolated, SMT off, performance governor, SpecLockMap workaround set, RAID resync
frozen. Posture and environment records: `00-posture.txt`, `01-environment.txt`.
Event under test: raw `0x5100d1` (`ex_ret_cond`), opened with `exclude_host=1`,
attached to the thread that enters the guest. All host-side runs `taskset -c 3`;
the L1 VM's single vCPU is pinned to core 3 and all in-guest runs `taskset -c 0`.
Oracle throughout: a real-mode guest payload whose retired-conditional-branch
count is known by analysis (`dec/jnz` loops; `out` and `hlt` retire no
conditional branches). Sources for every binary used: `ae10c.c` (the original
measurement, 20 reps), `ae11-window.c` (parameterized windows), `ae12-attrib.c`
(sampling mode). All binaries were rebuilt from these sources for this
verification; the binaries run inside L1 are checksum-identical to the host
builds.

## Verdict on the claim

**Reproduced, both halves.**

Bare metal: the guest-only counter returned the guest's branch count exactly in
every window. 60 windows at 10000/20000/30000 iterations read exactly
10000/20000/30000 (`10-metal-ae10v.json`, fixed_offset 0); 50 null-payload
windows read exactly 0 (`11-metal-null.json`); 50 more 10000-branch windows read
exactly 10000 (`12-metal-base50.json`). The closing controls after all L1 work,
same session, same core, were still exact (`60-metal-ae10v-closing.json`,
`61-metal-null-closing.json`).

Inside L1: the identical binary over-reports. Steady-state surplus
13,004-13,026 over the three payload sizes (e.g. 23,008 median for a
10,000-branch payload), against the claimed ~13,300 - same effect, ~2% smaller
on this boot (`21-l1-ae10v.json`). Within-process spread across 19 repetitions:
range 81-135, standard deviation 21-38, against the claimed ~190. Two larger
variations sit on top of that spread: level shifts of a few hundred counts over
minutes, and a first-window-after-idle excess of +10,000 to +15,400 (see
warm-up below). The claim's numbers are the steady-state ones and they
reproduce.

## Verdict on the hypothesis

**Confirmed in its general form - the surplus is the L1 side's own execution
attributed to the guest - but the stated mechanism is too narrow.** The surplus
is not confined to the window between the stop request and the counter
stopping. Inside L1 the host/guest filter has no effect at all: a counter
opened with `exclude_host=1` counts everything the vCPU retires while enabled -
L2 guest, L1 kernel, and L1 user space alike.

Deciding measurement: 1,000,000 user-space branches executed by the measuring
process itself, after counter enable and before the first `KVM_RUN`, appear in
the guest-only count in full - median 1,023,404 = 1,000,000 (user loop) +
10,000 (guest payload) + 13,394 (base surplus) (`25-l1-hostwork.json`, first
block). Those branches retire in plain user space, nowhere near an entry/exit
trap or a stop request, so the narrow trap-latency mechanism cannot explain
them. The same run on metal counts exactly 10,000 (`14-metal-hostwork.json`).
Placing the million branches after the guest halts instead (post) gives the
same result, so position in the window does not matter.

Two further measurements pin the shape:

- Sampling the guest-only counter with `PERF_SAMPLE_IP` inside L1
  (`26-l1-attrib.txt`, symbols in `31-l1-attrib-symbols.txt`): of 322 samples,
  193 land in the guest payload (ip 0x1008, the `jnz`), 128 in the L1 kernel,
  1 in L1 user space. The kernel hits are L1's own cost of running the guest:
  `clear_page_rep` (22), `__kvm_mmu_topup_memory_cache` (13),
  `svm_prepare_switch_to_guest`, `svm_vcpu_run`, `kvm_tdp_page_fault`, plus
  allocator paths feeding the nested MMU. The metal control
  (`15-metal-attrib.txt`) put 186 of 188 samples in the guest payload with 2
  boundary-skid samples in kvm_amd. So the surplus events sit in L1 kernel
  code, not in the payload - the payload is counted correctly and the excess
  is added around it.
- `exclude_host`, `exclude_guest`, and unrestricted counters opened
  simultaneously inside L1 read the same value in every configuration tested,
  differing only by a constant 9 and 32 counts of enable-order skew
  (`25-l1-hostwork.json`, all four blocks). The guest's 10,000 branches appear
  in the exclude_guest counter and the measuring process's branches appear in
  the exclude_host counter: every event is double-counted across the
  complementary pair. On metal the pair is complementary and exact
  (`14-metal-hostwork.json`: guest-only 10,000 exactly; host-only picks up the
  user loop and the VMM's kernel path, and their sum matches the unrestricted
  counter minus enable-order skew).

What this rules out: guest-side miscounting (the payload contributes exactly
its analyzed count on metal, and in L1 the count moves exactly 10,000 per
10,000 added payload branches - deltas in `21-l1-ae10v.json`), and a fixed
calibration offset (the surplus scales with exit count and with host work,
both below).

## Experiments

### E1. Reproduce on bare metal
Design: rebuild `ae10c.c`, run 20 windows each at 10000/20000/30000 guest
branches, plus 50-window null and base runs and all variants below.
Records: `10-metal-ae10v.json`, `11-metal-null.json`, `12-metal-base50.json`,
`13-metal-exits.json`, `14-metal-hostwork.json`, `15-metal-attrib.txt`,
`60-metal-ae10v-closing.json`, `61-metal-null-closing.json`.
Result: exact in all windows, in every variant, before and after the L1 work.
Zero variance.

### E2. Reproduce inside L1 (patched kernel, same build as host)
Design: same binaries inside the single-vCPU L1 (`20-l1-environment.txt`).
Records: `21-l1-ae10v.json`, `23-l1-base50.json`.
Result: surplus 13,004-13,026 steady-state; spread as above. Claim reproduced.

### E3. Null payload (pure surplus)
Design: guest executes only `hlt`; zero conditional branches by analysis.
Records: metal `11-metal-null.json`, L1 `22-l1-null.json`.
Result: metal 0 exactly, 50/50. L1: 13,074 mean (excl. first window; range
13,013-13,176). The surplus needs no guest work at all and matches the base
surplus, so it is additive.

### E4. Vary exits at constant guest work
Design: same 10,000-branch payload split by K `out` instructions, each a full
guest -> L1-kernel -> L1-user -> guest round trip; K = 0/10/100/1000, 10 reps
each.
Records: metal `13-metal-exits.json`, L1 `24-l1-exits.json`.
Result: metal exact at every K (exits cost zero on the counter). L1 surplus:
13,029 / 17,120 / 54,331 / 429,765 - linear, 409-417 extra counts per exit,
stable across two decades. The per-exit cost is the L1-visible cost of the
round trip; on metal, a host-only counter prices the same round trip at ~487
of the measuring side's own branches (`14-metal-hostwork.json`, third block).

### E5. Vary host work at constant guest work and exits
Design: 1M user branches at pre, at post, and 10,000 per exit at mid
(100 exits); dual counters open.
Records: metal `14-metal-hostwork.json`, L1 `25-l1-hostwork.json`.
Result: described under the verdicts. This is the experiment that decides the
hypothesis's shape.

### E6. Attribute the surplus by sampled IP
Design: guest-only counter in sampling mode, period 500, over a
200,000-branch payload.
Records: metal `15-metal-attrib.txt`; L1 `26-l1-attrib.txt` +
`27-l1-kallsyms.txt` + `31-l1-attrib-symbols.txt`.
Result: described under the verdicts. The sampling run's own count rises to
~249k-252k for the 200k payload: the sampling machinery's L1-side work is
itself counted, consistent with the mechanism.

### E7. Determinism at fixed exit count and fixed host path
Design: 50 identical windows per process (`23-l1-base50.json`), 10-rep groups
minutes apart (`24-l1-exits.json`, `30-l1-idlewarmup.txt`).
Result: genuinely variable. Within a process (excl. first window): stdev ~29,
range 144. Between groups minutes apart, medians move by up to ~600 (22,957 ->
23,595). On metal the identical arrangement has zero variance, so the variance
is variation in how much L1 work runs inside the window (interrupts, allocator
state), not counter noise.

### E8. First measurement vs later ones
Design: run the tools back-to-back and after a 120 s idle gap, several process
launches each way.
Records: `21-l1-ae10v.json`, `28-l1-ae10v-second.json`,
`29-l1-firstwindow.json`, `30-l1-idlewarmup.txt`.
Result: the first window of a process launched right after other work pays
+330 to +780. The first window after ~2 minutes of idle pays +9,886 to
+10,736; after a measured 120 s idle gap, +15,421. The excess follows idle
time, not the program: the same binary shows the large excess only when it
runs first after idle. This is a measurement of extra L1 work in the first
window, which lands in the count because all L1 work lands in the count.
Which code the extra work runs in was not attributed; a sampling-mode first
window after idle would answer it.

### E9. Dependence on the patched kernel build
The kernel patches (host build 6.18.35+) add a paravirt clocksource, a
character device, and per-VM opt-in KVM exit behavior gated behind
`KVM_ENABLE_CAP` (`KVM_CAP_X86_DETERMINISTIC_INTERCEPTS`); the default-off
path is intended byte-identical to stock (patch text under
/root/harmony/harmony-linux/linux/patches/x86/ and
/root/harmony/consonance/vmm-backend/kvm-patches/patches/; SVM analogue in
/root/harmony/qualification-evidence/box/stage2/amd-svm-reanchored.patch).
No measurement here enables that cap.
Design: boot L1 with three different kernels and rerun the core set.
Records: patched 6.18.35+ (`2x-*` files), unpatched 6.18.35 built without the
patch series (`l1-vanilla/40-...45-`), stock Debian 6.12.101+deb13-amd64
(`l1-stock/50-...55-`).
Result: the effect is unchanged in kind on all three inner kernels. Steady
surplus 13,015 (patched) / ~13,430 (unpatched 6.18.35) / ~12,660 (Debian
6.12.101); per-exit cost 413 / 413 / 422; the pre=1M user branches are counted
in full on all three; kernel-mode samples present in all three attribution
runs (128 / 86 / 97). The size differences track the kernel build (different
amounts of work in the same paths), not the presence of the patches. The
outer (L0) kernel could not be swapped without rebooting the host, which was
out of scope; see unsettled items.

## What I measured vs what I infer

Measured: metal exact everywhere; L1 surplus additive (+13k base), linear in
exits (~413/exit), inclusive of all measuring-process user work, present on
three different inner kernels, double-counted across complementary exclude
filters, sampled overwhelmingly in L1 kernel guest-support code; variance and
warm-up as in E7/E8.

Inferred (consistent with all of the above, not separately proven): the outer
KVM's emulation of the L1 PMU does not implement the AMD host/guest-only
eventsel bits for a nested guest, so a "guest-only" virtual counter becomes a
"count while this vCPU runs" counter. The magnitudes close the ledger: the L1
surplus per window (13.0k) and per exit (413) are the same scale as the
measuring side's own directly-measured cost of the same operations on metal
(14.5k per window, 487 per exit, host-only counter), leaving no residue that
would require some other contributor.

## Not settled

- Whether some part of the surplus is L0's own execution rather than L1's.
  In-L1 sampling cannot rule it out (a virtual PMI lands on whatever L1 code
  runs at delivery). The magnitude arithmetic above leaves little room for an
  L0 component, but a deciding experiment would correlate an L0-side counter
  on the vCPU thread with the movement of L1's virtual counter over the same
  window.
- The unpatched-outer-kernel cell of the matrix. It needs a host reboot into
  the stock kernel, excluded here. The three-inner-kernel result plus metal
  exactness through the patched L0 (the non-nested path of the same build is
  exact) is the strongest statement available without that reboot.
- Which specific code the first-window-after-idle excess runs in (E8 names
  the experiment).

## File inventory

All under /root/qual-evidence/nested-verify/: `00-posture.txt`,
`01-environment.txt`, sources `ae10c.c` / `ae11-window.c` / `ae12-attrib.c`,
metal records `10-` through `15-` and `60-`/`61-`, patched-L1 records `20-`
through `30-` (also bundled in `l1-patched-outputs.tgz`), symbol table
`31-l1-attrib-symbols.txt`, unpatched-6.18.35 records `l1-vanilla/`, stock
Debian records `l1-stock/`.
