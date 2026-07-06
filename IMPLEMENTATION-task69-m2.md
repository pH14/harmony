# Task 69 Milestone 2 — the GO/NO-GO #2 infrastructure + box runbook

> **Milestone boundary.** M1 (PR #65, `a39a8d4`) landed the *mechanism* + a
> determinism smoke and left the **GO/NO-GO #2 ruling explicitly PENDING**. This
> M2 work lands everything needed to **run and rule** the gate — the two deferred
> M1 prerequisites, the real task-67 signal on the box path, the round-7 P2s, and
> the box campaign bin — **all portably gated and verified on the box toolchain**.
> **It does not itself contain the ruling:** the actual ≥20-seed × 2-config ×
> 3-bug real-KVM campaign that emits `CORRELATION-REPORT.md` and rules GO/NO-GO is
> the remaining step (the "Box campaign runbook" section). Until that campaign
> runs, **the Phase-F gate is still PENDING** — nothing here should be read as a GO.

## What this milestone lands (all gated)

### 1. Socket console capture (M1-deferred prereq 1) — the signal's live input

The signal config is the real task-67 `logtmpl` `LogSensor` → `CellFnV1`, which
scrapes **`RunTrace.records`** (guest console lines). On the box the campaign
drives a `SocketMachine` over `control-proto`, which could not see the
server-side serial capture — so `Machine::console()` returned empty and the
signal had no cells. Fixed end-to-end:

- **`dissonance/control-proto`** — a new `Request::Console { offset }` /
  `Reply::Console { total, chunk }` wire verb, paged + frame-bounded exactly like
  the task-73 `SdkEvents` verb (codec + tag constants + the roundtrip proptest +
  the loopback harness + the `public-api.txt` golden all updated).
- **`consonance/vmm-core/src/control.rs`** — the server handler reads
  `Vmm::serial()` and pages `serial[offset..]` (`page_console`). A **pure read**:
  it never advances the VM or touches hashable state. A `console_drain_is_
  determinism_neutral` unit test proves a run's `state_hash` is identical whether
  or not the console is drained — records are host-side observation, never coupled
  into the hash (the determinism-neutrality the task requires).
- **`dissonance/explorer/src/adapter.rs`** — `SocketMachine::console()` pages the
  verb from a **cursor baselined at branch/replay** to the snapshot's captured
  serial length (probed once per `snapshot`, stored in `SnapMeta.serial_len`), so
  it reads only a run's NEW bytes. Lines are split exactly as
  `runtrace::decode_chunks` does over a single chunk (each completed line keeps its
  `\n`; a trailing remainder is a final line) and stamped at the run's stop
  `Moment`. The scripted adapter test harnesses were converted to auto-number
  replies by position so the added `snapshot` probe never desyncs them.
- **`dissonance/conductor/tests/loopback.rs`** — `console_capture_round_trips_
  over_the_socket` drives a serial-emitting fork guest through the **real**
  vmm-core server and proves `SocketMachine::console()` pages the banner back off
  the wire, determinism-neutral end-to-end.

> **Surface note (boundary waiver).** `dissonance/control-proto` is not in task
> 69's named surface list, but "socket console capture" (issue #66's M2 prereq 1)
> *is* the wire — you cannot surface the server console to a socket client without
> a wire verb. The change is additive and backward-compatible (a new
> Request/Reply variant, exactly as task 73 added `SdkEvents`), and the server
> half lives in `vmm-core` (in-surface) and the client half in `explorer`
> (in-surface via `seam.rs`, the named deferred item). Flagged here as a waiver,
> mirroring M1's `conductor` surface amendment.

### 2. Fault-moment rebasing (M1-deferred prereq 2)

