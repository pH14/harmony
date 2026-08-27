# Exit-count V-time: completing the instruction-surface closure

Follow-on workstream to `docs/EXIT-VTIME.md` §2.4. That section
defines the four-layer closure of the untrusted instruction surface and the
milestones build two layers in passing (M1 virtualizes the identity registers
and seeds the disposition table; M3 activates the guest-kernel userspace
traps). This document plans the rest: proving the closure works, completing
its recording artifact, deciding its open entries, and arming its hardening
layer. **None of this is part of the standing `EXIT-VTIME.md`
milestone sequence** — it layers on after M3 exists, and its own ordering is
T0 → T1 → (T2, T3 in either order) → T4. The verification rules of
`EXIT-VTIME.md` §3 apply to every item here unchanged.

## T0 — adversarial trap verification

The traps' failure mode is silent: a `CNTKCTL_EL1` bit left unset means
unaudited code reads the real counter and nothing errors. M3's same-seed
condition catches this when the payload happens to read time; T0 proves the
closure directly.

*Build:* a hostile guest payload that deliberately executes every closed
operation from EL0 — `CNTVCT`/`CNTFRQ` reads, `CTR_EL0`/`DCZID_EL0` and the
ID registers, attempted PMU register access, `RNDR` where silicon has it —
including one path that maps writable-executable memory at runtime, emits a
counter read into it, and executes it (the JIT case no static scan sees).
The payload reports every value it observed through the SDK JSON channel.

*Passes when:* every observed value matches the pvclock page / pinned model
(reported values checked against the run's own normalized log); ten same-seed
runs of the hostile payload produce identical normalized logs and
`state_hash` sequences; the residual operations behave as the disposition
table records (a `RNDR` result, where executable, appears in the report as
residual — never in guest state that the campaign oracles consume).

*Does not count unless:* a deliberately fail-open build — the trap
configuration bit left unset — demonstrably fails this same suite, at the
divergence the open trap causes. A verification suite that has never caught
an open trap proves nothing.

## T1 — the arm64 disposition table, complete and frozen

§2.4 names the artifact; M1's probe seeds it. T1 finishes it in the mold of
`docs/cpu-msr-contract.toml`:

*Build:* enumerate the full EL0/EL1-reachable sysreg and untrusted-operation
surface (generated from the ARM ARM's machine-readable sysreg listings, the
way the MSR table was built from kernel headers, so completeness is
mechanical rather than recalled); rule every entry into one of §2.4's
layers — protocol / image audit / guest-kernel trap / substrate hardening /
documented residual — or mark it not-applicable with the reason; record the
M1 probe's measured Hypervisor.framework behavior per entry; emit the
machine-readable table plus a prose spine, and a CI check that the backend's
installed policy and the table agree.

*Passes when:* the table covers the enumerated surface with no unruled
entries; the CI agreement check is green; the table is snapshot-frozen the
way the public-API surfaces are, so a ruling change is a reviewed diff.

*Does not count unless:* the enumeration source and generator are committed
(a hand-typed table cannot claim completeness), and the CI check was shown
to fail against a seeded disagreement (one entry's ruling altered without
the corresponding code).

## T2 — the LL/SC ruling

§2.4 records userspace LL/SC as a candidate relaxation: spurious
exclusive-monitor retries between exits re-converge before the next
observation point. T2 decides it, and must confront the caveat that bounds
it:

**The convergence argument holds only for side-effect-free retry loops.**
`STXR` reports failure in a register; a standard compiler-emitted atomic
loop overwrites that evidence on success and leaves no trace of the retry
count. But a program that *accumulates* its retries — a counter incremented
in the retry path — makes spurious clears guest-visible, and no layer can
close that. The ruling therefore has exactly two coherent shapes: admit
userspace LL/SC with retry-observing programs recorded as a documented
residual of the cooperative posture (the same standing as a program that
executes `RDRAND` against the pinned feature bits), or keep the prohibition.

*Build:* the written convergence argument in the disposition table, stating
its conditions (exit-only delivery, debug exits excluded from events, single
vCPU, side-effect-free retry path); an empirical stressor — a userspace
`LDXR`/`STXR` workload run same-seed under deliberately noisy and quiet host
conditions — plus a retry-accumulating variant demonstrating the residual is
real.

*Passes when:* the clean stressor produces identical normalized logs and
`state_hash` sequences across noisy and quiet host runs; the accumulating
variant demonstrably diverges (proving the boundary is where the argument
says it is); the table records the ruling and the image checks are updated
to match it (kernel prohibition retained either way).

## T3 — layer 4 as an auditor for layers 1–3

On Linux hosts carrying the patched KVM, the instruction exits are worth
more than hardening: armed but expected-silent, they measure the other
layers' completeness.

*Build:* a host configuration with the determinism intercepts enabled purely
as tripwires, and a CI job on such a host running the acceptance corpus and
the T0 hostile payload under it.

*Passes when:* runs under the armed configuration produce normalized logs
identical to stock-host runs (arming is observationally inert), and the
corpus completes with **zero** stray-operation exits — every closed
operation was closed by layers 1–3 before layer 4 could see it. A nonzero
count is a leak those layers missed, localized to a guest PC by the exit.

*Does not count unless:* the tripwire path was shown live — the T0 fail-open
build run under this configuration must register the strays that layers 1–3
failed to stop.

## T4 — the x86 port of the closure

When an x86 exit-count mode exists (stock `KvmBackend` plus the
exit-count run loop — the configuration that runs on default GitHub
runners), the closure ports with it: `CR4.TSD` and `CR4.PCE=0` with the
guest kernel completing reads from the pvclock page, the x86 rows of the
disposition table (`RDRAND`/`RDSEED` as the recorded residual,
`TPAUSE`/`UMWAIT` closed by the substrate's default-off controls), and T0's
hostile payload rebuilt for x86 — including its JIT path — passing the same
suite. T3's auditor configuration is the natural CI companion, since the
patched KVM exists on exactly these hosts.

## Timing rationale

T0 belongs immediately after the standing plan's M3 (its subject exists only
then); running it earlier requires only the M1 backend plus a minimal guest,
which is a legitimate pull-forward if trap code lands before M3 assembles
the postgres payload. T1 can start as soon as M1's probe emits data. T2 and
T3 are independent of each other. T4 waits on a decision outside this
document (building the x86 exit-count mode at all). The one closure item
that could not wait was M3's same-seed condition — the oracle that catches a
fail-open trap — which is why it lives in `EXIT-VTIME.md` M3 rather
than here.
