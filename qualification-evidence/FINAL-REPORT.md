Item 1 - starter pack registered and shipped
docs/chips/det-zen3-v1.toml modeled on det-cfl-v1: identity family 0x19 model 0x01
stepping 0x01, AuthenticAMD, microcode 0xa0011a9; work clock raw 0x5100d1 ex_ret_cond;
every unmeasured field absent with a reason; host conditions written as chips.rs
HostConditionKind tokens. Registered in pack.rs's builtin table, resealed, public-API
snapshot refreshed under UPDATE_PUBLIC_API=1, all five gates green, commit a6c6e6d2,
shipped by git archive to the box and rebuilt there.

Item 2 - posture
Before: kernel 6.12.95+deb13-amd64, no mitigation flag on the command line, LS_CFG
0x0004480000000000 on cpu0, SMT on with 32 threads, nmi_watchdog on.
After: spec_store_bypass_disable=on in the command line and confirmed by
/sys/devices/system/cpu/vulnerabilities/spec_store_bypass reading "Mitigation:
Speculative Store Bypass disabled"; LS_CFG 0x0044480000000000 read back on every online
CPU with bit54 set and bit10 clear; nmi_watchdog 0; governor performance on all 16;
SMT off, active=0, 16 online. Volatile settings are re-applied by /root/posture.sh after
every reboot, and the file it writes is the evidence for that boot. Two later additions
to the standing posture: cores 1,3,5,7,9,11,13,15 isolated for the parallel landing
campaign, and the kernel's sampling ceiling raised - see item 7.

Item 3 - stage 0
Exit 0 twice, the second after a reboot; 50 rows each run, row sets identical, zero
deviations. The close's third run has 52 rows, because item 4 added the two
sampling-ceiling conditions to the pack; nothing else moved. Two suite defects were
found and fixed first: the AMD speculative-lock-map probe used the wrong event and the
wrong polarity (rr requires the speculative-lock-map-commit counter to stay at zero),
and one host-condition expectation was being compared against two different module
scopes. Commits 49e86362, bd434471; the spec's description of that
probe was corrected with it.

Item 4 - stage 1 (campaign E, the qualifying run)
report --evidence-dir /root/qual-evidence/stage1-E exits 0 with 36 of 36 checks against
floors {min_clean_reps 32, min_overflow_arms 1250000, skid_margin 16192}.
- Exactness: 16 payload/condition groups (5 payload classes x 3 interference conditions,
  plus the guest payload), 512 windows attempted each, 502-511 interrupt-free, zero
  mismatches, offsets stable. Recomputed outside the suite: 8136 interrupt-free windows,
  813,600,000 work-clock events judged exactly against the analytical oracle.
- Overflow: 1,250,000 arms, contiguous indices, 1,250,000 delivered exactly once, zero
  lost, zero duplicated, zero premature, zero unaccounted, zero over margin. Per-payload
  skid maxima 272 / 498 / 1198 / 1604 / 8007.
- Skid across all four campaigns, recomputed from the raw records outside the suite:
  5,000,000 arms, none delivered other than exactly once, none premature, none lost.
  The largest skid is 8096 and it comes from campaign C, which did not pass: 5,548 of
  its arms were thrown away by the stock kernel sampling ceiling and are unaccounted,
  as were 5,542 in campaign A2. So the maximum is over 4,988,910 accounted arms. The
  two campaigns run after the ceiling was raised, D and E, lost none, and E's maximum
  is 8007. The pack seals 8096 rather than 8007 because it is the larger of the two
  and so the more conservative margin; the pack says which campaign it came from and
  that 11,090 arms are unaccounted behind it. That ceiling is now one of the pack's
  standing host conditions, so a future run on this baseline cannot silently measure
  under the stock ceiling and lose interrupts again.