On the `SocketMachine` path the adapter re-anchors a branch env's override keys by
**adding the snapshot's real seal V-time** (`adapter::rebase_to_wire`). The
manifest windows are absolute in the toy's frame (`BASE_VTIME + offset`), so an
absolute manifest `Moment` would land at `seal + BASE_VTIME + offset` — past the
real vulnerable window (`seal + offset`) — and the bug would never fire. Added
`BenchConfig::fault_rebase` (0 = toy absolute frame; `BASE_VTIME` = box
relative-offset frame, via `BenchConfig::box_campaign`), threaded through
`mint_scenario_env`, so a fault is keyed at the bare `offset` and the adapter's
`+ seal` restores the correct absolute window on the box. A constant subtraction —
the PRNG draw sequence (and thus which schedules the search visits) is unchanged.

### 3. The real task-67 signal on the box path (user ruling 2026-07-06)

**GO/NO-GO #2 must measure the actual CellFn the selectors (tasks 70/72/76) get
built on, not the M1 content-keyed stand-in** — a verdict on a placeholder
doesn't transfer, so measuring it would waste the multi-hour box campaign. So
`benchcampaign::cells_of` now runs the **real** `LogSensor` → `CellFnV1`: a
campaign-persistent `LogSensor` clusters the (marker-filtered) console into
template species, and `CellFnV1` keys the **accumulating** species slice into
bounded cells. One `SignalCells` (fresh `LogSensor` + `CellFnV1`) per
`(config, seed)` campaign.

**This is parallelism-safe by construction.** `CellFnV1`'s default key is
count-based — `species-progress = log2_bucket(k)` and `last-new-species =
max_id mod 64`, and template ids are minted **densely in first-seen order**, so
`max_id = k-1`. Both components are a function of the distinct-species *count* `k`,
not of which template got which id. So **independent per-seed codebooks stay
cross-seed comparable** — the seeds parallelize (no shared/persisted codebook) and
the report still pools. And the ruling (`report.rs`) uses only per-seed cell
*counts* (measure 1) + TTB medians (measure 4) + coverage, none of which need
cross-seed-identical ids. **No `report.rs` change was needed.** The M1 "persisted
codebook for cross-log stability" concern was a red herring for the count-based
default `CellFnV1`.

- **Signal guardrail (honesty).** The box bin prints the distinct-cell count and
  **warns loudly if the signal config produced ZERO cells** (the real sensor saw
  no console) — a vacuous signal campaign must never quietly pass as a valid gate
  input. If the real sensor genuinely cannot make cells live, that is a STOP-and-
  report condition, never a silent fall-back to the stand-in.
- The round-6 marker exclusion is preserved: the bug's serial marker is filtered
  out of the console **before** clustering, so it never mints a template species
  (novelty must not key its own attribution marker).

### 4. Round-7 P2s (folded in)

- `explorer/src/stads.rs` — the `Frac` `Ord` cross-multiply now falls back to an
  **exact, non-overflowing continued-fraction comparison** on `u128` overflow
  (fast path unchanged for campaign magnitudes).
- `guest/linux/order-super.c` — the involuntary-ctxsw counter is now sampled
  **after** the torn window fully closes (`mirror = ~primary`), so an interrupt
  landing in the last sliver of the window is not missed.
- `dissonance/benchmark/src/manifest.rs` — the ORDER_BUG (bug 2) crash-kind is
  now `Shutdown` (the real isa-debug-exit → reboot terminal), matching bug 1.

### 5. The box campaign bin — `conductor benchcampaign box`

`conductor benchcampaign box --bug N --config signal|baseline --seed S --out
log.json` runs **one** `(bug, config, seed)` campaign against a real planted-bug
guest on patched KVM and emits its `CampaignLog` (the offline `benchmark-report`
input) plus per-find `state_hash` lines. It drives the **identical**
`run_bench_campaign` loop the portable gate runs against the toy, only the backing
guest swapped, with both prereqs live (console capture → the signal; fault rebasing
→ the fault lands). One campaign per invocation, so the operator parallelizes
3-wide across leased cores and compares each finding's `state_hash` solo vs
co-tenant. `run_bench_campaign` now returns `BenchOutcome { log, certs }`, where
each `FindCert` carries the reproducer env + finding `state_hash` for that check.
Box calibration is **data, not code** — `--calibration cal.json` loads a serialized
(box-tuned) `Benchmark` manifest via serde (`TriggerParams` is `#[non_exhaustive]`
so it cannot be constructed externally, but it deserializes), defaulting to the toy
`wave5()` fixture.

