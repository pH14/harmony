# V-time consolidation status

Branch: `claude/consonance-virtual-time-6kvrz6`

This is the live decision and evidence ledger for
[`VTIME-CONSOLIDATION.md`](VTIME-CONSOLIDATION.md). A criterion is `PASS` only
when its positive oracle and every applicable “does not count unless” clause
have run on the milestone tree. `BLOCKED` names an external dependency without
weakening the criterion.

## Recorded decisions

1. **Milestones close strictly in N0→N6 order.** Evidence from an ancestor is
   source material, not a rerun. A later milestone does not begin until the
   current milestone's complete evidence is recorded here.
2. **The pre-existing dirty workspace is preserved.** Consolidation work uses
   a clean clone; the requested branch and all commits are pushed to `origin`
   from there.
3. **N0 freezes rows, not a hand-maintained second implementation.**
   `docs/determinism-instructions.toml` is the normative instruction-surface
   table. N6 must generate its probes from those rows and fail when row count and
   exercised count differ.
4. **LL/SC admission stops at side-effect-free EL0 retry loops.** The owned
   kernel remains LSE-only. Retry-observing loops are a documented cooperative
   residual; N6 must demonstrate the accumulating variant's divergence before
   the boundary counts as verified.
5. **KVM instruction-intercept patches remain optional tripwires; branch-clock
   patches retire.** N2 removes patches 0004 (force exit) and 0005 (MTF exact
   arrival) with the retired-branch clock. Patches 0001–0003 remain only as the
   N6 auditor for layers 1–3 and are not required for stock-KVM support.

## N0 — `DETERMINISM.md`, the document of record

### Build criteria

- **PASS:** `docs/DETERMINISM.md` contains the inductive argument, split
  assumptions, per-architecture defenses, support matrix, trust boundary, and
  consolidation decisions.
- **PASS:** `docs/determinism-instructions.toml` freezes both architectures'
  complete classes from the VM-exit plan, closure plan, and x86 X3 disposition:
  arm64 time/frequency/timer programming, identity/cache, PMU, entropy, and
  exclusive monitor; x86 CPUID, TSC, PMU, entropy, MONITOR/MWAIT, WAITPKG,
  XSAVE/MXCSR, RF, and undefined AF.
- **PASS:** factual claims cite committed evidence or are marked **untested**.
  The document intentionally retains multiple untested claims, including JIT
  trap negatives, complete generated listings, entropy audits, LL/SC boundary,
  tripwire liveness, and reproducible guest construction.
- **PASS:** the closure document is deleted; its T0–T4 work is now §7 of the
  document of record and the frozen table.

### Passes-when evidence

Documentation-specific verification on the N0 worktree:

```text
TOML parse: rows=18 arm64=9 x86_64=9 claims=ok ids=unique
Local Markdown link targets: all exist
git diff --check: exit status 0
```

Repository gates on the N0 worktree:

```text
cargo build --all-features
PASS

cargo nextest run --all-features
1314 passed, 25 skipped

cargo clippy --all-features --all-targets -- -D warnings
PASS (the existing three invalid-path notices from clippy.toml remain warnings
from Clippy's configuration parser; no crate diagnostic)

cargo fmt --all -- --check
PASS

cargo deny check
advisories ok, bans ok, licenses ok, sources ok
```

The first sandboxed nextest invocation was unable to open two local telemetry
sockets (`Operation not permitted`). The same complete command rerun with local
socket permission passed all 1,314 tests; this was an execution-environment
restriction, not a tree failure.

Pinned Miri (`nightly-2026-06-16`,
`MIRIFLAGS=-Zmiri-permissive-provenance`; snapshot-store additionally uses
`-Zmiri-disable-isolation`) completed for every crate named by the workflow:

```text
hypercall-doorbell: PASS (1 unit + 8 loopback; public-API target ignored by design)
vm-state: PASS (10 unit + 10 determinism + 2 golden + 1 roundtrip +
  16 strict-decode + 4 version/ratio; public-API/regenerator tests ignored by design)
vmm-backend --all-features: PASS (63 unit + 3 contract + 2 dynamic +
  2 exhaustive + 20 run-loop + 1 vCPU-state; host-only targets contain zero
  Miri tests and public-API target is ignored by design)
snapshot-store --lib: PASS (14 tests)
vmm-core: PASS (433 unit + 68 integration; 105 deliberately ignored/host-only)
```

