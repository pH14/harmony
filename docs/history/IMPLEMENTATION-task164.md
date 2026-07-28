# tasks/164 — box-window.sh: a lease must outlive the ssh shell that took it (hm-nvwx)

**Surface:** `scripts/box-window.sh` + its documentation (this file, and the coordinator
row in `docs/BOX-PINNING.md`) + `scripts/box-window-test.sh` (the mandated regression).
No Rust in scope.

This is the review record; it also serves as the PR body. It **leads with the before/after
of the defining regression** because that pair is the whole claim.

---

## Before / after — the defining regression, on the real module

The regression (spec): *acquire from a short-lived ssh, then invoke any verb, and assert
the patched module is NOT reverted.* Both runs below used a **pinned victim** —
`taskset -c 2 sleep` on the leased core — as a stand-in for a live gate, so the failure is
visible as "the module reverted **out from under a running, pinned workload**." Each
`box-window.sh` call is a separate `ssh hetzner '…'` (a genuinely short-lived shell, exactly
how every worker reaches the box), so the acquiring shell's pid is dead the instant it returns.

### OLD script — RED (module reverted out from under the live victim)

```
$ ssh hetzner '/root/box-window.sh acquire laneX'      # short-lived ssh
=== window open: loading patched KVM ===
2
# by hand:  lsmod kvm = 1400832   victim pid 1492360 pinned to core 2 (affinity mask 4)
#           lease laneX = "1492369 2"   (old format: pid core)
$ ssh hetzner '/root/box-window.sh release laneY'      # a DIFFERENT, well-behaved lane
sweeping stale lease laneX (pid 1492369 dead)          # <-- laneX swept: its ssh shell is gone
=== window close: reverting to stock KVM ===
lsmod kvm = 1396736 (want 1396736)
REVERT OK
# by hand:  lsmod kvm = 1396736   victim STILL ALIVE on core 2   leases: (empty)
```

A well-behaved concurrent lane releasing *its own* lease swept laneX (dead pid), saw zero
live leases, and **rmmod'd the patched module while the victim was still running on the
leased core.** In a real overlap this is a corrupted determinism gate that would read as a
determinism *finding*, not a tooling accident — the expensive failure.

### NEW script — GREEN (identical sequence, module preserved)

```
$ ssh hetzner '/root/box-window-new.sh acquire laneX'  # short-lived ssh
=== window open: loading patched KVM ===
2
# by hand:  lsmod kvm = 1400832
#           lease laneX = "1784990001 1492545 2"   (new format: deadline pid core; pid 1492545 already DEAD)
$ ssh hetzner '/root/box-window-new.sh release laneY'  # SAME well-behaved verb
# (no revert message)
# by hand:  lsmod kvm = 1400832   victim STILL ALIVE on core 2   laneX still held
$ ssh hetzner '/root/box-window-new.sh release laneX'  # last live lease out
=== window close: reverting to stock KVM ===
lsmod kvm = 1396736 (want 1396736)
REVERT OK
# by hand:  lsmod kvm = 1396736   leases: (empty)
```

The recorded pid `1492545` was already dead (the ssh shell exited), yet the lease stayed
**time-live** through the sweeping `release laneY`, so the window was preserved; only the
genuine last-lease-out `release laneX` reverted. This is the exact sequence that fails on old
code, passing on new.

The same red→green pair runs **hermetically** (off-box, faked module) in
`scripts/box-window-test.sh` scenarios A and B, so the negative control is re-runnable in CI
without the box, and keeps failing on the old code (read from git ref `c48d0901`) even after
this fix lands.

---

## The fix, and why this direction

**A lease is LIVE while `now < deadline` OR its recorded pid is still alive.** Liveness stops
depending on pid alone.

- **Time makes the natural calling pattern correct.** Every caller is
  `ssh <box> '<one-shot command>'`; a fresh `acquire` is now valid for its TTL (default
  1800 s) regardless of whether that shell survives. This is the whole bug: a pid-only lease
  is "stale the instant it exists" for a short-lived ssh.