## Verification status

**Portable (macOS):** `build` / `nextest` / `clippy -D warnings` / `fmt` /
`cargo deny` / `public-api` all green on every touched crate (`control-proto`,
`vmm-core`, `explorer`, `conductor`, `benchmark`).

**Box toolchain (`ssh hetzner`, `rustc 1.96.1`, pinned `taskset -c 2`, 2026-07-06):**
- `cargo build -p conductor --all-features` — **compiles**, incl. the
  `cfg(target_os="linux")` `boxrun::run_bench_campaign_box`. Closes the
  cfg(linux)/rustc-1.96 review gap for this blind-written box code.
- `cargo clippy -p conductor --all-features --all-targets -- -D warnings` — clean.
- Determinism-sensitive tests pass on the box toolchain: `benchcampaign` (real
  CellFnV1) 10/10; `console_capture_round_trips_over_the_socket` 1/1; vmm-core
  `console_drain_is_determinism_neutral` + `page_console_paging_math` 2/2; explorer
  `adapter::tests` 24/24; `stads::tests` 11/11.
- **KVM left on stock `1396736`** — only cargo build/test/clippy ran; no patched
  module was ever loaded. Verified on a fresh ssh before and after.

**What is NOT yet validated (the remaining gate-deciding step):** the real ≥20-seed
campaign has not run on patched KVM. So the box gates 2–4 (a certified find per
bug replaying 25/25; the committed `CORRELATION-REPORT.md`; the GO/NO-GO ruling)
are **PENDING**. The infrastructure to run them is complete and box-verified; the
run itself is the "Box campaign runbook" below. In particular, **the live signal's
discriminating power on the real guest is unproven** — the #1 thing the campaign
validates (does the real `LogSensor`/`CellFnV1` produce cells on the box, and do
they correlate with bugs?).

## Box campaign runbook (the remaining ≥20-seed run)

