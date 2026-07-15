# IMPLEMENTATION — task 110: paravirt work-derived clock, x86 (ABI v1)

Branch `task/paravirt-clock-x86`, bead `hm-rk5`, PR #110. Implements
`docs/PARAVIRT-CLOCK.md` per `tasks/110-paravirt-clock-x86.md`. **Portable
gates: all green** (full workspace, clippy on mac + x86_64-linux +
aarch64-NO_NEON cross-targets, fmt, deny, Miri on the new vmm-core paths,
public-api snapshots regenerated).

## Box gates — RUN AND GREEN (real patched KVM, det-cfl-v1 box, 2026-07-15)

All gates re-validated on the **registration-handshake** kernel (r8, bzImage
`e4591ea8`, head `03b91d2`), pinned `taskset -c 2`, patched KVM via
`box-window.sh`. The handshake reorder (the guest's post-doorbell RDTSC now
precedes its page-ABI validation) was **caught and fixed on the box**: without
it the guest logged `harmony_pvclock: ABI mismatch (page says 0) refusing the
page` and fell back to TSC (the host defers the first stamp to the handshake, so
a guest that validates before handshaking sees an unstamped page). With the fix
the page **materializes, registers, and is selected as the guest's active
clocksource** on real hardware:

```
clocksource: harmony-pvclock: mask 0xffff... max_idle_ns 881590591483
harmony_pvclock: work-derived clock page registered at 0x2dba000 (abi 1, 2 GHz)
clocksource: Switched to clocksource harmony-pvclock          ← selected, not just registered
```

| Gate | Result | Evidence |
|---|---|---|
| **G0** smoke-fire-once | **PASS** | page registered @0x2dba000, 4096 refreshes, monotonic to 99 ms vns, GUEST_READY, clean poweroff |
| **G1** same-seed bit-identical `state_hash`, page ON | **PASS** | 3/3 seals MATCH across two same-seed Postgres boots (page bytes hashed in full) — zero refresh/canonicalization entropy leaks into guest RAM; re-confirmed bit-identical on the r8 re-validation (final head, default schedule) |
| **N-4 perf** (kill condition 3) | **PASS (≈25×)** | RDTSC-exit-rate reduction over the actual Postgres workload (boot excluded) — see below; far above the 2× "worth it" threshold |
| **G2** page-stamp == RDTSC-trap-oracle at refresh Moments | **PASS** | 5 sampled boundary checks + terminal, 4096 refreshes, **deliberate-fault detected** (the gate can fail) |
| **G3** busy-wait liveness within Δ | **PASS** | syscall-free `pvclock-spin` completed; 2182 refreshes, **1921 (88%) forced by the Δ deadline**, max anchor gap = Δ exactly |
| **det-corpus O1** (run-vs-run determinism, page-off) | **PASS** (all payloads) | insn-rdtsc / insn-rng / insn-cpuid all deterministic same-seed on the patched backend — the page-off path my task must not regress is intact |
| **det-corpus O2** (conformance-to-golden) | insn-rdtsc ✓, insn-rng ✓, **insn-cpuid ✗ (pre-existing)** | the insn-cpuid digest is *deterministic* (identical run-to-run) but ≠ its committed golden — a CPUID conformance-golden drift (microcode/golden staleness). **My branch touches no golden/CPUID/corpus file** (diff is `consonance` + `docs` + `guest/linux` only), and pvclock touches no CPUID, so this is orthogonal to the page — flagged for the foreman to reconcile against main, NOT a pvclock regression |

**Kill condition 3 (perf) — PASSES: the x86 RDTSC-exit-rate reduction over the
actual Postgres workload is ≈25×, far above the 2× "worth it" threshold.**
Measured `PG37: workload begin` → `PG37: workload end` (boot excluded), page-off
vs page-on, on the r8 re-validation (final head):

```
pg workload page-OFF: rdtsc=1399 (242505/vsec)   total=8189   span=5.77M vns
pg workload page-ON:  rdtsc=  56 (  9718/vsec)   total=6846   span=5.76M vns
reduction = 24.95×    (workload begin→end, boot excluded)
```

**This corrects the first box pass's ≈1× number, which was a measurement
artifact, not the mechanism's behavior.** That pass measured *cumulative from VM
boot*, and boot is dominated by the kernel's pre-registration TSC calibration
(the harmony clocksource only takes over at `early_initcall`, after hundreds of
calibration RDTSCs). Those boot RDTSCs are identical page-on and page-off, so a
boot-inclusive window swamps the steady-state signal and reads ≈1×. Measuring
the workload itself (r8 P2d) shows the page removes **~96% of the workload's
RDTSC exits** (1399 → 56): once the clocksource is live, `clock_gettime` reads
the page instead of trapping. The residual 56 are the sources the page
deliberately doesn't touch — syscall-entry kstack-offset randomization
(`do_syscall_64`, allowlisted) and `delay_tsc` (kept on the trap by design) —
now shown to be a small tail, not the dominant term. The retained RDTSC trap
remains the backstop/oracle regardless, and the same page is the *only*
deterministic clock on ARM (AA-5, out of scope). (Boot-ratio arm, minimal image:
still ≈1× — boot is calibration-bound by construction and is not the
kill-condition workload.)

**Box-run gotchas (for reproduction):** the kernel has been re-pinned three times
— the STRICT_DEVMEM config fix (r7, → `051de137`), the r8 handshake RDTSC reorder
(→ `e4591ea8`), and the r11 exact-flags ABI check (→ **`e1c83dc3`**, the current
MANIFEST pin). Each time the two initramfs images reproduced bit-identically
(they are kernel-independent), confirming the build levers isolate the deliberate
kernel change; the r11 build additionally reproduced **bit-identically across the
box and the docs/BUILDING.md linux/amd64 container**, so the pin is toolchain-
stable. The Postgres image (`initramfs-postgres.cpio.gz`,
pin `3c4a7f2f…`) is short-lived (~0.46 virtual s: boots, runs its scripted
workload, powers off). Since **r8** the G1 and perf defaults are sized to fit it
(G1 seals at 0.1 s / 0.05 s / ×3; the perf measures the actual workload,
`PG37: workload begin` → `PG37: workload end`, boot excluded), so the **default**
`g1_`/`n4_perf_postgres` invocations run without any override. `PV_G1_*` and
`PV_PERF_WINDOW_VNS` remain env knobs for a longer-lived image. `INITRAMFS_EXEC_SHA256`
= `7f691432…` (G3 only). The first box pass (before r8) used explicit overrides
and a fixed cumulative window; the r8 re-validation reran G1 + the workload-
relative perf on the final head with defaults.