- **The pid is kept only to *extend* liveness, never to cut it short.** Callers that
  legitimately hold a lease inside one long-lived box process — the 3-wide campaign
  orchestrators `dissonance/benchmark/campaign-data/run-bug{1,3}-campaign.sh`, which acquire
  with stdout redirected to a file *specifically* so box-window's parent stays the
  orchestrator — keep working unchanged: their pid stays alive, so the lease never expires
  under them even past the TTL. Under the old design the pid's *death* killed a lease; under
  the new one it can only *lengthen* a lease's life within/ beyond its TTL, never shorten it
  below the TTL. Those scripts' `$PPID`-workaround comments are now obsolete-but-harmless.
- **A crashed/abandoned gate self-heals.** When both the TTL lapses and the pid is dead, the
  next verb sweeps the lease and reverts — a *bounded* leak that heals itself, strictly better
  than both the old "swept instantly" bug and a naive detached-holder's "leaked forever."

New surface, all additive (contract preserved): `acquire <name> [--exclusive] [--ttl <s>]`,
`renew <name> [<s>]`, unchanged `release` / `status`. Lease file format changed from
`pid core` to `deadline pid core`; `.exclusive` now records the exclusive lease's *name*
(so it self-clears when that lease goes) instead of a pid.

### Alternatives considered and rejected

- **Long-lived holder process** (`setsid`/`nohup` whose pid is recorded). A detached holder
  outlives the ssh shell, but if the caller crashes without releasing, the holder keeps
  running and the window **leaks forever** — worse than a bounded TTL. Making the holder
  self-expire just reinvents time-based leases with an extra process to spawn and reap. Pure
  overhead over the chosen design.
- **Refuse-to-revert-while-busy** (scan for a pinned workload on a leased core). A heuristic:
  "is a workload live on core N" has no crisp signal (a gate between phases looks idle; an
  unrelated pin looks busy). A false "busy" leaves the box **patched** — that is failure
  mode (a) of this very bug (nothing reverts at lane end). I did not add it even as a silent
  backstop, for that reason. Time-or-pid liveness is a single, crisp mechanism.

---

## Contract preserved (verified on the real module)

- `acquire` prints the leased core; concurrent gates get **distinct** cores in one window:
  `g1→2, g2→1, g3→3`, three independent short-ssh leases coexisting in a single patched
  window (`1400832`) — a thing the old script could not do, since each new short-ssh acquire
  would sweep the previous one's dead pid. Draining two kept it patched; the last `release`
  did `REVERT OK`.
- `--exclusive` still excludes both directions: an exclusive holder blocks a joiner
  (`joiner rc=124`, no lease created; bounded with a box-side `timeout` so no orphaned looping
  process is left), and (hermetically) an exclusive request waits while any shared lease lives.
- `release` of the **last live** lease reverts to stock `1396736` and verifies loudly; a
  non-last release does not.
- `status` reports (and, per spec, sweeps as a side effect — so the box's stock/zero-lease
  state was always checked **by hand** via `lsmod` / `ls /root/box-window-leases/`, never via
  `status`).

## Gates

- `shellcheck scripts/box-window.sh scripts/box-window-test.sh` — clean.
- `bash scripts/box-window-test.sh` — **31/31 green** (A/B defining regression red→green,
  C TTL self-heal, D pid-extension + death-releases, E exclusivity both directions,
  F distinct-core concurrency + all-cores-held blocks, G renew + expired-renew-refused).
- Box (`ssh hetzner`, Intel i9-9900K, governor=performance, no_turbo=1, leased core 2/1/3,
  SMT siblings idle per `docs/BOX-PINNING.md`): the before/after above on the real patched
  module (`1400832`) ↔ stock (`1396736`), plus concurrency and exclusivity. Box verified at
  **stock 1396736, kvm_intel 0 users, zero leases** before and after (by hand), and test
  residue (`/root/box-window-new.sh`) removed.

## Callers checked (no contract break)

- `run-bug{1,3}-campaign.sh` (box orchestrators): use only `acquire` stdout + `release` /
  `--exclusive`; depend on pid-liveness via a long-lived parent — **preserved** by the pid
  branch. No dependency on lease-file *contents*.
- `tasks/*` and `consonance/vmm-core/tests/live_*.rs` reference only the `acquire`/`release`
  verbs and the printed core — unaffected.
- No CI/GitHub-Actions caller references `box-window.sh` (grep of `.github`/`*.yml`: none).
- Nothing in-repo parses the lease-file format (only historical `docs/history/*` prose
  mentions the old `pid core`, which is left as accurate history).

## Known limitations / integrator notes