Environment: box-only, patched KVM. **Box safety is critical** — stock KVM is
`1396736`; always leave the box on stock + verified after any patched run (see the
spec's Box-safety section and `docs/BOX-PINNING.md`). Lease cores via
`/root/box-window.sh` (frontier-leasable set `{1,2,3}`, siblings idle); the
campaign's `state_hash` is microarchitecture-independent, so **up to 3 concurrent
campaigns on distinct cores** is sanctioned (and is the determinism stress-test).

1. **Build the three planted-bug images.** Bug 1's `initramfs-campaign.cpio.gz`
   already exists (task 60). Build bugs 2 & 3 from `guest/linux/order-super.c` and
   `guest/linux/uuid-super.c` via the `build-campaign-image.sh` / `campaign-init.sh`
   conventions (distinct markers `ORDER_READY` / `UUID_READY`, `ORDER_BUG` /
   `UUID_BUG`). Validate each boots to its readiness marker.

2. **Calibrate each bug's trigger (the iterative bring-up).** The `wave5()` manifest
   windows are toy stand-ins. For each bug, boot a bring-up guest, pin the real
   trigger parameters (bug 1: the ledger gpa via `/proc/self/pagemap` + the
   vulnerable window offset past `CAMPAIGN_READY`; bug 2: the reschedule-class
   vector + the update-window offset; bug 3: the entropy prefix length), and write
   them into a **calibration JSON** (a serialized `Benchmark`). The key convention:
   set each fault window's `.0` to `real_offset + BASE_VTIME` so the `fault_rebase`
   subtraction + the adapter's `+ seal` land the fault at `seal + real_offset`.
   Dial each window/prefix so expected naïve TTF sits in ~10²–10³ branches.
   Confirm on a bring-up run that the ground-truth triggering schedule fires the
   bug 100% and a nominal seed never does.

3. **Run the campaign, parallelized + determinism-stress-tested.** For each bug ×
   `{signal, baseline}` × **≥20 seeds**, one invocation each:
   ```sh
   CORE=$(/root/box-window.sh acquire t69bench)   # leases a core {1,2,3}
   taskset -c $CORE conductor benchcampaign box \
       --bug B --config signal|baseline --seed S \
       --calibration cal.json --initramfs initramfs-<bug>.cpio.gz \
       --ready-marker <MARKER> --out logs/B-config-S.json
   /root/box-window.sh release t69bench             # last lease out reverts to stock + verifies
   ```
   Run up to 3 concurrently on distinct cores. **The determinism stress-test:** for
   a sample of `(bug, config, seed)` trials, run the trial SOLO and again while 2
   other campaigns are co-tenant on other cores, and confirm the `FIND … state_hash
   <hex>` lines (and the whole `CampaignLog` JSON) are **identical**. Report
   explicitly that solo == co-tenant. **If any `state_hash` differs solo-vs-co-
   tenant, or any 25/25 certification replay diverges under co-running, that is a
   P0 determinism leak — STOP the campaign and escalate** (with both hashes, the
   seed, and the cores). Never serialize-and-continue to make it vanish; never
   report a finding whose replay only passed serially as if it passed under load.
   Measure and report the real per-run wall-clock on the first run.

4. **Certification (already enforced in-loop).** A find is only logged if the bug's
   serial marker attributes it AND the reproducer replays the identical
   `(stop, state_hash)` **25/25** — so every logged find is certified. A nominal-
   seed control run must crash on none (a per-bug sanity invocation with a
   known-clean seed).

5. **Render the ruling.** Concatenate the per-`(config, seed)` `CampaignLog` JSONs
   into one array and run the offline report:
   ```sh
   benchmark-report --logs all.json --out dissonance/benchmark/CORRELATION-REPORT.md \
       --budget <B> --effect-num 3 --effect-den 10 --eps-num 1 --eps-den 1000
   ```
   The report enforces the Klees floor (≥20 independent seeds + finders per
   bug/config) and rules **GO** iff novelty correlates with progress (ρ ≤ −3/10) on
   ≥2 of 3 bugs AND the signal median is not worse than baseline on any bug; else
   **NO-GO** → iterate the CellFn (task 67), re-run — *the search is not the fix*.
   Commit `CORRELATION-REPORT.md`. **A NO-GO is a real result — surface it, don't
   hide it.**

## Deviations considered & rejected

- **A shared/persisted campaign codebook for cross-seed cell-id stability**
  (the M1-doc plan) — rejected. It would serialize the seeds (each resumes the
  prior's codebook), killing the 3-wide parallelism. Unnecessary: `CellFnV1`'s
  count-based key is already cross-seed comparable under independent per-seed
  codebooks, and the ruling needs only per-seed counts. Independent codebooks are
  both correct and parallel.
- **Keeping the M1 content-keyed stand-in on the box** — rejected by the user's
  ruling (measure the real signal the selectors get built on).
- **Changing `report.rs` to per-seed STADS** — not needed; the pooled STADS is
  valid because `CellFnV1` cell keys are (mostly) cross-seed comparable, and STADS
  is instrumentation that does not gate the ruling regardless.

## Known limitations / integrator notes

- **The box campaign has not run** — the GO/NO-GO ruling and the committed
  `CORRELATION-REPORT.md` are the remaining step (runbook above). Everything
  needed to run it is complete and box-verified.
- **Per-bug box calibration is genuinely iterative** — the manifest windows are
  toy values; the real guest windows/gpas/prefixes must be pinned on the box
  (step 2). M1 never validated any bug's live trigger, and the entropy payload
  took M1 three rounds to stabilize, so budget for iteration.
- **A box worktree `~/harmony-t69m2`** (branch `task/sbc-m2`) was created on the
  box for the build/test verification, from a git bundle of this branch (no push
  to origin). It holds only build artifacts; no KVM was touched.
- **Push:** `origin/task/signal-bug-correlation` still has the stale pre-squash M1
  head (`48fb224`); a normal push is non-fast-forward. Push M2 with
  `git push --force-with-lease origin task/signal-bug-correlation` (safe — M1's
  content is already in main via the #65 squash).