The first full `vmm-core` run exceeded the workflow's 240-minute ceiling in
`portable_snapshot::tests::hostile_lengths_and_all_truncations_are_total`.
[Issue #205](https://github.com/pH14/harmony/issues/205) records the gate
failure. The test was quadratic under interpretation because every prefix of
an 8 KiB memory image was hashed. Under Miri it now uses a structurally complete
128-byte image while still visiting every prefix of every section; native runs
retain the full 8 KiB corpus. The full native test passed, the focused Miri test
passed, and the complete `vmm-core` Miri job then passed with zero failures.

### Does-not-count-unless evidence

- **PASS:** both frozen tables cover every instruction class enumerated by
  `VM-EXIT-COUNT-VTIME.md` §2.4, the former closure document T0/T2/T4, and x86
  status decision 21 plus decisions 22–30.
- **PASS:** at least one claim is honestly untested; the N0 document records the
  entire N6 backlog as untested rather than inferring it from workload equality.

**N0 overall: PASS.** The document of record, frozen 18-row table, explicit
untested backlog, closure-document removal, and exact-tree repository/Miri
evidence satisfy both the positive and anti-vacuity clauses.

## N1 — one branch

### Integration decisions

- **Merge the complete x86 history, not a squashed patch.** The merge parent is
  `origin/claude/x86-prescriptive-vtime` at `b0c019f1`; its status ledger records
  X3 met at `57b16ce0` and the later head only removes diagnostic workflow steps.
- **The ARM dissonance tree and license policy win byte-for-byte.** The staged
  merge has no diff from N0 commit `a352b4c9` under `dissonance/` or `deny.toml`;
  the retained dissonance tree object is
  `b9148e8732b1d4b58841890210ee6cbfdc7eed8c`.
- **Preserve both historical ledgers.** Rename detection tried to combine the
  x86 `PRESCRIPTIVE-VTIME-STATUS.md` with the ARM
  `VM-EXIT-COUNT-VTIME-STATUS.md`; the resolution restores each under its own
  historical name.
- **Keep the M6 ARM delivery fabric with the shared trace name.** The arm64
  conflict retains userspace-GIC level assertion and the ruled in-kernel-GIC
  fallback, while adopting the x86 branch's architecture-neutral
  `trace_clockevent_delivery` helper.

### Integration evidence

```text
merge parents: a352b4c9 + b0c019f1
unmerged paths: none
dissonance/ and deny.toml diff against a352b4c9: empty
cargo build --all-features: PASS
cargo nextest run --all-features: 1327 passed, 25 skipped
cargo clippy --all-features --all-targets -- -D warnings: PASS
cargo fmt --all -- --check: PASS
cargo deny check: advisories/bans/licenses/sources ok

cargo test -p vmm-core --test prescriptive_vtime \
  comparator_rejects_one_vns_increment_at_the_exact_event -- --exact
PASS: planted +1 V-ns rejected at the exact event
```

The exact `df4d1b3b` merged tree reran the ARM boot reference with the M5
fixtures (Image `47c6eac9…c96`, initramfs `6194ec4b…053`). Ten signed-HVF
boots and ten CPU-0-pinned msr1 KVM boots each produced 38,453 portable events,
283 schedules, 136 deliveries, and 151 checkpoints. Every placement check was
green; all twenty runs had normalized digest `e2e7852e…829` and final state
hash `1dc0c1da…b17`. Direct comparison of the complete 5,954,217-byte HVF and
KVM logs passed; both have SHA-256 `4b4e7a27…7db`.

The same tree reran the original M5 one-job/two-session NES campaign with seed
`1592642082`, one worker, action limit 96, and the byte-attested game fixtures.
HVF and CPU-0-pinned KVM agreed exactly:

| Session | Segments | Events | Schedules | Checkpoints | Session digest | State hash |
| --- | ---: | ---: | ---: | ---: | --- | --- |
| 0 | 3 | 50,931 | 393 | 198 | `0c4936c4…478` | `741e95a2…4d8` |
| 1 | 4 | 50,934 | 393 | 198 | `43105757…d21` | `de72c909…e58` |

Both complete session logs passed direct byte comparison (SHA-256
`a801bfb0…d45` and `ffdd9dbd…eda`). The independently produced archive,
report, stream, and snapshot checkpoint artifacts also matched at
`384d3029…6b2`, `b3032b4a…c46`, `584e0d3a…8b2`, and `ae8d699c…b783`.
Every server-side placement check passed. The first msr1 harness attempt looked
for the wrong existing readiness label (`KVM_BOOT_READY`); its run itself was
green, and the corrected ten-run harness required `KVM_ARM64_BOOT_READY`.

GitHub Actions run
[`33162001079`](https://github.com/pH14/harmony/actions/runs/33162001079)
ran the exact merge on the X-series stock-KVM workflow. Its X2 pool contained
AMD EPYC 9V74, AMD EPYC 7763, and Intel Xeon Platinum 8573C. Every replica
passed ten boots with 35,314 events, `X2_DIVERGENCES=0`, zero component/RAM
differences, digest `f0aa6256…f48`, and complete normalized-log SHA-256
`d90abd9f…27a`. The workflow's check, probes, planted access negative, X1,
guest build, X2, and bounded hunt jobs all passed.

The exact merge also passed the pinned unsafe-crate Miri matrix with
`nightly-2026-06-16` and `-Zmiri-permissive-provenance` (plus
`-Zmiri-disable-isolation` for `snapshot-store`): `hypercall-doorbell`,
`vm-state`, `vmm-backend --all-features`, `snapshot-store --lib`,
`play-agent`, `tetanes-agent`, and `vmm-core` all completed with zero
failures. The long `vmm-core` job passed 436 unit tests (103 ignored), then
all applicable integration suites, including 22 arm64-skeleton tests, 19
event-loop tests, the loader/protocol suites, and all 12
`prescriptive_vtime` tests. Targets gated away on this host and explicitly
ignored public-API/regeneration tests remained zero-test or ignored by design.

**N1 overall: PASS.** Commit `df4d1b3b` is the single merged implementation
tree. Its planted comparator negative, all-three-machine reference reruns,
ordinary repository gates, and pinned unsafe-crate Miri matrix pass without
weakening either architecture's evidence.

## N2 — one clock

### Deletion and narrowing decisions

- **Delete the retired clock instead of adapting it.** `WorkSource`,
  `ScriptedWork`, `PerfWorkCounter`, `InjectionPlanner`, its simulator, the
  backend exact-stop path, PMU overflow plumbing, force-exit delivery, and MTF
  stepping are removed. Their modules, feature flags, mutation exclusions,
  contract exams, live tests, seal-rate reports, and configuration are removed
  with them; no compatibility shim or deprecated copy remains.
- **Virtual time is an assigned exit accumulator.** Each modeled VM-exit class
  contributes its integer `vns` duration to `VClock`; idle jumps use the same
  accumulator. Snapshots carry the assigned V-time, guest counter base/rate,
  and entropy state. There is no branch ratio, live host counter read, or
  backend re-arm on restore.
- **Scheduled host events are exit-boundary events.** Normal execution calls
  only `Backend::run`. A scheduled `Moment` is applied when an exit boundary
  lands on it; crossing it fails loudly as `ScheduleUnsatisfiable`. The retired
  prospective exact-stop error and its wire tag are removed; application
  protocol version 9 remains reserved because version numbers are monotonic.
- **Apply N0's patch decision literally.** KVM patches 0004 (force exit) and
  0005 (MTF exact stepping) are deleted. Instruction-tripwire patches 0001–0003
  remain for N6. The guest pvclock/clockevent patches survive with their
  narrower exit-count-derived names and contracts.
- **Keep historical evidence verbatim.** The two historical status ledgers,
  the N2 plan itself, old decision links, and the KVM patch results ledger are
  the only intentional uses of the retired qualifier or machinery language.
  Git history is the archive for implementation material.
- **Do not rewrite a frozen v1 log identity.** The N1 normalized fixtures begin
  with versioned `consonance.*-prescriptive-log.v1` format/domain tokens. Those
  literal tokens remain solely as historical wire-format records: changing
  them would change both normalized bytes and their chained digests, directly
  violating §2. No Rust/Python identifier, file, workflow, or current prose
  uses the retired qualifier; new behavior and artifact names say virtual
  time. A future format rename would require a new version and a separately
  approved rebaseline, which this milestone forbids.
- **Preserve the post-doorbell seal boundary.** Exit-count V-time makes every
  serviced exit exact, but `setup_complete` still leaves a userspace-I/O
  completion for the next guest entry to commit. A transient control latch
  therefore defers the SDK snapshot point through exactly that successful
  re-entry. This retains N1 session segmentation without retaining a work
  counter, exact-stop backend path, or snapshot-visible compatibility state.

### Issue sweep

The following issues were closed as not planned with the required one-line N2
rationale; issues #199–#201 remain open because their image-audit/intercept
work is still part of N6.

| Issue | Recorded closure rationale |
| --- | --- |
| [#170](https://github.com/pH14/harmony/issues/170) | `Closed as not planned by N2: exit-boundary virtual time removed the exact-arrival arming and pvclock re-anchor path.` |
| [#174](https://github.com/pH14/harmony/issues/174) | `Closed as not planned by N2: exit-count virtual time removed the AMD force-exit productionization path.` |
| [#179](https://github.com/pH14/harmony/issues/179) | `Closed as not planned by N2: exit-count virtual time removed the PMU work-clock and skid requalification program.` |
| [#180](https://github.com/pH14/harmony/issues/180) | `Closed as not planned by N2: exit-count virtual time supersedes the AMD rr-parity PMU work-clock program.` |
| [#196](https://github.com/pH14/harmony/issues/196) | `Closed as not planned by N2: exit-count virtual time removed the single-step fallback and exact-landing path.` |

### Exact-tree repository and surface verification

```text
cargo build --workspace --all-features
PASS

cargo nextest run --workspace --all-features --no-fail-fast
1161 passed, 25 skipped

cargo clippy --workspace --all-features --all-targets -- -D warnings
PASS (the standing three invalid-path notices from clippy.toml remain parser
warnings, not crate diagnostics)

cargo fmt --all -- --check
PASS

cargo deny check
advisories ok, bans ok, licenses ok, sources ok

cargo test --manifest-path dissonance/Cargo.toml -p machine \
  --test control_loopback
1 passed
```

The first nextest invocation ran inside a filesystem-only sandbox and its
seven telemetry listener tests received `Operation not permitted`. The exact
complete command rerun with localhost-socket permission passed all 1,161
tests, matching N0's already-recorded execution-environment distinction.

The removed public items (`Backend::run_until`, deadline exit/counters,
branch-ratio state, the exact-stop control error, and ARM's raw branch event)
and the renamed pvclock flag are reflected in the regenerated public-API
snapshots. Every portable guard and the SDK guard pass locally. On msr1,
`cargo-public-api 0.52.0` regenerated and checked all 14 crate snapshots,
including the Linux-only `vmm-backend` and `vmm-core` surfaces, at exact commit
`60d70599db3f84871033d55c1bbb45cbc1cd17ca`.

Two CI-only repairs were required before accepting those surfaces. The x86
guest patch series had malformed hunk counts that `git apply --check` correctly
rejected; commit `b8e37f9f` repairs only the hunk metadata. Regeneration then
exposed the intended portable and Linux API removals, committed at `1939ffee`
and `60d70599`. The RDTSC audit also caught ten shifted
`.altinstr_replacement` offsets after the guest rebuild; `14bf4a63` rebases the
ten allowlist entries one-for-one rather than relaxing the check. GitHub's
secret scan for the final tree passed as run `33183121360`.

### Unsafe-crate verification

Pinned Miri (`nightly-2026-06-16`,
`MIRIFLAGS=-Zmiri-permissive-provenance`; snapshot-store additionally uses
`-Zmiri-disable-isolation`) completed with zero failures for
`hypercall-doorbell`, `vm-state`, `vmm-backend --all-features`,
`snapshot-store --lib`, `play-agent`, `tetanes-agent`, and `vmm-core`.
`vmm-backend` passed 48 unit/integration tests, including all 16 run-loop tests.
`vmm-core` passed 356 unit tests (99 intentional host-only ignores) and every
applicable integration suite: 21 ARM skeleton, 18 event-loop, loader,
protocol, snapshot, and all 12 virtual-time tests.

The long `vmm-core` run was made at implementation commit `696d70a5`; the only
later changes are test-fixture repairs, guest patch metadata/allowlist offsets,
and generated API snapshots. `git diff 696d70a5..60d70599 --
consonance/vmm-core/src consonance/vmm-backend/src` is empty. Thus this is not
described as a byte-for-byte final-SHA Miri invocation: it is a complete Miri
run of the exact final unsafe implementation, with the final fixture and
generated-surface changes covered by the native exact-tree suite above.

### N1 reference reruns on the N2 tree

The ARM boot reference used the same attested Image and initramfs as N1. Ten
signed-HVF boots and ten CPU-0-pinned msr1 KVM boots each produced 38,453
portable events, 283 schedules, 136 deliveries, and 151 checkpoints. Every
placement check passed. All twenty runs retained normalized digest
`e2e7852e…829`, final state hash `1dc0c1da…b17`, and the complete
5,954,217-byte log SHA-256 `4b4e7a27…7db`, byte-identical to N1 and across
backends. The rebuilt HVF executable was signed with the committed
`hvf.entitlements.plist`; attempting to run the unsigned rebuild correctly
failed at VM creation and was not counted.

The same one-job/two-session NES campaign passed on signed HVF and pinned KVM.
Its complete session logs remained byte-identical across both machines and N1
(SHA-256 `a801bfb0…d45` and `ffdd9dbd…eda`). The session metrics and end states
also remained unchanged:

| Session | Segments | Events | Schedules | Checkpoints | Session digest | State hash |
| --- | ---: | ---: | ---: | ---: | --- | --- |
| 0 | 3 | 50,931 | 393 | 198 | `0c4936c4…478` | `741e95a2…4d8` |
| 1 | 4 | 50,934 | 393 | 198 | `43105757…d21` | `de72c909…e58` |

The independently produced archive, report, stream, and snapshot artifacts
matched at `384d3029…6b2`, `b3032b4a…c46`, `584e0d3a…8b2`, and
`ae8d699c…b783`. All placement checks passed. The exact final KVM campaign is
recorded under `/root/harmony-n2-kvm-campaign-final-60d70599`; the signed-HVF
rerun used the same final runtime implementation (subsequent commits affect
only fixtures, allowlists, and generated snapshots).

GitHub Actions run
[`33183121337`](https://github.com/pH14/harmony/actions/runs/33183121337)
ran exact final commit `60d70599` through the stock-KVM X-series workflow.
The check, guest build, both X1 jobs, all six probes, all four X2 vendor-pool
replicas, and all eight bounded Intel-hunt replicas passed. Each X2 replica
completed ten equal boots with 35,310 events and `X2_DIVERGENCES=0`; the
sampled pool transcript has digest `79cb885a…51f3`, zero component differences,
zero RAM page differences, and every placement check green. This is a new
exit-count log identity, so equality across the final N2 vendor pool—not the
retired N1 branch-clock digest—is the oracle.

The exact final planted negative also passed:

```text
cargo test -p vmm-core --test virtual_time \
  comparator_rejects_one_vns_increment_at_the_exact_event -- --exact
PASS: the comparator rejects the planted +1 V-ns at its exact event
```

Searches over the non-historical tree find no retired modules, symbols,
feature flags, patch names, or file names. A case-insensitive filename search
for the retired qualifier returns only `docs/PRESCRIPTIVE-VTIME-STATUS.md`.
Text matches outside the plan/status ledgers are links to that historical
record or the seven occurrences of frozen v1 log/event tokens in the encoder,
dumpers, and byte-comparison oracle described above. The workflow display name
and concurrency group are `x86-virtual-time`.

**N2 overall: PASS.** Commit `60d70599` contains one exit-count virtual-time
clock, no retired branch-count implementation or compatibility half-state,
regenerated frozen surfaces, the recorded issue sweep, exact-tree repository
gates, and all three machine references. The preserved v1 format literals and
historical ledgers are the only search matches and are required frozen records,
not orphaned implementation.

## N3 — fast

### Pre-optimization measurement and correctness findings

**Baseline recorded before optimization.** No performance implementation change
has landed. The exact pre-optimization runner at `9440f1f2` was profiled on the M1
Max with the attested kernel (`91b4f578…a72f`) and reproducibly built initramfs
(`c3939c77…66ab`). A 30-second `sample(1)` profile collected 22,558 main-thread
samples:

| Cost | Samples | Share of main-thread samples |
| --- | ---: | ---: |
| `sha2::sha256::compress256`, reached from `Vmm::step` checkpoint `state_hash` | 22,332 | 99.00% |
| `state_hash` memory copy (`_platform_memmove`) | 180 | 0.80% |
| HVF trap/run | 5 | 0.02% |

Thus the measured top cost is the 512 MiB full-state SHA-256 at every 256th
portable event; rendering, stdout, watchdog traffic, and HVF execution are not
material suspects. This profile is recorded before any optimization, as N3
requires.

The original fixture could not produce an admissible baseline because its
structured oracle was written through asynchronous `/dev/kmsg`; rows 4–8,
19–20, and terminal markers could be absent even though PostgreSQL's aggregate
proved all rows committed. [Issue #206](https://github.com/pH14/harmony/issues/206)
records that defect. Commits `77d37a0e`, `ddcd0865`, and `a9c83d93` make the
oracle transport synchronous and keep the init within the image's explicit
BusyBox applet surface. These are pre-baseline correctness repairs, not
performance optimizations. The rebuilt fixture passed the LL/SC, raw
host-register, and timer-program instruction scans for every shipped ELF.

The final repaired run reached `ARM64_PG_M3_READY`, passed the 20-row SQL,
kernel-health, delivery-placement, watchdog, and gap-bound oracles, and supplied
the following provisional phase timings. They are not accepted as the N3
baseline because the independent comparators failed:

| Phase | Wall ns | Host loop exits |
| --- | ---: | ---: |
| boot to PostgreSQL start | 119,573,586,292 | 20,084 |
| PostgreSQL startup | 52,102,331,917 | 10,080 |
| ready to workload | 13,797,858,250 | 2,836 |
| workload | 30,598,263,250 | 5,771 |
| PostgreSQL shutdown | 9,213,251,250 | 1,650 |
| kernel health | 4,582,470,833 | 857 |
| total | 229,867,761,792 | 41,278 |

The blocking mismatch is now exact:

```text
raw HVF exits / host loop iterations: 41,278
portable normalized events:           38,295
substrate-private difference:           2,983

trace gaps:    count=30,959 max=1,000,000 V-ns
pvclock gaps:  count=33,796 max=1,000,000 V-ns
```

ARM normalization intentionally records HVF's userspace-GIC MMIO/sysreg exits
as raw-only because stock KVM consumes the same operations in its in-kernel GIC.
The M3 runner nevertheless compares all host loop iterations with only portable
events, and samples the unchanged pvclock page again on each raw-only exit. The
result is a category mismatch in both independent comparators, exposed rather
than caused by the synchronous transport. [Issue #207](https://github.com/pH14/harmony/issues/207)
records the exact counts and the six invariants plus two planted negatives an
authorized repair must satisfy.

### Comparator resolution and accepted baseline

The integrator explicitly authorized issue #207's six-invariant repair.
Commit `63980ca5` adds a structural disposition to every raw event: exactly one
portable ordinal or `None` for a substrate-private exit. The M3 runner checks
that raw ordinals are contiguous, portable ordinals are contiguous, the two
dispositions partition every host-loop exit, and the portable count equals the
normalized trace. Pvclock samples are retained at portable boundaries only;
two equal samples are deliberately retained when a legitimate portable event
advances zero time. Commit `9440f1f2` cfg-gates those host-runner helpers on
non-macOS builds without changing behavior.

The focused gate passes on macOS and Linux:

```text
positive partition: raw=3 portable=2 substrate-private=1 — PASS
planted dropped portable event — REJECTED
planted private-to-portable disposition — REJECTED
zero-time portable event around a private exit — RETAINED
LiveVirtualTimeTrace private then portable ordinals — PASS
Linux frozen public API — PASS
macOS and Linux vmm-core all-target Clippy -D warnings — PASS
vmm-core library — 453 passed, 2 ignored
```

The exact `9440f1f2` run then produced a complete `status PASS` report. Report
SHA-256 is `1a282d34…c0a5`; the 30-second profile SHA-256 is
`7ecbd33e…b0fd`. All acceptance, watchdog, kernel-health, placement, gap, and
independent-comparator oracles passed:

```text
raw/event-loop exits: 41,278 / 41,278
portable events:      38,295
substrate-private:     2,983
trace/pvclock gaps:    30,959 / 30,959
max gap:               1,000,000 V-ns (20,000,000 limit)
trace digest:          84181418…c5ab8
```

This is the accepted pre-optimization phase table and ≥10× denominator:

| Phase | Wall ns | Host loop exits |
| --- | ---: | ---: |
| boot to PostgreSQL start | 119,338,860,708 | 20,084 |
| PostgreSQL startup | 52,292,723,750 | 10,080 |
| ready to workload | 13,779,767,083 | 2,836 |
| workload | 30,485,590,709 | 5,771 |
| PostgreSQL shutdown | 9,181,723,708 | 1,650 |
| kernel health | 4,562,046,167 | 857 |
| **total** | **229,640,712,125** | **41,278** |

No performance optimization predates this committed baseline and profile.

### Optimization and fail-capable audit

Commit `88b0b7fe` moves only the sparse checkpoint digest off the event-loop
critical path. `Vmm` still creates the checkpoint at the exact portable-event
boundary, but an explicitly enabled host-runner mode returns the canonical
owned state blob and leaves that checkpoint's hash slot empty. The M3 runner
submits the blob to a bounded eight-worker pool and installs completed digests
back at their original event indices before accepting the trace. The total
wall measurement includes draining and joining all workers; work cannot be
hidden outside the denominator.

The normal `Vmm` path remains synchronous. Deferred mode must be enabled before
the trace starts, accepts only exact 256-event boundaries, rejects duplicate or
nonexistent checkpoints, and cannot be used without a virtual-time trace. The
frozen public API snapshot records the two deliberately exposed controls,
`defer_virtual_time_checkpoint_hashes` and
`checkpoint_virtual_time_trace_at`. The runner's independent audit requires
all 149 expected checkpoints and no others. Its planted missing-checkpoint and
stray-checkpoint mutants are both rejected, in addition to the two exit-stream
partition mutants recorded above, so a green performance report cannot omit
hash work or compare a shortened stream.

The same commit adds `scripts/benchmark-vtime-m3.sh`. The harness pins the
scenario, attests both guest images, builds the release runner, requires every
acceptance and comparator line, requires 149 digests produced by eight workers,
and checks the frozen portable trace digest before reporting throughput. It is
an M1 Max measurement tool, not a heterogeneous CI timing gate.

The first implementation enabled `sha2`'s assembly feature for the macOS
runner. Cargo feature unification made that feature reachable in Linux and
Miri builds, which violated the intended host isolation even though the live
reference output remained exact. Its x86 workflow run `33194896286` therefore
failed in the repository-check job because `CheckpointPool` was not exercised
on Linux; all X1/X2 live determinism jobs and both vendor hunts themselves
passed. This caused failure is not waived. Corrective optimization commit
`7cbdf6df` removes `sha2-asm` and its lockfile entry entirely, exercises the
pool on every host, and confines the accelerated one-shot digest to macOS
CommonCrypto outside Miri. The small FFI boundary has an adjacent safety
argument and parity tests against portable `sha2` at lengths 0, 1, 55, 56, 63,
64, 65, 127, 128, 129, and 1,048,579 bytes. Miri selects the portable function,
so the new unsafe boundary does not create an uninterpretable test path.

### Measured result and post-optimization profile

The exact `7cbdf6df` benchmark report is
`/private/tmp/harmony-n3-benchmark-commoncrypto.report` (SHA-256
`d3a440e3…2504`). It passes every workload, health, placement, gap, checkpoint,
and independent exit-count oracle. The portable trace remains 38,295 events
with digest `84181418…c5ab8`, partitioned from 41,278 raw exits into 38,295
portable and 2,983 substrate-private exits. Its total is 4,956,055,334 ns, or
8,328.801 exits/second and **46.335×** faster than the accepted
229,640,712,125 ns baseline. This is below the required 22,964,071,212 ns
ceiling by more than 4.6×.

| Phase | Before wall ns | After wall ns | Host loop exits |
| --- | ---: | ---: | ---: |
| boot to PostgreSQL start | 119,338,860,708 | 2,502,447,250 | 20,084 |
| PostgreSQL startup | 52,292,723,750 | 1,067,159,334 | 10,080 |
| ready to workload | 13,779,767,083 | 273,870,083 | 2,836 |
| workload | 30,485,590,709 | 578,796,000 | 5,771 |
| PostgreSQL shutdown | 9,181,723,708 | 150,070,583 | 1,650 |
| kernel health | 4,562,046,167 | 60,989,917 | 857 |
| **total** | **229,640,712,125** | **4,956,055,334** | **41,278** |

The independent post-change profile run completed in 5,849,533,542 ns. Its
report SHA-256 is `19286da4…448a`; the `sample(1)` capture SHA-256 is
`87dd0258…ab7d`. Of 2,507 main-thread samples, 1,416 (56.5%) are now the
canonical state-blob copy, 333 (13.3%) are serial scanning, and 127 (5.1%) are
the HVF trap. Each of the eight workers spends 2,239–2,416 samples in
`CC_SHA256`. The former 99% synchronous SHA-256 main-thread bottleneck is gone;
the remaining largest serial cost is the unavoidable owned-state copy that
makes concurrent hashing race-free.

### Per-optimization reference reruns

The following evidence is tied to each optimization commit rather than only to
the final tree:

- `88b0b7fe`: ten signed-HVF boots on the M1 Max and ten CPU0-pinned KVM boots
  on msr1 independently produced normalized-log SHA-256
  `4b4e7a27…7db`, 38,453 portable events, 283 schedules, 136 deliveries, 151
  checkpoints, placement PASS, trace digest `e2e7852e…9829`, and terminal
  state hash `1dc0c1da…8b17`. The one-job/two-session NES campaign then matched
  across HVF (`/private/tmp/harmony-n3-hvf-campaign-88b0b7fe-v2`) and pinned
  KVM (`/root/harmony-n3-kvm-campaign-88b0b7fe-v2`): session logs
  `a801bfb0…9d45` and `ffdd9dbd…6eda`; archive `384d3029…f6b2`; report
  `b3032b4a…6c46`; stream `584e0d3a…78b2`; snapshots `ae8d699c…b783`.
  The corrected KVM run deliberately used the canonical recorded host token;
  an earlier diagnostic run used `msr1`, which left the VM logs exact but
  changed host-bearing JSON and was rejected rather than counted. GitHub run
  `33194896286` passed every x86 live reference but failed the Linux dead-code
  check described above, motivating the immediately following corrective
  commit.
- `7cbdf6df`: ten signed-HVF boots again produced the same
  `4b4e7a27…7db` normalized log and the same event, schedule, delivery,
  checkpoint, placement, trace-digest, and terminal-state values. GitHub x86
  run `33195869000` passed the portable check, six probes, both X1 minimal
  guests, all eight vendor hunts, and all four full ten-boot X2 jobs. Ten
  CPU0-pinned msr1 boots (`/root/harmony-n3-kvm-boot-7cbdf6df`) independently
  reproduced the same normalized log and every listed oracle value. The final
  NES campaign matched across HVF
  (`/private/tmp/harmony-n3-hvf-campaign-7cbdf6df`) and pinned KVM
  (`/root/harmony-n3-kvm-campaign-7cbdf6df`), reproducing the two session and
  four portable-artifact hashes listed above.
- `f56930ef`: the canonical aarch64-Linux public-API guard exposed one
  generated-snapshot ordering defect in the otherwise exact final tree. The
  correction moves `checkpoint_virtual_time_trace_at` to the generator's
  canonical position and changes no executable behavior. Because it changes
  the exact commit, the full reference evidence was rerun rather than
  inherited. Ten signed-HVF boots
  (`/private/tmp/harmony-n3-hvf-boot-f56930ef`) and ten CPU0-pinned KVM boots
  (`/root/harmony-n3-kvm-boot-f56930ef`) again produced normalized-log
  SHA-256 `4b4e7a27…7db`, 38,453 portable events, 283 schedules, 136
  deliveries, 151 checkpoints, placement PASS, trace digest
  `e2e7852e…9829`, and terminal state hash `1dc0c1da…8b17`. The HVF and
  KVM NES campaigns (`/private/tmp/harmony-n3-hvf-campaign-f56930ef` and
  `/root/harmony-n3-kvm-campaign-f56930ef`) reproduced session hashes
  `a801bfb0…9d45` and `ffdd9dbd…6eda`, archive `384d3029…f6b2`, report
  `b3032b4a…6c46`, stream `584e0d3a…78b2`, and snapshots
  `ae8d699c…b783`. GitHub x86 run `33198943599` passed the portable check,
  six probes, both X1 minimal guests, all eight vendor hunts, and all four
  full ten-boot X2 jobs. The canonical Linux `vmm-core` and `vmm-backend`
  public-API guards also passed on this commit.

The exact final local code tree passes `cargo build --all-features`, all 1,171
tests under `cargo nextest run --all-features`, all-target Clippy with warnings
denied (apart from the standing unreachable-entry parser notices emitted by
`clippy.toml`), formatting, and `cargo deny check`. All instrumented tests also
pass under `cargo llvm-cov`; its macOS aggregate is 87.15% because Linux-only
paths are absent, so it is recorded as portability evidence rather than
misrepresented as the repository's authoritative Linux 90% floor. The
canonical Linux public-API guards pass for both affected crates. Exact-commit
pinned-nightly Miri run `33198957494` completed successfully: the dedicated
`vmm-core` job passed, as did the companion chain for `hypercall-doorbell`,
`vm-state`, `vmm-backend`, `snapshot-store`, `harmony-linux/play-agent`, and
`harmony-linux/tetanes-agent`.

The accepted baseline was committed and profiled before either optimization;
the final 4,956,055,334 ns result is **46.335×** faster than the
229,640,712,125 ns baseline, preserves every frozen boot and campaign byte on
all three machines, and every optimization commit has its own reference rerun
listed above. **N3 overall: PASS.**

## N4 — the guest is part of Consonance

Not started.

## N5 — reproducible guest builds

Not started.

## N6 — defenses tested by attacking them

Not started.