- **Deploy step (integrator/foreman):** the box's live coordinator is `/root/box-window.sh`
  (and `~/box-window.sh`); this PR changes only the repo's `scripts/box-window.sh`. After
  merge, copy the merged script to the box (`scp scripts/box-window.sh hetzner:/root/box-window.sh`)
  so the box runs the fixed version. The box's `/root/box-window-leases/` is empty at cutover,
  so no old-format `pid core` files linger; a mixed old-file/new-script state is not a concern.
- The default TTL (1800 s) is the grace window for the *pure* one-shot pattern (acquire and a
  long campaign in separate short-lived sshes, with no live pid to extend liveness). A campaign
  longer than the TTL under that pattern must `renew` (or acquire with a covering `--ttl`), or
  it risks expiry mid-run. Orchestrators that hold the lease in a long-lived process (the
  `run-bug*` pattern) are unaffected — their live pid covers any duration.
- Time source is `date +%s` (wall clock) — appropriate here: lease lifetime is an operational
  timeout, not part of any determinism-relevant computation. `BOX_WINDOW_NOW` overrides it for
  hermetic time tests only.

## Review round 1 — discovery tribunal (REQUEST_CHANGES, head e018c3cf → this batch)

One P1 + four judge-designated ride-alongs, one batch. No contract surface changed; all edits
confined to the two scripts (+ this note). Beads F4→`hm-v1m0`, F6→`hm-ubkp` parked (not touched
here). Deploy remains `hm-tp45` — **merge does not deploy**; the box still runs
`/root/box-window.sh` until that chore runs.

- **F1 (P1) — the invariant was enforced only in `release`.** An expired last lease swept by
  `status`/`renew`/`acquire` left the module patched with zero leases (byte-for-byte the
  observed-live `hm-nvwx` state), and a new lane's `acquire` then hard-aborted in `load_patched`.
  Fixed by factoring the empty-window revert into a `revert_if_empty` helper called after
  **every** `sweep_stale` under lock (acquire — before the `n==0 → load_patched` decision, so an
  orphaned window reverts and reloads cleanly; renew — covers both branches; release; status).
  Tests: scenario G now asserts the module is stock immediately after the expired-renew refusal;
  new scenario H proves acquire-after-expiry from the orphaned-patched state succeeds (rc 0, core
  printed) and that `status` heals too. Suite 31→43 green.
- **F2 (P2) — `is_int` admitted TTLs bash arithmetic rejects/wraps.** `--ttl 09` died *after*
  `load_patched`; `--ttl ≥ 2^63` wrapped to a negative deadline. Normalized `TTL=$((10#$TTL))`
  and bounded `1..31536000` after `is_int` in acquire and renew, so bad TTLs are rejected before
  any module transition. Verified: `09`→ok (deadline +9), `0`/`99999999999`→rc 2 with kvm left
  stock.
- **F3 (P2) — flock fail-open.** The macOS suite had been running with every `flock` failing 127,
  silenced by redirects. Added a fail-closed guard (`command -v flock … || exit 3`) so an
  unserialized run is impossible on the box; the hermetic suite supplies a deliberate, commented
  no-op `flock` shim in its FAKEBIN (the suite is strictly sequential, so there is nothing to
  serialize). Verified: no flock on PATH → `FATAL … refusing to run unserialized`, rc 3.
- **F7 (P2) — blocked-acquire tests couldn't tell "blocked" from "errored".** `with_timeout`'s rc
  is now captured and asserted to be the kill path (143/137 = SIGTERM/SIGKILL) at all three sites
  (E ×2, F ×1), alongside the lease-count check.
- **F5 (P3, opt-in) — deadline-only lease counted live but held no core.** `lease_live` now also
  requires non-empty pid and core fields (pid `-` allowed), closing the truncated-file footgun at
  the sweep choke point.
- **F8 (P3, opt-in) — dead knob.** Removed the unused `BOX_WINDOW_CORES` seam (`CORES=(2 1 3)`);
  kept `BOX_WINDOW_KVM_B`/`STOCK_SIZE`, which the suite uses.

Gates re-run: `shellcheck` clean on both scripts, `bash -n` OK, suite **43/43** (scenario A still
red on the OLD script — negative control intact). Per the judge, the box lane is **not** re-run
for this batch — the decision-logic changes are covered by the extended hermetic suite, and the
box gates re-run naturally at the `hm-tp45` deploy step.