- Fixpoint: components regs, sregs, xsave, xcrs, msrs, vcpu-events; 6040 bytes both
  times, nothing differing. Three free-running time bases held to advancing within a
  rate-derived bound rather than to equality. Two registers excluded by name with the
  reason: MSR_KVM_ASYNC_PF_INT (0x4b564d06), which the window's vCPU does not own
  because it has no in-kernel LAPIC, and HV_X64_MSR_TIME_REF_COUNT (0x40000020),
  read-only synthetic hypervisor state whose host write Linux discards by design.
- Interference: co-tenant, memory pressure and quiet all produced the same clean delta
  of 100000 on every payload. The SMT-sibling condition is not planned, because the
  baseline's own posture requires that thread off.
Six defects in the suite were found and fixed on the way (commits 01c2aee6, 7c7aa587,
69371af6, 7f8a6f88, dfc7c7a4, 51a44b7b), each one a case where the suite could not have
passed on a correctly-postured host.

Item 5 - pack filled and shipped
Filled: skid.observed_max 8096, skid.margin 16192, skid.derivation stating both the
observation and that the campaigns were judged against a tighter 16068 so the seal is not
a bound widened to fit; event_density 1 event per iteration for all five payload classes;
two new host conditions for the kernel's sampling ceiling.
Absent with reasons: count_offsets (stage 2 measures it and the suite's stage 2 is not
built) and single_step.work_per_step (the spike single-step harness never opens the work
clock, and the suite's stage 2 is not built).
Sealed e233f43ad4a6272a9a9cbebd9bc059b7407fd5e8018859680195e2ab68374be2 at this point.
The stage-2 campaign then measured the guest-mode skid distribution and the overshoot
handling, which belong in the same three fields, so the pack was resealed once more at
the end of item 7 and again after the supplement. The final seal is
61a0829b0069195daf5e611a4576cd1bd394c476c8c59e76f18e9ea64d58504d. The margin never moved,
and skid.observed_max never moved; what changed each time is what the derivation and the
overshoot field say, and the last reseal also added core-isolated to host_conditions.

Item 6 - patched kernel
Blocker found and cleared on the first of the two permitted attempts. The shipped
amd-svm.patch is a malformed diff, not context drift: hunk 2 declares @@ -5447,4 +5447,8 @@
for a body of 4 old and 10 new lines, its blank context line has no leading space, and the
diffstat claims 23 insertions for 22 lines. Both edits were re-anchored by hand at verified
unique context in 6.18.35 svm.c and committed in the kernel tree as 52248ae11 (23
insertions), saved as qualification-evidence/box/stage2/amd-svm-reanchored.patch.
Built in 18m20s. The install step then refused on a false negative of its own: its initrd
check is grep -q under pipefail, so an early match returns 141 and reads as missing. Both
root-stack modules are present; verified by hand, staged, and pre-flighted by booting the
kernel and its initramfs inside a KVM guest on the box before rebooting the host. Booted
in 70 seconds: uname 6.18.35, /dev/kvm present, kvm_amd nested=1 avic=N.
Attestation: image .deb sha256 8f9f151533a42763fbc9c4a0f611ed7d7f66b9607d165e6a3ee17aa6fc0c9d2a,
kvm-amd.ko sha256 573f95fab0b7211ed242b57963cd59759d164392f54eb24e403036aa275534b6,
build-id 5fbf9bc0c714acb76b323a297a82c489d226c516.

Item 7 - stage-2 measurements

(a) Single-step driver. TF works on SVM and there is no monitor-trap facility, as the
spike ruling said. Debug exits match the instruction oracle exactly on four of five
payloads; the fifth differs by the movss shadow, an x86 rule rather than a chip
property. BTF delivers no traps at all on this part. Full write-up in
qualification-evidence/stage2-stock-findings.md.

(b) Landing campaign. Eight shards on the eight isolated cores, 62,500 random targets
each, every target armed twice so the second arm is a replay of the first. 500,000
targets, 1,000,000 landings. 838,014 of those arms had a target above the margin and so
went through the overflow-then-single-step path; the other 161,986 were below the margin
and single-stepped from the start. Recomputed from the per-arm records rather than the
shard summaries:
- 838,008 of the 838,014 overflow arms took the KVM_EXIT_PREEMPT deterministic exit and
  then landed on work == target. 419,007 of those carry the exit reason in the record;
  for the other 419,001, which are replay arms of the same targets, the record does not
  carry it and it is inferred from the harness's control flow. That gap is why the
  supplement in (b2) was run.
- Zero lost overflows, zero duplicates, zero re-primes, zero arms unaccounted. Shard
  indices are contiguous.
- 424,978 landings had at least one interrupt during the window and still landed exactly.
- Six arms overshot: two first arms and four replay arms, one in 139,669. All six are in
  (c).
- Guest-mode skid over the 419,007 first arms: min 1,202, median 2,915, p99 3,251,
  p99.9 5,371, p99.99 7,583, max 37,595. Per-shard maxima 7,026 / 7,423 / 7,443 / 7,998 /
  8,042 / 13,060 / 14,718 / 37,595. Two shards exceeded stage 1's host-user maximum of
  8,096 and still landed inside the 16,192 margin.
- Landing is exact under repetition. 99,345 distinct work counts were landed on, every
  one of them more than once, 94,625 of them from more than one core, and one of them 36
  times. 999,992 landings sit at a repeated work count. Not one work count produced two
  different landed states. Script qualification-evidence/box/stage2/repetition.py.
Four shards exited nonzero while their own summary line reported no failure. That is
resolved from the records in qualification-evidence/shard-exit-codes-and-recovery.md: the
exit code is an all-arms conjunction that includes replay agreement, and the summary line
carries no replay quantity at all, so a replay overshoot fails the exit code invisibly.
Each of those four shards has exactly one failing arm and replay agreement is its only
failing term. The three shards that exited zero have no failing arms. It is a harness
reporting gap, not a measurement failure, and it is the reason nothing in this report
rests on a summary line.

(b2) Attested-exit supplement. The campaign records the deterministic exit for the first
arm of a target and not for its replay, so only about 419,000 of its landings carry the
attestation item 7 asks for. An instrumented build of the same harness, differing from it
only in what it writes down, was run fifteen wide to close that gap: cores 0,1,2 and 4
through 15, 24,000 targets each, every target drawn above the margin so every arm went
through the overflow path, every target armed twice. 360,000 targets, 720,000 landings,
720,000 arms exposed to an overshoot, all fifteen shards run to completion.
- 720,000 deterministic exits, every one attested in the record and none inferred.
- Zero lost overflows, zero duplicates, zero re-primes, zero unaccounted. Indices
  contiguous on all fifteen shards.
- 56 overshoots, all refused, all recovered: the harness re-armed each one and 56 of 56
  landed exactly.
- The digest inversion scores 360,000 agreements and 0 disagreements here.
- Repetition: 82,709 distinct work counts landed on, every one more than once, 76,987 of
  them from more than one core, one of them 34 times, and not one work count produced two
  different landed states.
- Together with the campaign this puts 1,139,007 deterministic exits in the record with
  the mechanism attested, above item 7's floor of a million.

(b3) What the supplement found: core isolation, not the chip, is what bounds the tail.
Seven shards ran on cores the kernel keeps off itself and eight on cores it schedules on,
same chip, same run, same posture, same target draw.

                       isolated cores    ordinary cores
  arms exposed             336,000           384,000
  overshoots                     1                55
  rate                 1 in 336,000       1 in 6,981
  skid p50                   2,908             2,910
  skid p99                   3,024             3,014
  skid p99.99                7,539            35,346
  skid max                  37,616            86,738

The bulk of the two distributions is the same to within a few counts and the tail is not:
a core the kernel schedules on is 48 times likelier to carry the guest past its deadline.
SMI, guest speed and target size were all checked and none of them separates the failing
arms. Full write-up in qualification-evidence/core-isolation-and-the-skid-tail.md.
Consequence: the pack now carries core-isolated as a host condition and stage 0 reads it
from /proc/cmdline against the core the measurement thread is actually pinned to. Before
this, core-pinning said the thread was on cpu3 and nothing said whether the kernel stayed
off cpu3, so a host could satisfy every sealed condition and still measure in the
population with the 48-times-worse tail. Demonstrated on the box in
box/stage2/isolation-enforced.txt: check exits 0 on the isolated core and refuses with
rc=2 on a core the kernel schedules on.

(c) Overshoot: detection and recovery. Six events in the campaign, one in 139,669 of the
838,014 arms exposed to it, a rate of 7.2e-6.
- Detection is loud and cannot be mistaken for a landing. An overshoot fails the harness's
  exactness test outright, and on the suite side VtimeError::SkidExceeded is raised and
  mapped to a backend error; consonance/vtime/tests/planner.rs::skid_exceeding_margin_is_loud
  is the gate for it. No overshoot in any run was recorded as a landing.
- Four of the six are replay arms, whose landing the record does not describe. They were
  recovered by inverting the state digest: the payload's digest is over the instruction
  pointer and the loop counter, and over the 600,002 reachable pairs no two collide, so a
  digest names its state uniquely. The inversion is scored rather than trusted - against
  7,000 replay arms whose landing the instrumented harness states outright, it agrees
  7,000 times, disagrees 0 and finds 0 digests outside the dictionary. Recovered skids
  29,884 / 50,432 / 52,737 / 56,725, which is why the guest-mode maximum this chip
  produced is 56,725 and not the 37,595 the records show unaided.
- Recovery is measured, not assumed. In the campaign, all six overshot targets were landed
  exactly by the other arm of the same target: 6 of 6. A dedicated run at a deliberately
  small margin of 3,072 produced 113 overshoots in 10,000 arms and re-armed each one:
  113 of 113 landed exactly, 111 after one re-arm and 2 after two. A third run took the
  campaign's own worst target, 85,981, at the sealed margin of 16,192 and armed it 4,000
  times: 4,000 exact, no overshoot. Records overshoot-recovery.json,
  overshoot-target-85981.json, overshoot-demo.log.
- The retry is not in the suite or the backend. It is a contract a consumer must
  implement; condition 1 of the verdict states it.
- Clustering was tested and there is none. The six events fall on cores 3, 5, 9, 11 and
  15 (twice), which is all four of this socket's L3 domains on its single NUMA node.
  Device interrupts are affinitised to the even cores; the odd cores took one device
  interrupt between them over the whole campaign. No payload or target-size concentration.
- The SMI correlation asked for cannot be measured on this part, and that is itself the
  finding. MSR_SMI_COUNT (0x34) is an Intel MSR and does not exist here, and the AMD
  fam-19h PMC that documents an SMI count (0x51002b) reads zero on this silicon. The
  probe returned a delta of zero on all 10,000 demonstration arms including all 113
  overshoots, which is what a probe that does not work returns, so it is reported as a
  null instrument rather than as evidence of no SMIs.

(d) Enforcement against the AMD draft column. Run twice: on the stock kernel and again on
the patched 6.18.35 the draft column names. Every record is byte-identical between the
two, which is the expected result once the two gaps below are read and is worth having
measured. Write-up in qualification-evidence/patched-kernel-enforcement.md.
- CPUID freeze is enforced. A feature bit set on the host and cleared in the frozen
  model reads back clear inside the guest, and the vendor string is AuthenticAMD.
- MSR default-deny is enforced. The read is trapped to the VMM rather than delivered.
- RDTSC is interceptable on this chip - SVM's vector carries INTERCEPT_RDTSC and
  INTERCEPT_RDTSCP - but the determinism patch series wires the intercept only in
  vmx.c. That is a gap in the series, reported and not worked around, in
  qualification-evidence/amd-determinism-kernel-gap.md.
- RDRAND and RDSEED are not interceptable on this chip at all. SVM's intercept vector
  has no control for either where VMX has both. Measured, not only read: with leaf 1
  ECX bit 30 cleared in the frozen model, the guest executed RDRAND anyway, no fault,
  carry set, ten distinct values over five runs. CPUID masking changes what the guest
  is told and nothing about what it can do.
  qualification-evidence/amd-rdrand-not-interceptable.md.

Item 8 - close
Stock kernel 6.12.95+deb13-amd64 booted and verified, after a detour worth reporting: it
was not on the box any more. Installing the kernel build dependencies in item 6 pulled
Debian's linux-image-amd64 meta-package forward to 6.12.101 and removed 6.12.95's image,
at a moment unrelated to any measurement. Booting 6.12.101 would have failed stage 0 on
both kvm-module-identity rows, which is the pack doing exactly what it is for. 6.12.95-1
is still carried by trixie-security, so it was reinstalled and the GRUB default pinned to
it by name; its KVM modules read back with the same two build-ids the pack seals. Write-up
in qualification-evidence/stock-kernel-moved-under-the-program.md.
Stage 0 run 3 on it: exit 0, 53 rows, 3 added versus runs 1 and 2 and 0 removed, and the
3 added are exactly the sampling-ceiling pair from item 4 and core-isolated from item 7.
`check --baseline det-zen3-v1` on the box: exit 0. `report` on the qualifying stage-1
campaign against the pack as finally sealed: exit 0. Evidence synced to
qualification-evidence/box. Final commit ac7fe893, not pushed; the pack itself is sealed 61a0829b and was last changed in d35050cc.

Code changed, and why each one was in scope
Only the pack, plus defects the box exposed, one commit each.
- Eight suite defects across items 3 and 4, each a case where the suite could not have
  passed on a correctly-postured host. Commits listed in items 3 and 4.
- Three stage-1 arithmetic tests asserted one architecture's payload constant, so they
  were red on x86_64, the only architecture this crate measures on, while green on the
  aarch64 development host. Scaled by the spec instead. The box is where that was visible.
- The saved-MSR decode tripped a clippy lint that exists on current stable but not on the
  older stable the development host carries; the repo pins no toolchain, so the gate was
  red on Linux. One line.
- core-isolated added as a host condition, with its probe and its pack entry, on the
  strength of the supplement measurement in item 7.
After the close the workspace was built, linted and tested on the box, which is the
platform combination a macOS development host cannot see: Linux, x86_64, current stable.
cargo build exits 0 and cargo test exits 0 with 140 test binaries and 1,469 tests, none
failing. Clippy surfaced one thing outside this program's scope and it is recorded rather than fixed:
the same clippy lint fires in four other crates, so the workspace clippy gate does not
compile on current stable. Sites and reasoning in
qualification-evidence/linux-clippy-on-current-stable.md. The root cause is that
rust-toolchain.toml pins no version, so "stable" means a different linter on each host.

Absent pack fields, both with the same cause
- `count_offsets`: a count offset is the work an exit of a given class contributes on
  top of the guest's own, which the suite measures in stage 2. Stage 2 is not built and
  this program was told not to build it, so no run on this chip has produced one. Stage
  1 measures a different quantity, the fixed count around a payload loop: recomputed
  from the raw records it is 6 for branch_dense, call_ret, locked and loop_backedge,
  7 for straight_line and 0 for the guest payload, with every one of 3,072 samples per
  payload agreeing.
- `single_step.work_per_step`: measuring it needs the work clock read across a
  single-stepped guest and across the same guest running free. The spike single-step
  harness counts debug exits against an instruction oracle and never opens the work
  clock.

Box end state
Kernel 6.12.95+deb13-amd64, the stock Debian kernel the pack seals, booted from the
restored pristine /etc/default/grub with the default pinned to the stock entry by name.
The first attempt at that boot came up on the patched kernel again: Debian nests the
per-version entries in a submenu, so a saved default has to name the submenu and the entry
both. Pinned in the two-level form and re-booted, and grubenv now carries it. Posture re-applied
after the reboot and recorded: SMT off, nmi_watchdog 0, governor performance on all 16,
spec_store_bypass_disable=on, SpecLockMap disabled on every core, AVIC off, sampling
ceiling raised, md1 resync frozen. Nothing left running: no shards, no monitors, no
background writers. Evidence under /root/qual-evidence, mirrored into the worktree.

Box-hours
The program's own window on the box is about 7.7 hours, from the first posture record at
2026-08-25T01:50:01Z to the close. The box was provisioned before this program and is
not billed to it here.

Verdict
QUALIFIED as a work-clock and deadline-landing baseline, on the posture the pack seals
and with the overshoot contract stated below. NOT qualified for instruction-level
entropy denial, which this silicon cannot provide.

Qualified on:
- stage 0 passes three times with the same rows, and refuses a host that is not in the
  sealed posture. Refusal is demonstrated three ways, not asserted: on the patched kernel
  it names both KVM module identities; with the sampling ceiling dropped to its stock
  value it names the throttle; on a core the kernel schedules on it names the isolation.
  Each demonstration ends with the condition restored and check exiting 0 again.
- stage 1 passes: 36 of 36 checks, 1,250,000 overflow arms delivered exactly once with
  none lost, duplicated or premature, and 813,600,000 work-clock events judged against
  the analytical oracle with no mismatch.
- landing is exact under repetition: over the stage-2 campaign 99,345 distinct work
  counts were landed on, every one more than once and 94,625 of them from more than one
  of the eight cores, and over the supplement another 82,709 across fifteen cores. Not one
  work count in either run produced two different landed states. The state compared is the instruction pointer and the loop
  counter of the harness's payload, which is a narrow state - the payload has little
  else - so the claim this supports is the precise one: landing at a given work count
  always leaves the guest at the same instruction and the same iteration. It is not a
  claim about arbitrary guest state.
- the landing mechanism at volume: 1,719,938 landings at work == target across the two
  stage-2 runs, with the KVM_EXIT_PREEMPT deterministic exit attested in the record for
  1,138,949 of them. Item 7's floor is a million with attestation.
- overshoot is loud and recoverable: 62 events over 1,558,014 arms exposed to one, every
  one refused rather than accepted as a landing, and every one recovered by re-arming.

Conditions that travel with the qualification:
1. The sealed margin 16192 is twice the host-user maximum stage 1 measures. The
   distribution a guest deadline is landed in is heavier and its tail is unbounded; the
   largest this chip produced is 86738. A consumer must retry on SkidExceeded rather than
   treat it as fatal. Retry recovered 62 of 62 events here, so this is a contract the
   chip supports, not a hope.
2. The measurement core must be one the kernel keeps off. On an ordinary core the
   overshoot rate is 1 in 6,981 rather than 1 in 336,000, a 48-fold difference, with the
   bulk of the distribution unchanged. This is now a checked condition of the pack, so a
   host that does not meet it is refused rather than measured.
3. The backend arms with a hardcoded SKID_MARGIN of 256 and nothing reads the pack's
   margin. On this chip 256 is below the smallest skid ever observed, so the backend
   would refuse nearly every deadline. Software gap, named not fixed.
4. Guest RDRAND and RDSEED cannot be intercepted: SVM has no control for either.
   Clearing the CPUID bit does not stop the instruction.
5. RDTSC is interceptable on this chip and the determinism patch series does not wire
   it on this vendor.
6. Two pack fields are absent with reasons, both because the suite's stage 2 is unbuilt.
