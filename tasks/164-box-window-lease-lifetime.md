# Task 164 — box-window.sh: a lease must outlive the ssh shell that took it (hm-nvwx)

**Bead:** `hm-nvwx` (P1 bug) — `bd show hm-nvwx` first; it carries the live observation.
**Surface:** `scripts/box-window.sh` and its documentation. Nothing else.

**You are the only box-touching worker this session.** The box is free and at its clean
resting state. Keep it that way: this task's whole subject is the mechanism that guarantees
that.

## The defect, observed live on 2026-07-25

`box-window.sh` stores each lease as a file containing `pid core`, and every verb sweeps
leases whose pid is dead. So a caller that acquires inside its own `ssh` invocation —

```sh
ssh <box> 'box-window.sh acquire t157'      # this shell exits immediately
```

— creates a lease that is **stale the instant it exists**. The next invocation of any verb
sweeps it, and because the script's own rule is that *a window with zero live leases is
reverted on the next invocation of any verb*, that sweep will `rmmod` the patched module
**out from under a live campaign running in a different ssh session**.

Observed state during the tasks/157 lane: KVM patched (1400832), `/root/box-window-leases`
**empty**, and a pinned `campaign-runner` sweep live on core 2.

## Why this is a P1 and not a nit

The script's stated box-safety invariant is *"the window NEVER outlives its last lease."*
This failure mode **inverts it**: the window outlives its lease bookkeeping entirely, so

1. nothing reverts to stock when the lane ends (the box is left patched), and
2. a **correctly-behaved** concurrent verb becomes destructive.

It was survivable only because the work order serialized the box to one worker. The next
time two lanes overlap — which the coordinator exists to permit — this corrupts a live
determinism gate, and the resulting divergence would look like a determinism finding rather
than a tooling accident. That is the expensive failure: not the crash, the misdiagnosis.

## Fix — you choose, and justify it

Three directions, all defensible. Pick one, say why in the implementation record, and say
what you rejected:

- **Long-lived holder**: the lease is held against a process on the box that outlives the
  ssh shell (`setsid`/`nohup` holder whose pid is what gets recorded).
- **Time-based leases with explicit renewal**: liveness stops depending on a pid at all.
- **Refuse-to-revert-while-busy**: never revert while any pinned workload is running on a
  leased core, regardless of lease bookkeeping.

Consider that callers are `ssh <box> '<one-shot command>'` by construction — that is how
every worker reaches the box — so a fix that only works when acquire and run share a shell
has not fixed anything. Whatever you choose must make the *natural* calling pattern correct,
not merely make a careful pattern possible.

**Do not break the existing contract**: `acquire <name> [--exclusive]` prints the leased
core; `release <name>` reverts and verifies when the last lease goes; `status` reports.
Concurrent gates on distinct cores must still work — that is the coordinator's whole reason
to exist, and a fix that serializes everything has thrown away the feature to fix the bug.

## The regression that defines "fixed"

**Acquire from a short-lived ssh, then invoke any verb, and assert the patched module is
NOT reverted.** That is the exact sequence that fails today. It is also, per the standing
doctrine, the negative control: run it against the *current* script first and show it
failing, then against yours and show it passing. A fix whose test never failed on the old
code has proven nothing.

Add whatever else is needed for the direction you chose (a stale-holder sweep still working,
`--exclusive` still excluding, last-lease-out still reverting and verifying).

## Box discipline

`ssh hetzner`. Pin every workload with `taskset` per `docs/BOX-PINNING.md` and record the
core, governor, and `no_turbo`. Verify the box is at **stock KVM 1396736 with zero leases**
before you start and again when you finish — and note that you are testing the very
mechanism that normally guarantees the second half, so check it by hand (`lsmod`,
`ls /root/box-window-leases/`) rather than trusting `box-window.sh status`, which sweeps as
a side effect of running.

**Smoke-fire-once**: probe your riskiest assumption with a short run and report it before
spending a full validation cycle.

## Gates

Shell: `shellcheck` if available, plus the regression above run on the box. There is no
Rust in scope. If a Rust or CI caller depends on the current behavior, find it before
changing the contract and say so.

## Deliverable

PR from `task/box-window-lease-lifetime` closing `hm-nvwx` with the merge. The PR body
leads with the before/after of the defining regression — old script red, new script green —
because that pair is the whole claim.