**Review round 1 folded in** (cross-model r1: 5 P1 + 1 P2; foreman stamping
ruling, flagged for Paul's veto): the anchor-derived stamping is RULED
accepted and `docs/PARAVIRT-CLOCK.md` §§1/2/3.1 are amended in this PR so doc
and code agree at ABI freeze; the natural-exit refresh now runs at **every**
exit tail (value-keyed — resolves the r1 natural-exit P1 under the same
ruling); the full channel configuration (offer + Δ + registration) is carried,
cross-validated symmetrically on restore, and folded into state identity as
the `PVCK` chunk; seals re-stamped the page canonically only after all
validation (reject-before-mutation) — **later superseded at r4: seals are
verbatim, see "The seal ruling"**; the opcode scan accounts per-function
instruction COUNTS; G3 re-arms the refresh log at its window and fails on
saturation. The
W^X/rescan-on-exec follow-up is bead **hm-rfz** (ruling item 3).

**Review round 2 folded in** (cross-model r2: 2 P1 + 1 P2 with foreman
dispositions): (a) the **overdue-first-deadline** P1 — the Δ forced refresh
arms only from a fresh anchor, so an armed pvclock target is always strictly
ahead of the guest and — since a `run_until`-bounded entry can never overshoot
its target — the overdue zero-step (whose report is a live PMU count) is
unreachable for pvclock deadlines. **SUPERSEDED BY r5**: r2 got the freshness by
*waiting* for the guest's next intercept (`first_advance_seen`), which froze the
page of a guest that registers and immediately busy-waits; r5 gets it by
*taking* a fresh work read at registration. The invariant is the same and now
holds from the doorbell onward. (b) the **opcode-gate** P1 — capture mode is now **fail-closed**
(the marker prints the baseline and FAILS the build), and the baseline was
produced portably in the documented linux/amd64 container, reviewed
entry-by-entry, and committed with the marker removed — the gate ships
ARMED (see below). (c) the **GPA** P2 — RULED (foreman, Paul-veto-flagged):
ABI v1 is the guest-registered **one-shot** GPA; re-registration is a guest
fault (`BadRequest`, touches nothing, the stamping target is pinned for the
machine's life); `docs/PARAVIRT-CLOCK.md` §1's "fixed, contract-reserved GPA"
wording is amended in this PR per the ruling.

**Review round 3 folded in** (cross-model r3: 5 P1 + 2 P2 with foreman
dispositions): (a) **licensing** — the clocksource ships as a **kernel diff**
(`guest/linux/patches/0001-x86-harmony-pvclock-work-derived-clocksource.patch`;
the standalone `.c` and the anchor-applier are gone; `patches/README.md`
states the GPL-2.0 kernel-diff exception and the regeneration workflow;
`build-kernel.sh` applies the diff with a reverse-dry-run idempotence guard).
(b) **per-service doorbell gating** — Event and Sdk require the SDK channel,
Entropy requires SDK-or-Net (its exact pre-pvclock reachability); an unoffered
service answers `UnknownService`, never a fake success, a fabricated buggify
answer, a seeded-stream draw, or a `Step::SdkStop` into a session with no SDK
channel (the PR-68 lesson). (c) **post-registration liveness** — the reference
guest executes a second deliberate `rdtsc` right after the doorbell exchange,
before selecting the page clocksource: a trapping intercept at a point where
the registration exists, so the Δ refresh is armed before any kernel path
reads page time. (d) **direct-restore carry** — the pvclock channel record
(offer + Δ + registration) rides the vm_state **device blob (v4)**: validated
symmetrically in the vendor's validate phase (reject-before-mutation) and
committed with the restore, so the public `save_vm_state`/`restore_snapshot`
path preserves same-state ⇒ same-future with **no control-server side
channel** (the control server's `pvclock_snaps` table is gone; a mismatched
factory now surfaces as the recoverable `RestoreFailed`, the LAPIC-mismatch
class; `Vmm::pvclock_restore` is removed from the public API). (e)
**MANIFEST** — regenerated via the container `run-tests.sh` (reproducibility
double-build + QEMU boot) and committed. (f) P2s: pvclock forced-refresh
`Deadline`s no longer pollute `preemption_landings` (recorded only when a
timer/arrival deadline was actually due), and the reference
`PvclockRegistrar` enforces the one-shot exactly like production
(`BadRequest` on any second register, ordered before the range check).

**Review round 4 folded in** (cross-model r4: 3 P1 + 1 P2). (a) **Seal-time
seqlock ABA** — a seal no longer canonicalizes the **live** page; the page is
sealed **verbatim**. See "The seal ruling" below: this is a deliberate
divergence from both `docs/PARAVIRT-CLOCK.md` §1.1 *and* the reviewer's
suggested fix (canonicalize the snapshot copy), with the doc amended in this
PR and the reasoning recorded — flagged for the foreman's and Paul's veto.
`save_vm_state` is **`&self` again** (it mutates nothing), which *removes* the
public-API change this PR previously carried. (b) **Device blob v3/v4** — the
version is now the offer flag: a VM that never called `enable_pvclock` encodes
the **v3 shape byte-for-byte** (no trailing record), so page-off blobs and the
`VMST` hashes over them are identical to main's and main's v3 blobs still
decode; only offered compositions encode v4. The task's "page off =
byte-identical" clause is now true at the wire, not merely in intent, and a
wire-level test pins it. (c) **G3 tick-refresh vacuity** — the 100 Hz guest tick
already forces a `Deadline` (hence a refresh) every ~10 ms, which is *also* the
default Δ, so `max_gap ≤ Δ` would have passed with the forced refresh deleted.
G3 now runs at **Δ = tick/10** (a bound the tick cannot meet) **and** asserts the
new `Vmm::pvclock_forced_landings()` — `Deadline` landings at which neither the
timer nor an arrival was due — **dominate** the window. Deleting
`pvclock_refresh_deadline` now fails G3 twice; a portable test proves a
timer-caused landing is **not** counted (otherwise the attribution would be as
vacuous as the bound). (d) **bzImage boot-artifact scan gap (P2)** — bzImage is
three executable artifacts (real-mode `setup`, `decompressor`, kernel) and all
three run; only `vmlinux` was scanned. All three are scanned now, with
**artifact-qualified** allowlist entries (`artifact:function count`) so the same
symbol in two artifacts cannot spend the other's budget (self-tested with a
cross-artifact-alias fixture). **Result: `setup` and `decompressor` contain zero
counter reads** — confirmed by symbol-attributed disassembly (4,846 and 5,988
instructions actually scanned, not an empty-file zero) and, because the
real-mode setup's 16-bit stream could decode differently under objdump, by a raw
byte search of both executable images for `0F 31` / `0F 01 F9` (zero of each).
Baseline re-captured from the same linux/amd64 container build; the armed scan
was verified green **and** verified to fail on a planted unlisted site and on a
stale `decompressor:` entry (proving the new artifacts are really in the
comparison, not silently skipped).

**Review round 5 folded in** (cross-model r5: 4 P1 + 2 P2; the box-gates P1 is
the task's own standing merge condition, recorded below, not a code fix).
(a) **Registration capability for pre-registration snapshots** — a snapshot
sealed *before* the guest registered carries no GPA, so the GPA check never ran
and the deterministic-backend requirement was skipped entirely. Restored onto a
backend with no deterministic counter, the guest's next `pvclock_register` —
the one the *source* accepted — answers `UnknownService`: same state, different
future. The v4 record now carries the source's **`registrable`** bit
(`Vmm::pvclock_available`) and restore requires **equality**, so the converse (a
child that can register where its parent never could) fails loud too. The old
test *asserted the bug* ("the same target accepts the UNREGISTERED channel
state"); it is now the regression pin. (b) **Deterministic first-arm at
registration** — a guest that registered and immediately busy-waited on the page
took none of the intercepts the Δ deadline was waiting for, so it was never
forced out and its page froze **forever**: the mechanism's headline case,
broken, and masked only by the reference kernel's courtesy `rdtsc`.
`pvclock_register` now anchors V-time from a **fresh work read** — the doorbell
`OUT` is a synchronous instruction trap, the same class as RDTSC, so the counter
is frozen at the instruction and the read is exact, not skid-laden — which arms
the deadline at registration and retires the r2 overdue hazard by an
**invariant** rather than a delay (every entry is `run_until`-bounded at or
before `anchor + Δ`, and a bounded entry cannot overshoot, so guest work is
always ≤ the armed target). `first_advance_seen` is gone. (c) **G3 was vacuous**
— its `date` shell loop syscalls, and this kernel's syscall entry reads the TSC
(kstack randomization; `do_syscall_64` is in the reviewed allowlist), so every
syscall was a V-time intercept refreshing the page *for free*: the loop would
terminate with the forced refresh deleted, and the constant intercepts could
even hold the attribution count at zero. G3 now runs
`guest/linux/pvclock-spin.c`, which mmaps the page through `/dev/mem` and spins
on seqlock reads with **no syscalls and no counter traps in the loop** — so the
only thing that can advance its clock is the host's Δ refresh. Freeze the page
and it hangs; that is the gate. (d) **LAPIC MMIO hole (P2)** — `map_memory`
splits the memslots around `[0xFEE00000, +0x1000)`, so a page-aligned GPA there
passed "inside guest RAM" while the guest's own loads went to the LAPIC device
model: registration would answer `Ok` and stamp backing the guest can never
read. Rejected now, behind a new `Vendor::mmio_holes()` seam (naming x86 MMIO is
vendor knowledge, not the engine's — ARCH-BOUNDARY). (e) **UnknownService before
classification (P2)** — a composition keeping the doorbell alive for another
channel graded pvclock requests (`BadRequest` / `UnknownOpcode`) *before* the
availability gate, leaking the service's existence; availability is checked
first now, per the generic dispatcher contract.

**Review round 6 folded in** (cross-model r6: 3 P1 + 3 P2, all edges of the r5
machinery). (a) **PVCK hashes the capability** — the fold carried Δ and the GPA
but not `pvclock_available()`, so two offered VMs with V-time wired but different
`deterministic_clock` backends hashed identically, though the next registration
succeeds on one and answers `UnknownService` on the other (the very future
difference `registrable` preserves in the restore record). PVCK now appends the
availability bit. (b) **Reject the impossible v4 tuple** — a crafted
`(delta, Some(gpa), registrable=false)` blob would pass the equality validator on
an offered-but-unavailable target and commit an *active* registration (next
refresh errors with no V-time; page freezes with no deterministic backend). A
registered page can only exist on a VM that could register, so the record is
rejected **at decode**, before the validator. (c) **G1 negotiates Hello** — the
live arm's first `Run` came back `Unsupported` (the server refuses every verb
until the handshake) and panicked before the gate reached its hash comparison;
Hello is sent first now. (d) **Registration restores `vtime_synchronized`** — a
`step()` clears it before entry, and registration anchors to the frozen
doorbell-`OUT` work count exactly like RDTSC but hadn't set it back, so a direct
caller that registered then snapshotted got a spurious `NotQuiescent`. Set now.
(e) **Perf window must complete** — the Postgres arm discarded `RunObs`, so a
step error / guest terminal / wall timeout before the window produced positive
*partial* counts that passed the sanity check as valid kill-condition evidence;
it now requires no step error and final V-time ≥ window. (f) **doc §3.1** — the
normative lines still said "the doorbell `OUT` is not a V-time intercept, so the
first value may lag", contradicting the r5 fix they forced; amended to the
immediate fresh-anchor arm rule.

**ABI coordination (ruled on PR #108 r9, folded into r3):** ABI-v1 `flags`
bit 1 = `WORK_DERIVED` — set by every real stamp (`vtime::pvclock` publishes
`MATERIALIZED | WORK_DERIVED`; canonical re-stamps included; remaining bits
reserved-zero), verified by `pvclock_check_oracle`, and amended into
`docs/PARAVIRT-CLOCK.md` §1. The ARM spike's static placeholder page
deliberately leaves the bit clear, so AA-5 fails closed against a page
nothing is actually deriving. The guest kernel's `MATERIALIZED` check is
unaffected (bit 0 unchanged) — the committed kernel image and MANIFEST stay
valid.

**Review round 8 folded in — the REGISTRATION HANDSHAKE ruling** (cross-model r8,
foreman-ruled; supersedes the r5/r6/r7 registration-anchor iterations). The
tension across r5–r7 was: arm the Δ refresh at the doorbell `OUT` and you either
read a skid-tainted PIO counter (r5/r6) or arm off a possibly-stale anchor whose
overdue deadline imports a live count (r7/first-r8); defer the arm and a
register-then-spin guest freezes (the r5 concern). The ruling resolves it:
**the doorbell `OUT` + the guest's REQUIRED post-doorbell RDTSC together are the
registration handshake.** The `OUT` records a *pending* registration; the first
stamp and the Δ arm happen only at the handshake intercept (a genuine skid-free
RDTSC), off a fresh anchor that can never be born overdue. A guest that omits the
handshake is explicitly **out of contract** (stale-but-deterministic page, no
refresh) — so the host never arms off a skid or stale anchor, and liveness for a
conforming guest is preserved. Host side: `PvclockChannel::armed`, pending at
`OUT`, promote+stamp+arm at the first `vtime_synchronized` step-tail; restore
arms immediately (anchor 0 is synchronized). The r8 `on_deadline`
deterministic-target change is **kept** — it makes "`vtime_synchronized` ⇒
deterministic anchor" hold universally, which the handshake stamp relies on.
Guest side: the kernel patch's post-doorbell RDTSC is now **protocol**, and — the
part the **box caught** — it had to be **reordered before the kernel's page-ABI
validation** (the validation reads the page's `abi_version`/`flags`, which the
host writes only at the handshake; validating first saw an unstamped page and
refused it, falling back to TSC). §3.1 rewritten to the two-step handshake. The
reorder changed the bzImage (`051de137` → `e4591ea8`, MANIFEST re-pinned); both
initramfs images reproduced bit-identically. Re-validated on the box: G0–G3 +
Postgres perf all green, clocksource correctly `harmony-pvclock` (see the box
gates section above).

**Review round 9 folded in — final polish** (cross-model r9: 2 P1 + 2 P2).
(a) **Overdue snapshot exactness** — `on_deadline` now marks V-time synchronized
only when `anchor == reached` (the guest actually stopped AT the deterministic
target), not on an overdue zero-entry landing where the live counter sits past
it; the latter would let a snapshot restore inexactly (the restored counter
re-baselines from the anchor while the uninterrupted VM's sits at `reached`).
(b) **Scanner fail-closed** — `raw_byte_scan_one` runs under `if ! …` (errexit
off), so a swallowed `objcopy`/`od`/`tr` failure could green the gate without
reading a section; the extraction pipeline's status is now checked explicitly.
(c) **X-flag section selection** — the raw scan selects any PROGBITS section
whose flags contain `X` (an exact `AX` token missed combined flags like `WAX`);
self-test gains a `WAX` fixture. (d) **Public contract** — `hypercall-proto`
(the `Pvclock` service + `pvclock_register` docs) and `docs/INTEGRATION.md` now
describe the two-step handshake (the response records a *pending* registration
and does not stamp the page; the guest's required post-response RDTSC is the
stamping intercept). No kernel change; confirming box G1 on the r9 head is
bit-identical to the handshake-kernel run (the overdue-not-synchronized edge
never fires for a conforming guest).

**Review round 10 folded in** (cross-model r10: 2 P1). (a) **Page-off probe
path** — the doorbell port was serviced only when a channel was wired, so a
clock-aware guest probing service 7 on a channel-less VM hit a fatal
`ContractViolation` instead of the protocol's `UnknownService`. The doorbell port
is a *modeled* device, so it now always reaches the default-deny dispatcher,
which answers `UnknownService` for an unoffered service (byte-identical for
M1/M2/corpus/stock — they never write the port). (b) **Site-identified
allowlist** — the opcode-scan allowlist recorded per-function *counts*, which
miss a removed+added pair; each entry is now a per-site `symbol+0xOFFSET`, so a
new/removed/moved read is caught. Verified on the box: armed scan passes and
fails on a dropped or moved site.

**Review round 11 folded in — APPROVE round** (cross-model r11: 1 P1 + 2 P2).
(a) **Exact flags word** — the guest kernel's ABI check tested only bit 0
(`MATERIALIZED`); it now requires the exact `HARMONY_FLAGS_V1 = MATERIALIZED |
WORK_DERIVED` word with reserved bits zero, so a placeholder page (the ARM
spike's static page leaves `WORK_DERIVED` clear) fails closed. A real kernel
change — bzImage re-pinned `e4591ea8` → **`e1c83dc3`**; the **box and container
builds produced the identical binary** (reproducible), both passed the per-site
scan (the flags-check change is after the rdtsc sites, so no offset shifted), and
box G0+G1 re-confirmed (clocksource still selects `harmony-pvclock`, 3/3 seals
bit-identical). (b) **PVCK folds `armed`** — the handshake bit is in state
identity now (pending vs armed have different futures). (c) **§3.3 per-vendor
split** — clarified that a reviewed reachable `rdtsc` is contract-safe on x86 via
the retained trap backstop (the gate is an allowlist), while the strict
zero-reachable (empty allowlist) bar is the ARM no-trap story.

**Review round 12 folded in** (cross-model r12: 1 P1 + 1 P2, portable-only — no
kernel rebuild). (a) **`restore_vtime` re-stamps the armed page (P1).** A
*V-time-only* `restore_vtime` (the standalone restore and the `resume_idle`
epoch-rebase) rebases the timeline but — unlike a full `restore_vm_state`, whose
page bytes ride the restored RAM image — never touches the live RAM page. So an
**armed** registration's page kept its PRE-restore stamp, a value from the old
timeline that can sit *ahead* of the just-restored effective V-time. Two hazards:
the guest's next entry would read the stale-ahead value and then watch the
step-tail refresh drop it (a backward `vns` jump the seqlock cannot mask), and
two VMs rewound from different timelines to the *same* snapshot would hold
different page bytes → a `state_hash` fork. Fix: at the tail of `restore_vtime`,
if the registration is armed, re-stamp the page to **canonical** form (seq 0) at
the restored anchor (exactly the handshake's first-stamp posture) — every later
refresh only moves it forward. A *pending* (un-armed) registration is left alone
(its first stamp still belongs to the handshake intercept). The re-stamp is the
sole fallible step after the commit but is a *mechanism* step, not `snap`
validation — it can only fail on a host-side stamping bug (fail-closed), never on
untrusted input, so the reject-a-bad-snapshot-before-mutation atomicity is
intact. Tests: `restore_vtime_restamps_the_armed_page_to_the_restored_timeline`
(backward-jump + byte-identical-across-two-VMs) and
`restore_vtime_leaves_a_pending_registration_unstamped`. (b) **Registrar restore
re-validates the GPA (P2).** `PvclockRegistrar::restore_state` (hypercall-proto's
reference registrar) decoded the registered GPA from the state blob *without*
re-checking it, while `handle` enforces 4 KiB alignment + RAM containment. A
malformed/hostile blob could therefore restore a registration `handle` would
reject (an unaligned or out-of-RAM GPA that later stamps outside the page
window). Fix: both paths now route through one `gpa_fits(gpa, ram_len)` helper —
`restore_state` re-runs it against the blob's own `ram_len` and returns
`BadState` on failure (before any commit — no partial restore). Test:
`pvclock_registrar_restore_revalidates_the_gpa`.

**Review round 13 folded in** (cross-model r13: 2 P1 + 1 P2, all fallout of the
r12 restore fix; portable-only — no kernel rebuild). (a) **The re-stamp uses the
epoch-advancing REFRESH, not canonical `seq = 0` (P1).** r12 re-stamped the
armed page with `StampKind::Canonical`, which resets `seq` to 0 — and the page
is **live**. A guest reader that sampled `seq = 0`, then took a V-time exit
mid-read (e.g. an interrupt during its two `seq` loads) and resumed *after* the
restore, would find `seq = 0` again and accept its pre-restore `vns` instead of
retrying: the exact ABA the seal ruling (r4) forbids and the `stamp_canonical`
module doc calls out as "snapshot copies only." Fixed to `StampKind::Refresh`,
which republishes the restored value **and advances the epoch on the distinct
value**, so a straddling reader sees the change and retries. This also corrects
the r12 note above: the re-stamp is *not* canonical seq-0, and the "byte-
identical across two VMs" property r12 claimed was wrong — that determinism story
belongs to `restore_vm_state` (verbatim page bytes ride the RAM image; a sibling
fork inherits the parent's exact epoch), never to the live-page V-time-only path.
The test now asserts the epoch **advances** (ABA-safety) rather than resets.
(b) **A pending registration is unsealable (P1).** `restore_vtime` can leave a
registration pending (`armed == false`) yet mark V-time synchronized, so
`save_vm_state` could legally seal it — but the v4 device record carries the GPA,
not the pending-vs-armed bit, and `pvclock_commit_restore` arms every carried
GPA. A restored child would therefore come up armed (normal refresh, Δ deadline
live) while the source still owes its canonical handshake stamp: different bytes,
different future. Chose **reject over serialize** (the reviewer offered both): a
pending seal now fails closed alongside `check_sealable_vcpu` — no blob-ABI bump,
consistent with the existing "define out the hard boundary" seal guards (a
pending registration is never at a synchronized boundary in normal operation; the
`OUT` is a PIO and the first synchronized point after it is the arming
handshake). `pvclock_commit_restore`'s "arm every carried GPA" is now provably
faithful. Tests: `save_vm_state_rejects_a_pending_pvclock_registration`; the
`branch_into_an_unoffered_composition_fails_loud` control test was snapshotting in
the pending window and now completes the handshake first (matching the real
doorbell→RDTSC order). (c) **G0 smoke asserts the clocksource switch (P2).** The
box G0 gate logged but did not *assert* the `Switched to clocksource
harmony-pvclock` line, so a guest that registered+stamped the page but kept
timekeeping on the TSC (page registered-but-unused) would pass every executable
assertion while the perf lever's RDTSC exits never left the hot path. Now
asserted — a TSC-still-active guest fails G0 loudly. (The switch line was already
present in every green box run, IMPLEMENTATION §Box gates line "Switched to
clocksource harmony-pvclock", so this tightens the gate without needing a new
box run; box re-confirmation rides the next foreman-granted window.)

**Review round 14 folded in — APPROVE round** (cross-model r14: 1 P1 + 1 P2;
portable-only — no kernel rebuild). (a) **The page-on campaign composition knob
(P1, deliverable 7's letter).** The box Postgres campaign (`campaign-runner box`,
the real-Postgres sweep) had no way to actually run **page-on**, so deliverable
7's "postgres campaign smoke … page-off vs page-on" wasn't genuinely runnable
from the composition root. Added a `--page-on` knob that wires BOTH halves of the
composition from one flag — the guest cmdline (` harmony_pvclock` appended) and
the host offer (`enable_pvclock`) — for the live VM AND every forked branch
(same-state ⇒ same-future: a branch must offer the page too, else the guest's
registration answers `UnknownService` where the source accepted it). The cmdline
half is a portable `pvclock_cmdline(base, page_on)` helper (unit-tested); the host
`enable_pvclock` half lives in the Linux-only `boxrun.rs` composition root next to
it, mirroring the `live_pvclock.rs` "one body, one knob" discipline so the arms
can't drift. Off by default — a bare `box` run is byte-for-byte today's page-off
boot. Tests: `pvclock_cmdline_appends_the_clocksource_only_when_page_on`,
`box_mode_parses_the_page_on_knob` (both portable). (b) **Deadline-cause
classification anchors to the target, not the skidded count (P2).** `on_deadline`
classified a landing's cause (timer / arrival / pvclock-Δ) by comparing the
armed deadlines against `reached` — but on an **overdue** real-backend landing
`reached` is a live PMU read that skids PAST the deterministic target, so a
deadline sitting between `target` and `reached` (one the guest never actually
stopped for) tested as "due" and misattributed the landing — e.g. crediting a
timer/arrival the skid crossed and stealing a genuine Δ-forced pvclock refresh,
corrupting G3's attribution evidence exactly on the overdue path it exists to
measure. Now classified against `target.unwrap_or(reached)` (the same
deterministic point the anchor already uses; `reached` only when a `Deadline`
arrives targetless, a backend contract violation). The MEASURED-landing trace
still records `reached` — that IS the seed-dependence signal; only the cause
classification uses the deterministic point. The mock rewrites `reached :=
deadline`, so the overdue path is driven by a direct `on_deadline(reached >
target)` call. Test: `overdue_landing_classifies_against_the_target_not_the_skidded_count`.

**Review round 15 folded in** (cross-model r15: 2 P1 + 2 P2, all on the r14
campaign knob; portable-only — no kernel rebuild). (a) **`--page-on` must reach
the page-on composition before reporting (P1).** A stale/non-pvclock image, a
failed registration, or a TSC-still-active guest could sail to the readiness
marker, so `campaign-runner box --page-on` could report a determinism/throughput
result for an effectively page-OFF run. `boot_server` now fails closed unless BOTH
`pvclock_registration().is_some()` AND the guest console shows `Switched to
clocksource harmony-pvclock` (the same registration+selection bar G0 asserts).
Portable helper `require_page_on_active` (tested). (b) **Campaign-throughput A/B is
producible (P1).** The sweep discarded its timing and `SweepReport` carried no
throughput, so deliverable 7's page-off-vs-page-on campaign-throughput ppm
comparison wasn't producible. The sweep is now wall-timed (the task-96 print-only
stopwatch — never touches state/hash), each arm prints a page-{on,off}-labelled
`[REPORT] … branches/s` line from the report's actual branch count, and the A/B
ratio is a tested render (`render_throughput_ab` / `throughput_ab_ratio`, degenerate
denominators defined out). Tests: `campaign_throughput_and_ab_ratio_are_producible`.
(c) **Perf denominators sampled at synchronized boundaries (P2).** The N-4 perf
arms sampled the exit counters and V-time at a workload marker — a serial PIO exit
where `effective_vns()` is only a last-intercept lower bound — and the arms take
different intercept mixes (page-off takes far more RDTSC intercepts), so the bound
biased the reported rate/reduction unequally. `perf_arm` now captures the last
SYNCHRONIZED sample of its whole-boot run, and the Postgres workload window
baselines/ends via a new `sample_at_sync` (advance to the next synchronized
intercept, where `effective_vns` is exact and the counters agree). (d) **G3 rejects
a backward clock (P2).** `pvclock-spin.c` computed `vns1 - vns0` unsigned, so a
backward page wrapped to a huge value and the spinner emitted a false
`PVSPIN_DONE`; it now detects `vns1 < vns0` explicitly and emits `PVSPIN_FAIL
backward` (which the host G3 gate, keyed on `PVSPIN_DONE`, surfaces as a failure).
The host G3 also now asserts published-vns monotonicity across the spin window, so
the work-anchor `saturating_sub` can't mask a backward step. (c)/(d) are box/guest
code, compile-checked on the x86_64-linux cross target and `cc -fsyntax-only`; box
re-confirmation of the perf/G3 gates rides the next foreman-granted window.

## What landed (by deliverable)

1. **Rename ride-along** — already fully landed by tasks/108 (`guest_hz`/
   `guest_base`/`guest_ticks`, `VtimeState` mirror included). Per the spec's
   reconcile-with-main instruction those names stand; this task only swept the
   comment residue that still said `VClock::tsc`/`visible_tsc` (commit 1). The
   §5 table's `guest_clock_hz`/`guest_clock()` spellings were NOT re-renamed
   onto main's names — "do not re-rename what 108 already landed" — and the
   §1 page *field* names (`guest_clock`, `guest_clock_hz`) are ABI names in
   `vtime::pvclock`, independent of the Rust API names.
2. **Page + refresh discipline** — `vtime::pvclock` (arch-blind stamping:
   seqlock write protocol, value-keyed idempotence, §1.1 canonical form; unit +
   property tests) and the vmm-core `PvclockChannel` (registration state, the
   refresh at the tail of **every** serviced exit — value-keyed, so the page
   bytes move exactly at the deterministic clock-advance boundaries — and the
   Δ forced refresh folded into `run_until_deadline`). Δ is
   `enable_pvclock(delta_work)` with documented default
   `PVCLOCK_DEFAULT_DELTA_WORK = 10_000_000` counted branches ≈ 10 ms V-time
   under the contract clock.
3. **Registration transport = the hypercall doorbell** (`ServiceId::Pvclock = 7`,
   op 1, 8-byte LE GPA → 4-byte LE ABI version). Why the doorbell and not a
   contract-reserved MSR: (a) zero contract change — a new MSR row would
   change `contract_hash`, invalidating every sealed blob and touching the
   frozen MSR policy, while a new doorbell service id is additive on an
   already-released wire ABI; (b) the doorbell is the already-modeled seam
   with existing validation/framing/versioning discipline (INTEGRATION.md §1)
   and reaches any future `/dev/harmony` transport unchanged; (c) it is
   arch-portable — an ARM vendor rings the same frame ABI without inventing an
   MSR analogue. The **host→guest offer** is advertised by appending the
   `harmony_pvclock` kernel parameter to the cmdline (the host owns the
   cmdline), so a guest never rings the doorbell at a host that would
   default-deny it; the guest half degrades cleanly on any non-Ok status.
   GPA validation: page-aligned, wholly inside RAM, not a doorbell frame page;
   accepted only on the determinism-complete path (else `UnknownService`).
4. **Seal capture — VERBATIM, not canonical (r4; see "The seal ruling")**. The
   deliverable as specced was a canonical seal re-stamp; it is **not
   implementable safely** (the live-page ABA) and its copy-only variant breaks
   the snapshot engine's image == live-RAM contract, so the page is sealed
   exactly as the guest sees it and history-freedom comes from value-keyed
   stamping instead. `save_vm_state` therefore mutates nothing and is `&self`
   (main's signature — this PR no longer changes it). No new vm-state section,
   no `VM_STATE_VERSION` bump. The full channel configuration
   (offer + Δ + registration) rides the sealed vm_state's **device blob (v4 when
   offered, v3 byte-identical to main when not — r4)** — validated symmetrically
   in the restore's validate phase
   (offer/Δ/GPA/deterministic-backend mismatches all fail loud, before any
   mutation) and committed with the restore, so the direct
   `save_vm_state`/`restore_snapshot` path preserves same-state ⇒ same-future
   with no control-server side channel; the configuration also folds into
   `state_blob` as the `PVCK` chunk when offered (state identity — the SDK
   fault-policy precedent; un-offered blobs are byte-for-byte unchanged).
5. **Guest kernel clocksource** — the kernel's first source change, shipped
   as a **kernel diff** (r3 licensing form):
   `guest/linux/patches/0001-x86-harmony-pvclock-work-derived-clocksource.patch`
   (new file + Kconfig + Makefile hunks generated against the pristine
   pinned tree; `patches/README.md` states the GPL-2.0 kernel-diff exception
   and the regeneration workflow; applied by `build-kernel.sh` with a
   reverse-dry-run idempotence guard), `CONFIG_HARMONY_PVCLOCK=y` in
   `config-fragment` + `assert_y`. Clocksource `.read()` = the §1 seqlock
   page load (vns, ns-native, registered at 1 GHz, rating 450); sched_clock
   routed through the same read (`paravirt_set_sched_clock`);
   `mark_tsc_unstable` makes the TSC unselectable for timekeeping once the
   page is live. The registration is still bracketed by two deliberate `rdtsc`
   traps, but since r5 **nothing depends on them**: the VMM anchors V-time from
   a fresh work read inside `pvclock_register` itself and arms the Δ deadline
   there, so a guest that registers and immediately busy-waits is forced out on
   the host's own guarantee rather than on the reference kernel's courtesy. The
   traps stay as belt-and-braces (and are allowlisted). Runtime-gated on the
   `harmony_pvclock` parameter → one image is both measurement arms.
   **Compiles and boots proven portably**: the full linux/amd64 container
   build produces bzImage and passes the reproducibility + QEMU-boot gates
   (`run-tests.sh`), whose regenerated `MANIFEST.sha256` is committed.
6. **Reachability gate, x86 half** — `guest/linux/scan-counter-opcodes.sh`
   wired into `build-kernel.sh`: symbol-attributed objdump scan for
   rdtsc/rdtscp vs the reviewed `rdtsc-allowlist.txt`, accounting **per
   function AND per instruction count** (a new read inside an
   already-reviewed function moves its count — the r1 fix), exact in both
   directions, self-testing its ability to fail (planted-new,
   planted-inside-allowlisted, stale-entry, bare-entry fixtures) on every
   invocation. **Ships ARMED** (r2): the reviewed baseline was captured from
   a real 6.18.35 build in the documented linux/amd64 container and
   committed; the `GATE-UNARMED` re-baselining marker, when present, prints
   the captured baseline and **fails the build** (fail-closed — a disarmed
   gate never passes anything). **W^X/rescan-on-exec runtime half: specced
   and stubbed, accepted as such by the PR ruling — follow-up bead
   `hm-rfz`**; `CONFIG_MODULES` asserted off means no loadable code escapes
   the static scan meanwhile.
7. **Gates** — portable G1/G2/G3 analogues + mandated deliberate-fault tests
   in `consonance/vmm-core/src/vmm.rs` (incl. the r1 additions: natural-exit
   repair, symmetric restore-mismatch matrix, `PVCK` state identity,
   rejected-seal atomicity) and control-server carry tests in `control.rs`;
   live G0(smoke)/G1/G2/G3/perf in
   `consonance/vmm-core/tests/live_pvclock.rs` (runnable-from-the-repo;
   Environment section in the file header).

## The seal ruling (r4 — diverges from the doc AND from the reviewer's fix; flagged for veto)

`docs/PARAVIRT-CLOCK.md` §1.1 ruled that **at every seal the page is re-stamped
to canonical form** (`seq = 0`, zeroed tail), and justified it as safe because
"the guest only reads the page while running, never at the seal boundary (a seal
is taken at an HLT quiescent Moment)". **That premise has been false since task
41**: a seal is taken at *any* V-time-synchronized intercept, so a guest reader
**can** be mid-seqlock-read across one. Resetting a live `seq` to a fixed epoch
is then an **ABA** — a reader that sampled `seq = 0`, took an exit before its
validating re-read, and resumed after a refresh-then-canonicalize sees `seq = 0`
again, accepts the values it loaded *before* the refresh, and misses it.
**Taking a snapshot would change the guest's future.** The doc's ruling, read
literally, is unimplementable; §1.1 is amended in this PR with the reasoning.

The reviewer's suggested fix — **canonicalize the snapshot copy** — is
*also* rejected, for a reason the review could not see from the diff alone:

- `ControlSession::seal_into_store` captures **live** `vmm.guest_memory()`, and
  `snapshot_derive(parent, live_ram, dirty_gfns)` diffs live RAM against the
  **parent image**. `control.rs`'s
  `seal_derives_from_tracked_parent_and_reproduces_the_image` asserts, in as many
  words, that the sealed image **reproduces live guest memory**.
- A copy-only canonicalization breaks exactly that: the image would carry
  `seq = 0` while live RAM carried `seq = K`. A parent that seals and **keeps
  running** (the branching model — seal a Moment, spawn children, continue) would
  then diverge, by exactly its `seq`, from a child restored from its own
  snapshot — forever. That is a `same-state ⇒ same-future` break, and it would
  land in `state_hash` (the page is hashed guest RAM).

So the page is **sealed verbatim** — no seal touches guest RAM at all. What
replaces canonicalization is the **value-keyed stamping already ruled at r1**: a
stamp that publishes values the page already carries writes *nothing* and does
not move the epoch, so `seq` advances only on **distinct-value** publications,
whose stream is a pure function of the deterministic execution. The epoch is
therefore reproducible **by construction**, a restored run inherits its parent's
epoch and continues in lockstep, and the sealed image stays a faithful copy of
the machine. The two fragilities §1.1 cited as its reason for not leaning on
this are both closed by other rulings **in this same PR**: **skid** cannot reach
the values (stamps use the skid-free `last_intercept_work` anchor — r1), and
**Δ** is machine configuration carried in the sealed device blob and
cross-validated on restore (r3) — a Δ mismatch is *rejected*, never silently
divergent. Canonical form survives as the **registration** form (a fresh page,
no reader possible, no prior epoch to alias), which is what gives the channel a
known starting epoch and a zeroed tail whatever the guest's allocator left there.

Pinned by tests in both crates: `canonical_reset_would_be_an_aba_on_a_live_page`
(the hazard itself), `a_verbatim_sealed_page_keeps_restored_and_continued_runs_in_lockstep`,
`pvclock_seal_never_touches_the_live_page`, and
`pvclock_seal_is_verbatim_and_restore_carries_the_registration` (which now also
asserts image == live RAM).

## The stamping ruling (RULED at review round 1; flagged for Paul's veto)

`docs/PARAVIRT-CLOCK.md` §2.1 originally said the vmm stamps with "the current
work count from `CpuBackend::work()`" at every natural exit. Implemented — and
now **ruled accepted** (foreman, PR #110 round 1) — **as the skid-free anchor
(`last_intercept_work`)**, for the codebase's own reason (task-27 O1 evidence,
the `VtimeWiring` doc): a live counter read at a non-intercept boundary
carries non-deterministic exit-path skid, and the page is **hashed guest
RAM** — the literal reading contradicted §2's own determinism argument. The
anchor is exactly the value the RDTSC-trap oracle returns, so G2's
function-equality holds by construction. The refresh **runs at every exit
tail** (the r1 natural-exit P1, resolved under the same ruling): between
clock advances the anchor cannot move, so those stamps are value-keyed byte
no-ops, and the published value stream advances exactly at the deterministic
boundaries — intercepts, deadline landings (pre-injection ordering — kill
condition 1), idle warps, and the Δ forced refresh (which is what keeps the
anchor fresh once the page removes the dense RDTSC traps that keep it fresh
today). Per the ruling, `docs/PARAVIRT-CLOCK.md` §2.1 is **amended in this
PR** to the anchor formulation, and the two smaller reconciliations are
recorded where they bind: §1 (the `guest_clock` field is the guest-visible,
offset-adjusted clock — what the trap returns) and §3.1 (the registration
stamp publishes the current anchor; the doorbell `OUT` is not an intercept).
Doc and code agree at ABI freeze.

## Deviations considered and rejected

- **`seq = 0` always** (no epoch bumps): rejected — a reader straddling an
  exit/resume boundary could accept a torn (old-field, new-field) pair; the
  epoch is what forces its retry. Epochs are deterministic anyway (value-keyed
  stamping ⇒ pure function of the distinct-value stream), which is exactly what
  lets the seal preserve them rather than erase them (r4).
- **Host-side "last stamped" cache** for the skip-if-unchanged: rejected in
  favor of reading the page itself — the page IS the cache, which makes
  restored-vs-continued runs align by construction (no cache to reconcile).
- **A contract-reserved MSR transport**: rejected (see deliverable 3).
- **CPUID-probe for the offer** (kvmclock-style feature leaf): rejected — the
  CPUID model is frozen under `contract_hash`; the cmdline advertisement costs
  nothing and the host owns the cmdline anyway.
- **Routing guest delay paths through the page**: rejected — udelay spinning
  on a piecewise-constant clock with Δ ≈ 10 ms resolution would overshoot
  microsecond delays by orders of magnitude. `delay_tsc` stays on the RDTSC
  trap (correct, oracle-equal, just slower); 6.18 exports no loop-delay
  override seam. Revisit only if the §6 numbers say delay traps dominate.
- **A new vm-state section for the registration GPA**: rejected — the
  SdkSnapshot precedent (control-server carry) covers it without a
  `VM_STATE_VERSION` bump, exactly as deliverable 4 demands for the page.
- **Kani proofs for `vtime::pvclock`**: considered; the module is safe slice
  code with total bounds checks and 256-case property tests — no panicking
  arithmetic to prove. Not added.

## Known limitations / box-verify items

- **The kernel patch compiles and QEMU-boots, proven portably** (r2/r3: the
  full linux/amd64 container build + `run-tests.sh`); what remains
  box-verified is the LIVE half — the doorbell registration against the real
  patched-KVM host (memremap of the X86_RESERVE_LOW-reserved doorbell pages
  at early_initcall, page clocksource selection, G0–G3/perf).
- **The rdtsc allowlist baseline is container-captured** (r2; re-captured at r4
  as `artifact:function count` across all three boot artifacts): reviewed and
  committed from the linux/amd64 container build (debian:stable gcc). The
  box's compiler version may inline differently and shift a per-function
  count — if the first box build fails the armed scan with a count drift,
  that is the exact-accounting design working: re-review the drifted entries
  against the box capture and commit the delta (the `GATE-UNARMED` marker
  exists for full re-baselines, e.g. a kernel version bump, and FAILS builds
  while present). The **setup/decompressor entries are empty on purpose** —
  those artifacts contain no counter reads today, and the stale/unlisted checks
  run in both directions, so the first one to appear fails the build.
- **G3 is deliberately unlike the other gates, in two ways, and both are
  load-bearing anti-vacuity measures** — do not "simplify" either back.
  1. **A non-default Δ** (r4): `PV_G3_DELTA_WORK`, default `tick/10` ≈ 1 ms. At
     the *default* Δ (≈ 10 ms) the guest's own 100 Hz tick refreshes the page
     often enough to satisfy `max_gap ≤ Δ` with the forced-refresh mechanism
     deleted. Every other gate (G1/G2/perf) still runs the documented default Δ,
     which is what the kill-condition-3 perf numbers must be judged at.
  2. **A syscall-free guest** (r5): `guest/linux/pvclock-spin.c`, not a shell
     `date` loop. This kernel's syscall entry reads the TSC (kstack
     randomization — `do_syscall_64` is in the allowlist), so a shell loop's
     every `date` was a V-time intercept that refreshed the page *for free*. The
     spinner mmaps the page via `/dev/mem` and reads it directly, so the Δ
     refresh is the only thing that can advance its clock. It reports
     `PVSPIN_DONE`, which the harness asserts — an exit status alone cannot tell
     a real completion from a shell error.
  Together with `pvclock_forced_landings` (the attribution count), deleting
  `pvclock_refresh_deadline` now fails G3 three different ways.
- **MANIFEST.sha256 is regenerated and committed** (r3): the container
  `run-tests.sh` run (reproducibility double-build + QEMU boot of the
  manifested bytes) produced it against the pvclock kernel, so the live gates
  pin against a committed hash out of the box. The box's own build should
  reproduce it bit-for-bit (the levers pin timestamp/user/host/path); a
  toolchain-version difference would surface as a pin mismatch — rebuild and
  re-commit from the box in that case (deliberate `*_SHA256` env overrides
  exist for the window itself).
- **`pvclock_refreshes` is capped** at `PVCLOCK_REFRESH_TRACE_CAP` (4096, the
  landing-trace cap); windowed gates re-arm it via `pvclock_clear_refreshes`
  and treat a saturated window as a measurement failure (the r1 G3 fix).
- **Full-suite Miri for vmm-core** runs in the nightly job as before; the new
  pvclock tests were run under Miri here (11/11 clean, restore-path tests
  Miri-ignored with reasons, matching the crate's convention).
- **Public API** (snapshot regenerated on Linux, the box — the vmm-core
  `public_api` test is Linux-frozen and skips on the Mac): the *only* additions
  are the pvclock accessors, now including `pvclock_forced_landings()` (G3's
  attribution evidence, r4). `save_vm_state` is **`&self`, unchanged from main**
  — the r4 verbatim-seal ruling removed the `&mut self` this PR briefly carried.
- **The page's `seq` is history-dependent by design (r4)** — it counts
  distinct-value publications since registration. That is reproducible for any
  same-seed run and across restore (the child inherits the parent's epoch), but
  it means two *convergent* states reached by different paths need not have
  byte-identical pages. Nothing in-tree depends on that (state dedup keys on the
  whole memory image, which would differ anyway); noted because §1.1's original
  canonicalization was partly aimed at it.

## Box runbook (the foreman-granted window)

All from the repo on the box, pinned per `docs/BOX-PINNING.md`:

1. `make -C guest fetch && make -C guest/linux kernel`
   → expected green against the committed baseline AND the committed
   `MANIFEST.sha256` (both produced portably in the container). If the box
   toolchain differs: an inlined-count drift fails the armed scan
   (re-review + commit the delta) and a byte drift fails the manifest pin
   (rebuild + re-commit from the box) — both loud, neither silent.
   (`make -C guest/linux exec-image` for G3; note its sha256 for
   `INITRAMFS_EXEC_SHA256`. Since r5 that image also carries the static
   `/bin/pvclock-spin` and a `/dev/mem` node — G3's syscall-free busy-wait — so
   it MUST be rebuilt, not reused from an earlier window. Verified portably: the
   image builds in the linux/amd64 container and unpacks with the spinner
   present, static, and executing.)
2. **Smoke-fire-once** (minutes, before any budget):
   `taskset -c 2 cargo test -p vmm-core --release --test live_pvclock -- --ignored g0_smoke --test-threads=1`
   Report before continuing (per the task's box discipline).
3. G1, G2, G3, perf (in that order; G3 needs `INITRAMFS_EXEC_SHA256`):
   same invocation with `g1_`/`g2_`/`g3_`/`n4_perf_` filters. Kill condition 3
   is judged from the `[REPORT]` ratio lines — the perf tests never assert it.
4. det-corpus + campaign smoke (the remaining §6 items): `box_corpus.rs` as
   usual (M1/M2 payloads never touch the page — expect unchanged), and a
   short campaign-runner Postgres smoke with the page-on kernel via the
   existing campaign tooling, throughput reported ppm-style vs a page-off run.

## Integrator notes

- Surface touched (frontier list): `consonance/vtime` (new module),
  `consonance/hypercall-proto` (new service id + client verb + reference
  service), `consonance/vmm-core` (engine + vendor/x86 doorbell gate + control
  server + tests), `guest/linux` (patch machinery, config, scan gate),
  `docs/INTEGRATION.md` §1.2 (service table row), `docs/PARAVIRT-CLOCK.md`
  §§1/2/3.1 (the ruling-mandated amendments — doc and code agree at freeze).
  `vm-state` needed no change (108 had already renamed the mirror).
- Every existing composition is byte-identical: pvclock is offered only via
  `enable_pvclock` (no production composition calls it yet — the live gates
  compose it explicitly), the doorbell gate short-circuits identically when
  unoffered, `run_until_deadline` returns exactly the old min without a
  registration, and the `PVCK` state-blob chunk exists only when offered. The
  portable test
  `pvclock_unregistered_guest_is_guest_identical_and_differs_only_in_pvck`
  pins the guest-side half of that claim (guest-observably identical; blobs
  differ by exactly the configuration chunk); existing goldens (toy
  det-corpus, box O2 digests) are untouched by construction (state_hash is
  not in O2; O1 is run-vs-run).
