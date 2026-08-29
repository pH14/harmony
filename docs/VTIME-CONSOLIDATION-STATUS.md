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

**PASS** at exact implementation commit `4b62de3f`; later tree changes through
this status update are documentation-only. The final repository
gates, fresh moved-tree artifacts, corrected-tree live references, x86
guest/KUnit gate, both-vendor primary X2 references, and Miri evidence are
complete. A reference rerun on ancestor
`f2a24062` honestly exposed a guest-visible KVM MIDR leak; that run and its
internally deterministic halves are diagnostic only. The standing-rule issue
bookkeeping is complete as recorded below.

### Issue bookkeeping

- [#172](https://github.com/pH14/harmony/issues/172) was closed completed with
  the fixed-driver cold-build positive and planted no-lock negative evidence.
- [#211](https://github.com/pH14/harmony/issues/211) was closed completed with
  the portable terminal-whitespace repair, extra-suffix rejection control, and
  corrected KUnit workflow evidence.
- [#212](https://github.com/pH14/harmony/issues/212) records the vacuous prior
  host-side MIDR probe, first cross-backend divergence, repair, live in-guest
  `MRS MIDR_EL1`, Miri result, and final byte-equal reference.
- [#213](https://github.com/pH14/harmony/issues/213) records the mismatched x86
  hunt image-cache key, the failed-closed discovery run, one-line repair, and
  final both-vendor green references.

Issues #208, #209, and #210 were already closed with their successful repair
evidence. Thus every bug found during N4 has a durable GitHub issue record.

### Layout and doorbell decisions

- The final owned-guest path is `consonance/harmony-linux/`. Build entrypoints,
  workflow paths, active documentation, standalone agent dependency paths, and
  repository-root discovery all follow the move. The kernel patch workspace and
  its GPL-2.0 boundary are unchanged; first-party userspace remains
  AGPL-3.0-or-later.
- Issue [#172](https://github.com/pH14/harmony/issues/172) is closed in the
  design, not worked around in userspace. The x86 `/dev/harmony` driver now owns
  the fixed request/response mappings and exposes a raw-frame ioctl. One global
  kernel mutex covers staging, doorbell OUT, and response copyout. The play
  agent implements `hypercall_proto::Transport` over that ioctl and no longer
  maps or rings the shared pages directly.
- `linux/test-harmony-serialization.sh` builds and boots a KUnit positive, then
  removes the serialization helper from a fresh negative kernel and requires
  the concurrent-ringer test to fail. Thus the test cannot pass merely because
  both ringers happened not to overlap. The existing x86 guest-image workflow
  runs this gate; no new required workflow was added.

### Fresh moved-tree ARM artifacts

All four artifacts were built after the move from the checked-out
`consonance/harmony-linux/` sources. Both manifest checks reported every entry
`OK`:

| Artifact | SHA-256 |
| --- | --- |
| `Image` | `5b0b8fb8d13af2b1aa3ea3a312b3bafeaa690b9e2bd0935029942cf3995ff4d8` |
| `initramfs.cpio.gz` | `6194ec4be99b08e68a61f9020fcedd7aae515b00fa63d38a44b9070a23fea053` |
| `Image-game` | `8cd386f8fcc3a6010f47b39c0a6aae50dbacdde2d1e36529a6dc926c618ea116` |
| `initramfs-game.cpio.gz` | `9d762ec68b6827021b18208ddfcef3bffca8141065ec61966e4bf36c45988ecf` |

The embedded/host SMB ROM is independently pinned at
`0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`.
Fresh minimal and game kernel builds passed their planted counter and LL/SC
scanner negatives and every shipped-image scan. Fresh minimal and game
initramfs builds likewise passed every executable scan. Issues
[#208](https://github.com/pH14/harmony/issues/208) and
[#209](https://github.com/pH14/harmony/issues/209), found by those fresh
builds, were fixed in `3fa8f6f9` and `601c1a9f` and closed with the successful
rerun evidence.

### Exact-tree repository gates so far

On the M1 Max at exact implementation commit `f2a24062`:

```text
cargo build --all-features
PASS

cargo nextest run --all-features
1171 passed, 25 skipped

cargo clippy --all-features --all-targets -- -D warnings
PASS (standing three unreachable clippy.toml entries only)

cargo fmt --all -- --check
PASS

cargo deny check
advisories ok, bans ok, licenses ok, sources ok
```

The first nextest invocation had the already-characterized filesystem-sandbox
socket denial in two telemetry listener tests. The complete command rerun with
localhost socket permission passed all 1,171 tests. The full five-command gate
was rerun after the timestamp-regex and KVM identity repairs at `8312745e`; it
passed with the same standing Clippy and dependency-policy notices shown above.

The previously running exact-`d3e0eaee` nightly
[`33216717204`](https://github.com/pH14/harmony/actions/runs/33216717204)
was preserved to completion before the hardware-allocation plan edit was
committed or any later msr1 unit was launched. Its general unsafe-crate Miri
matrix passed in 25m31s, including `hypercall-doorbell`, `vm-state`,
`vmm-backend`, `snapshot-store`, and both moved-tree agents. The separate
`vmm-core` Miri job passed at `00:52:04Z` after 2h26m51s; its final
`virtual_time` suite reported 12 passed and zero failed, including the planted
one-V-ns, one-exit-late, and flipped-state-byte comparator negatives. Live-KVM
tests and the public-API regenerator remained explicitly ignored for their
documented host/tool requirements. The workflow concluded success.

At final implementation commit `4b62de3f`, the complete local repository gate
passed again: `cargo build --all-features`; `cargo nextest run --all-features`
with 1,171 passed and 25 skipped; Clippy for all features and targets with only
the standing three unreachable `clippy.toml` notices; formatting; and `cargo
deny check` with advisories, bans, licenses, and sources all `ok`. The first
nextest invocation was denied localhost sockets in the two telemetry-listener
tests and stopped fail-fast after 408 passes; the full permitted rerun is the
credited result. The first deny invocation likewise failed before checking
because the sandbox made Cargo's advisory-database lock read-only; the permitted
rerun is the credited result.

### x86 serialization-gate finding

Exact-tree x86 run
[`33212855686`](https://github.com/pH14/harmony/actions/runs/33212855686)
at `601c1a9f` passed the portable check, six KVM probes, and both X1 boots, but
the fresh guest job correctly failed before KUnit boot. The new kernel test had
declared a local named `current`, which x86 Linux expands to `get_current()`;
the compiler rejected the resulting non-prototype/conflicting declaration and
assignment. Issue [#210](https://github.com/pH14/harmony/issues/210) records the
failure. Signed commit `d3e0eaee` renames only that local to `active`. Exact
rerun `33215519428` then compiled and booted the fixed KUnit test successfully:
`harmony_concurrent_ringer_test` and the `harmony-transaction-lock` suite both
reported `ok`. The shell gate nevertheless rejected the pass because its regex
required `ok` at column zero while the serial console prepended the kernel
timestamp. Issue [#211](https://github.com/pH14/harmony/issues/211) records that
false negative. Signed commit `f2a24062` retained the exact suite/test suffixes
but admitted the timestamp prefix. Exact rerun
[`33217271391`](https://github.com/pH14/harmony/actions/runs/33217271391)
compiled and booted the fixed suite; the concurrent-ringer test and suite both
reported `ok`, but the gate still rejected the serial line because QEMU kept a
carriage return before newline. Commit `8312745e` admits that terminal `CR` in
both the positive and planted-negative expressions, but used `\r` in a POSIX
extended expression; `grep -E` interpreted it as the letter `r`. Exact run
[`33219335412`](https://github.com/pH14/harmony/actions/runs/33219335412)
therefore repeated the same false negative while the KUnit test and suite again
reported `ok`. Commit `8e6de5e0` uses an anchored `[[:space:]]*` suffix, which
admits CR and terminal whitespace but still rejects any non-whitespace suffix;
local positive, planted-negative, and extra-suffix rejection controls passed.
All four ancestor runs are failed honestly and are not counted. Exact corrected-
tree run
[`33220706514`](https://github.com/pH14/harmony/actions/runs/33220706514)
rebuilt the guest from a cold image/source cache and passed the fixed-driver
positive at `23:51:29Z`, rejected the pre-fix no-lock mutant at `00:00:22Z`,
and printed `PASS: /dev/harmony serialization positive and negative controls`.
The portable check, six X1 probes, two X1 live oracles, and all four primary X2
same-seed boot/determinism replicas also passed.

The workflow nevertheless concluded red because all eight auxiliary
`x2-intel-hunt` replicas failed before installing tools or running a guest.
Their image-cache `hashFiles(...)` list omits
`linux/test-harmony-serialization.sh`, while both the producing `guest-image`
job and primary X2 consumers include it; each hunt replica therefore requested
key `x86-guest-image-8c8fb7…57a` instead of the produced
`x86-guest-image-a84a82…b8a` and failed closed on the cache miss. This is a
workflow-input bug exposed by the N4 serialization test, not a guest or
determinism failure. The required GitHub issue creation was attempted, but the
external-write guard requires explicit user authorization to publish the run,
commit, and diagnostic details; no retry is made pending that authorization.
Signed commit `4b62de3f` adds the omitted input and makes all three cache-key
lists byte-identical. Exact rerun
[`33224950144`](https://github.com/pH14/harmony/actions/runs/33224950144)
then passed every job: the cold guest/KUnit positive and negative, portable
check, six probes, two X1 oracles, four primary X2 jobs, and all eight repaired
hunt jobs. Each primary X2 job completed ten same-seed boots with 35,310 events,
digest `cd8a372f…b8721`, zero divergences, zero component diffs, and zero RAM
diff pages. That draw sampled only AMD (EPYC 7763 and 9V74), so it is retained
as the exact-tree AMD half rather than misrepresented as both-vendor evidence;
manual exact-tree reroll `33225893480` also passed all four primary jobs with
the same event/digest/zero-divergence result, but again sampled only AMD (EPYC
7763, 9V74, and 9V45). It is retained as additional AMD coverage. Manual
exact-tree reroll
[`33226361588`](https://github.com/pH14/harmony/actions/runs/33226361588)
then passed the whole workflow and supplied the missing primary Intel draw.
Primary 2 ran on an Intel Xeon Platinum 8370C; primaries 1, 3, and 4 ran on AMD
EPYC 9V74 and 7763 hosts. Each completed ten same-seed boots with 35,310
events, digest `cd8a372f…b8721`, zero divergences, zero component differences,
and zero RAM-diff pages. This exact `4b62de3f` pair of primary vendor pools is
the credited x86 reference.

### Cross-backend identity finding

The exact `f2a24062` halves were each internally deterministic but not equal to
one another:

- ten msr1 KVM boots: 38,342 portable events, 283 schedules, 136 deliveries,
  150 checkpoints, digest `88f14983…2c43`, final state hash
  `3177e9…3983`, and full normalized-log SHA-256 `7e252014…e0f` on every run;
- ten signed-HVF M1 Max boots: 38,453 portable events, 283 schedules, 136
  deliveries, 151 checkpoints, digest `a4a34879…8b77`, final state hash
  `8dea84…c29cd` on every run. The fresh minimal image and initramfs hashes
  matched the manifest above, and the oracle reported zero watchdogs.

The first event divergence is serial event 157. Linux printed
`0x410fd811` on HVF but `0x410fd801` on KVM; the first checkpoint at event 255
already differed (`6c8be503…32b0` versus `0df857b9…925c`). The prior live
KVM identity gate was vacuous for this register: it accepted a
`KVM_SET_ONE_REG`/`KVM_GET_ONE_REG` round trip of the boot-CPU reset value, but
never executed `MRS MIDR_EL1` in the guest. Workload cores `2-5` could therefore
expose a different physical MIDR.

Signed commit `8312745e` enables
`KVM_CAP_ARM_WRITABLE_IMP_ID_REGS` on the VM before vCPU creation and fails
closed if the capability is absent or its enabling ioctl fails. The live
identity probe now executes `MRS MIDR_EL1`, exports the value through an MMIO
exit, and requires the frozen `0x410fd811` baseline before retaining the
existing in-guest DCZID probe. Portable tests, native and aarch64-linux cross
checks, and aarch64-linux Clippy pass; live msr1 proof and corrected-tree
cross-backend equality were then proved by the final ordered reference below.

The exact `8312745e` unsafe-crate gate for the changed crate also passed:
`MIRIFLAGS=-Zmiri-permissive-provenance cargo +nightly-2026-06-16 miri test
-p vmm-backend --all-features` completed 48 unit, three contract, two dynamic,
two exhaustive, 16 run-loop, and one vCPU-state test with zero failures (the
host-only live binaries contain zero Miri tests and the public-API regenerator
remains deliberately ignored).

The final ordered `4b62de3f` boot reference closes the identity finding. After
the complete ancestor Miri wait and separate plan-only commit, the live msr1
probe executed `MRS MIDR_EL1` and observed `0x410fd811`, then ten canonical-
protocol KVM boots and ten signed-HVF M1 Max boots independently produced
38,453 portable events, 283 schedules, 136 deliveries, and 151 checkpoints.
Every placement check passed; every run had digest `a4a34879…8b77`, final state
hash `8dea84…c29cd`, and the same complete normalized-log SHA-256
`da267950…0ad4`. Both hosts rechecked the fresh moved-tree minimal artifact
hashes (`Image` `5b0b8f…f4d8`, initramfs `6194ec…053`), the KVM host rechecked
both committed manifests, and the M1 oracle reported zero watchdogs. Two local
preflight attempts before the signed run are non-evidence: the first named a
missing ignored artifact path, and the next two unsigned executions failed at
`hv_vm_create` with `HV_DENIED`; no guest ran in any of them. Applying the
repository's established ad-hoc Hypervisor entitlement produced the successful
ten-run reference and restored the exact signed binary hash
`73096f53…747e956`.

The final ordered `4b62de3f` one-job/two-session NES campaign also passed on
signed HVF and canonical-protocol KVM. Session 0 matched at three segments,
50,931 events, 393 schedules, 198 checkpoints, trace digest
`7a6b14a1…ea512c`, normalized digest `5cbb8b13…f25c7`, and state hash
`d5f7fcd3…caa2e`; session 1 matched at four segments, 50,934 events, 393
schedules, 198 checkpoints, trace digest `310cfef0…24d99`, normalized digest
`828a8802…77af`, and state hash `ce5a4e93…26ce9`. Both placements passed.
The portable archive, report, snapshots, stream, RAM images, virtual-time logs,
and searcher output were byte-identical across hosts; host/backend-specific
vCPU encodings and wall-throughput/progress/server text were retained and not
misrepresented as portable bytes.

### msr1 isolation ledger

Every listed unit requested `AllowedCPUs=2-5`, observed
`Cpus_allowed_list: 2-5`, set `CARGO_BUILD_JOBS=4`, and held the shared
`/run/lock/harmony-msr1-benchmark.lock` for its command lifetime. Units through
the grandfathered `8312745e` boot used explicit nonblocking lock probes where
shown. Canonical Dissonance-v4 units instead preserve and attest systemd's
single nested-flock ExecStart, so their payload-start timestamp is the proof
that both locks were held. No command used CPUs `0,1,6-11`.

| Unit | Result and lock state |
| --- | --- |
| `harmony-n4-checkout-7e05556e` | exact source worktree created; shared lock acquired immediately |
| `harmony-n4-audit-7e05556e` | allocation/cgroup audit passed; shared lock acquired immediately |
| `harmony-n4-arm-input-7e05556e` | pinned Linux input fetched and SHA-256 verified; shared lock acquired immediately; curl retried one HTTP/2 transport failure |
| `harmony-n4-input-audit-7e05556e` | input audit passed; shared lock acquired immediately |
| `harmony-n4-arm-image-7e05556e` | **invalid**: malformed ARM patch exposed issue #208; wrapper also failed to propagate the earlier `make` error |
| `harmony-n4-update-3fa8f6f9` | exact repair checkout; shared lock acquired immediately |
| `harmony-n4-arm-image-3fa8f6f9` | fresh minimal kernel and all scanners passed; initramfs verifier then exposed issue #209, so the combined unit failed honestly |
| `harmony-n4-arm-initramfs-601c1a9f` | **invalid before build**: preflight contained an incorrect expected full hash |
| `harmony-n4-arm-initramfs-601c1a9f-r2` | fresh minimal initramfs and manifest passed |
| `harmony-n4-game-audit-601c1a9f` | located ROM and prior harnesses; shared lock acquired immediately |
| `harmony-n4-game-build-601c1a9f` | fresh game kernel and scanners passed; combined unit then stopped because the exact worktree lacked the pinned BusyBox tarball |
| `harmony-n4-input-locate-601c1a9f` | located prior hash-pinned BusyBox/musl inputs under the shared lock |
| `harmony-n4-game-initramfs-601c1a9f-r2` | fresh game initramfs, exact KVM binaries, and game manifest passed |
| `harmony-n4-kvm-boot-601c1a9f` | **invalidated and stopped after two equal boots** when #210 advanced the exact tree |
| `harmony-n4-kvm-boot-d3e0eaee` | **invalid before compute**: full-SHA/short-SHA preflight mismatch; its shared lock waited 11.539 seconds for a Dissonance reservation before running |
| `harmony-n4-kvm-boot-d3e0eaee-r2` | **invalid while waiting**: read-only inspection found malformed nested shell quoting; stopped before lock acquisition or compute |
| `harmony-n4-kvm-boot-d3e0eaee-r3` | preserved under the original shared-lock rule; waited from 22:22:53 to 22:28:47 UTC for Dissonance's exclusive reservation, then ran on requested/observed CPUs `2-5`; it held no Consonance-compute lock and overlapped Dissonance's loaded E00 arm, invalidating evaluator-v3 after one valid idle sample; it completed ten byte-equal ancestor-tree boots at 22:39:08 UTC but is not final N4 evidence |
| `harmony-n4-kvm-boot-f2a24062-final` | **invalid before compute**: acquired compute exclusive at `22:40:13.466792951Z`, then benchmark shared at `22:40:13.472866130Z`; PID `4101674`, cgroup `/system.slice/harmony-n4-kvm-boot-f2a24062-final.service`, requested/observed `AllowedCPUs=2-5`, `EffectiveCPUs=2-5`, and `Cpus_allowed_list: 2-5`, with both lock probes blocking as required; systemd's restricted `PATH` then made `cargo` unavailable before any build or guest execution |
| `harmony-n4-kvm-boot-f2a24062-final-r2` | exact `f2a24062` rerun with explicit tool path; acquired compute exclusive at `22:40:49.316839454Z`, then benchmark shared at `22:40:49.322903673Z`; PID `4101775`, argv `bash /root/harmony-n4-f2-final.sh`, cgroup `/system.slice/harmony-n4-kvm-boot-f2a24062-final-r2.service`, requested/observed `AllowedCPUs=2-5`, `EffectiveCPUs=2-5`, and `Cpus_allowed_list: 2-5`, Cargo jobs four, and both lock probes blocked as required; completed ten byte-equal KVM boots, but the cross-backend MIDR mismatch above invalidates them as final evidence |
| `harmony-n4-transfer-f2a24062` | artifact transfer under the compute protocol: compute exclusive acquired `22:52:27.805372238Z`, then benchmark shared `22:52:27.811185700Z`; PID `4107285`, requested/observed CPUs and cgroup were exact and both lock probes blocked; transferred the hash-verified moved-tree artifacts and KVM normalized log for local comparison |
| `harmony-n4-kvm-campaign-f2a24062` | diagnostic ancestor campaign: compute exclusive acquired `22:53:29.888838945Z`, then benchmark shared at `22:53:29.894714947Z`; PID `4107431`, argv `bash /root/harmony-n4-kvm-campaign-f2.sh`, cgroup `/system.slice/harmony-n4-kvm-campaign-f2a24062.service`, requested/observed `AllowedCPUs=2-5`, `EffectiveCPUs=2-5`, and `Cpus_allowed_list: 2-5`, Cargo jobs four, and both lock probes blocked; preserved and still running when `8312745e` advanced the tree, so it is diagnostic and non-creditable regardless of result; no later unit was launched while it held the compute lock |
| `harmony-n4-kvm-boot-8312745e-final` | exact corrected-tree identity probe and ten-boot rerun; requested/acquired compute exclusive at `23:13:03.912796378Z`/`23:13:03.918600628Z`, then requested/acquired benchmark shared at `23:13:03.921704793Z`/`23:13:03.927283223Z`; PID `4123193`, argv `/bin/bash /root/harmony-n4-8312745e-final.sh`, cgroup `/system.slice/harmony-n4-kvm-boot-8312745e-final.service`, requested/observed `AllowedCPUs=2-5`, `EffectiveCPUs=2-5`, and `Cpus_allowed_list: 2-5`, Cargo jobs four, and both lock probes blocked as required; live guest `MRS MIDR_EL1` returned the frozen `0x410fd811`; ten KVM logs were byte-identical and matched all ten M1 Max logs at SHA-256 `da267950…0ad4`, digest `a4a34879…8b77`, 38,453 portable events, 283 schedules, 136 deliveries, 151 checkpoints, and final state hash `8dea84…c29cd`; both moved-tree manifests passed; completed successfully at `23:24:20Z` |
| `harmony-n4-kvm-campaign-8312745e-final` | first Dissonance-v4 canonical waiter, queued at `23:21:12Z` while the preserved boot unit still held compute-exclusive; PID `4124733` argv was exactly `/usr/bin/flock --exclusive /run/lock/harmony-msr1-consonance-compute.lock /usr/bin/flock --shared /run/lock/harmony-msr1-benchmark.lock /bin/bash /root/harmony-n4-campaign-8312745e-payload.sh`; cgroup `/consonance.slice/harmony-n4-kvm-campaign-8312745e-final.service`, declared/observed `AllowedCPUs=2-5`, `EffectiveCPUs=2-5`, and `Cpus_allowed_list: 2-5`; journal contained no payload output while the outer flock waited, proving no build or guest started before both locks; payload began at `23:24:20.809809255Z` only after the preserved unit released compute, and re-attested the slice, exact CPU set, canonical ExecStart, and Cargo jobs four; completed successfully at `23:33:52Z` with two sessions and `N4_ONE_JOB_CAMPAIGN_OK backend=kvm`; archive, report, snapshots, stream, and both virtual-time-log hashes matched the signed-HVF run (`384d3029…4b6b2`, `a6cbcb3b…046c6`, `aec881c6…a2cd`, `243ecc51…571c`, `ff49389b…1879`, `0d2c54fe…d310`); backend-specific live progress, throughput, RAM/vCPU dumps, and server output differed and are retained rather than falsely normalized; this remains diagnostic rather than final ordered N4 evidence because it was launched before the complete ancestor Miri wait finished |
| `harmony-n4-kvm-boot-4b62de3f-final` | final ordered exact-tree boot reference after the successful Miri wait and signed plan-only commit; outer waiter PID `4190784` started at `00:57:18Z` with exact argv `/usr/bin/flock --exclusive /run/lock/harmony-msr1-consonance-compute.lock /usr/bin/flock --shared /run/lock/harmony-msr1-benchmark.lock /bin/bash /root/harmony-n4-boot-4b62de3f-payload.sh`; cgroup `/consonance.slice/harmony-n4-kvm-boot-4b62de3f-final.service`, declared/observed `AllowedCPUs=2-5`, `EffectiveCPUs=2-5`, and `Cpus_allowed_list: 2-5`; payload PID `4190787` began at `00:57:18.703747567Z` only after both canonical locks; Cargo jobs four; live MIDR `0x410fd811` and DCZID `4`; ten logs byte-identical and equal to the final signed-HVF log at SHA-256 `da267950…0ad4`, digest `a4a34879…8b77`, state hash `8dea84…c29cd`, 38,453 events, 283 schedules, 136 deliveries, and 151 checkpoints; both moved-tree manifests passed; completed successfully at `01:07:47Z` |
| `harmony-n4-kvm-campaign-4b62de3f-final` | final ordered exact-tree campaign; outer waiter PID `4190853` started at `00:57:29Z` with exact canonical compute-exclusive then benchmark-shared argv and no payload output while the boot held compute; cgroup `/consonance.slice/harmony-n4-kvm-campaign-4b62de3f-final.service`, declared/observed `AllowedCPUs=2-5`, `EffectiveCPUs=2-5`, and `Cpus_allowed_list: 2-5`; payload PID `6882` began at `01:07:47.681275543Z` only after the boot released compute and re-attested the exact slice, argv, CPU set, and Cargo jobs four; completed successfully at `01:17:03Z` with two sessions and `N4_ONE_JOB_CAMPAIGN_OK backend=kvm`; archive, report, snapshots, stream, both RAM images, both virtual-time logs, and searcher output matched the final signed-HVF run (`384d3029…4b6b2`, `a6cbcb3b…046c6`, `aec881c6…a2cd`, `243ecc51…571c`, `321d00c9…ba5`, `409e08a2…b6d`, `ff49389b…1879`, `0d2c54fe…d310`, `d4d03bed…1715`); both session trace digests, normalized digests, state hashes, and placements matched; backend-specific vCPU dumps, progress/throughput timing, and server text remain explicitly distinct |

The first attempted download command failed locally during shell parsing and
created no remote unit. The initial unrestricted SSH preflight only inspected
installed tools and systemd support; it launched no build, test, guest, Cargo,
or rustc process. All actual post-adoption compute and material I/O is accounted
for above. The `-r3` unit was deliberately allowed to wait; the exclusive
reservation was not bypassed.

The first `8312745e` systemd launch attempt split a description containing
spaces at the remote shell and failed with “executable exact not found”; it
created no unit, acquired no lock, and launched no compute. The immediately
corrected no-space description produced the recorded unit above.

### Hardware-coordination correction

The active Dissonance task reported that `harmony-n4-kvm-boot-d3e0eaee-r3`
held only the shared benchmark lock and overlapped its loaded E00 arm. Per the
correction, that already-running unit is preserved and not stopped. Every
subsequent Consonance compute unit uses exact CPUs `2-5` and acquires, in
order, `/run/lock/harmony-msr1-consonance-compute.lock` exclusive and then the
benchmark lock shared. Compute-first prevents a queued Consonance unit from
becoming another benchmark-lock holder before Dissonance's loaded arm. Its
evidence must retain PID, argv, cgroup, `AllowedCPUs`, `EffectiveCPUs`,
`Cpus_allowed_list`, and both lock identities and acquisition times. No later
unit will reuse the single-lock invocation.

Dissonance v4 subsequently froze the cross-task admission shape. The active
`harmony-n4-kvm-boot-8312745e-final.service` remains valid and is preserved to
drain naturally because it was already running with exact CPUs `2-5` and held
compute-exclusive before benchmark-shared. Every later unit runs in
`consonance.slice`, declares `AllowedCPUs=2-5`, and uses the single canonical
ExecStart chain `/usr/bin/flock --exclusive
/run/lock/harmony-msr1-consonance-compute.lock /usr/bin/flock --shared
/run/lock/harmony-msr1-benchmark.lock <payload...>`. No shell acquires either
lock and no payload starts before both locks. This one-PID waiting shape is the
only form Dissonance v4 attests; `system.slice` or shell-wrapper waiters are not
used again.

Sequencing audit: `harmony-n4-kvm-boot-8312745e-final` was launched after the
general unsafe-crates Miri job and the exact `f2a24062` x86 workflow completed,
but while ancestor nightly run `33216717204` still had its separate `vmm-core`
Miri job in progress. That violated the steer to finish the complete Miri/CI
wait before starting another msr1 compute unit, even though the unit itself met
the then-current hardware locks and the later Dissonance-v4 steer explicitly
preserved it. Its byte-equality result is retained as technical evidence but is
not used as the final ordered N4 reference; after the Miri wait finishes and the
plan-only commit is pushed, the boot and campaign references are rerun through
the canonical `consonance.slice` nested-flock form.

### N4 result

- **PASS:** the owned guest tree is at `consonance/harmony-linux/`; build
  entrypoints, workflows, documents, and agent paths follow it without changing
  the GPL/AGPL boundary.
- **PASS:** all repository checks passed at the moved layout on macOS and Linux,
  including the required unsafe-crate Miri coverage.
- **PASS:** the N1 references passed again on M1 Max HVF, msr1 KVM, and GitHub
  x86 KVM, including final ARM byte equality and primary AMD/Intel X2 runs.
- **PASS:** every credited guest image was freshly built from the moved tree;
  the four artifact hashes above were verified against fresh manifests on the
  reference hosts rather than inherited from the old path.
- **PASS:** the `/dev/harmony` concurrent-ringer positive passed on the fixed
  driver and the same gate rejected a freshly built planted no-lock mutant.
- **PASS:** issues #172 and #211 are closed with evidence and every additional
  bug found during the milestone is filed as #208–#210, #212, or #213.

**N4 overall: PASS.** The moved guest is part of Consonance, the serialization
race is closed by one kernel-owned path with a non-vacuous negative, and the
three-host rebuilt references and fresh manifests meet both clauses.

## N5 — reproducible guest builds

In progress.

### Locked-build decisions and local validation

- The repository-root `flake.nix` and `flake.lock` pin nixpkgs at
  `d57af924f160a5084293c71c2043f058bd1cdb60`. The builder receives the
  hash-checked Linux 6.18.35, BusyBox 1.38.0, musl 1.2.6, and PostgreSQL
  17.10 sources as Nix store inputs. Cargo's application and Rust
  standard-library registries are produced from their two lockfiles and
  joined into one offline vendor directory.
- The root flake snapshot is copied into a new temporary worktree for every
  invocation. Downloads, build roots, and output staging are new; Cargo is
  forced offline, Cargo and make parallelism are capped at four, and the SMB
  ROM is admitted only at its frozen SHA-256. The build emits its sorted
  per-artifact `MANIFEST.sha256` rather than accepting a copied manifest.
- Rust's musl standard library is rebuilt with `+lse,-outline-atomics`. The
  pinned GCC unwind archive references two outline-atomic ABI helpers, so the
  Nix closure adds strong `CASAL`/`SWPAL`-only implementations. This keeps the
  final NES agent static while preserving the existing zero-LL/SC gate.
- A first isolated-store attempt reached the pinned LLVM/Clang closure that
  had been selected only for `libunwind.a`, then the local Linux builder killed
  Clang's final link for memory exhaustion (exit 137). The design now uses the
  pinned musl GCC unwind archive and the two LSE helpers instead; no LLVM build
  is part of the guest closure.
- Fail-closed retries exposed and fixed the wrapped-versus-unwrapped Rust
  source layout, the missing standard-library vendor lock, the vendor helper's
  extra registry directory, and four planted-by-the-toolchain outline LL/SC
  instructions. The last retry proved the agent audit clean before proceeding
  to the kernel.

The clean macOS-hosted Linux/aarch64 build then completed the full release set
from a fresh external build root. The kernel counter and LL/SC planted
negatives were rejected; kernel, vDSO, init, NES agent, and PostgreSQL payload
audits were clean; and `sha256sum -c MANIFEST.sha256` accepted all ten outputs:

```text
314afa30412f3e9ee0022913bb3dbe9ff67971f8dd0a7fcd529a853054f2a9af  arm64/Image
fb1bc7957d558d1261d587c0bfcde916087f0fe207be1117d6c9861806823c84  arm64/Image-game
08cafe8a473b56f7ad9274641cb661770bb45e11245fb504254ea6a154a499b1  arm64/Image-postgres
a72a6f0b9587f58beaa6bf3fbb5d7e2002f885fb8e11cf36da14499160eccb79  arm64/harmony-tetanes-agent
a0dad8dba8693a07e8e90e50f3388d33bbd0257854b09001c4b6e6dc85d64cea  arm64/initramfs-game.cpio.gz
a7ec0987ff422f4c587f2d2ef54df194ae6de937420902319e9cb519c868905b  arm64/initramfs-postgres.cpio.gz
b2fbb8021eef6e5e8c0b11a6bb4227fc710fe54cfe9eb2d85d4a58a1e3cb4ab7  arm64/initramfs.cpio.gz
5c2c28a0972c07a26235ceb1c08eb524be4e16da7f2594a5f43e35b4b5cbfe15  arm64/pg_ctl
e9c2c4801727f4e784188f33b01efc08a3a820d684493fe86db1aeba389b756d  arm64/postgres
a78b0657c6974881924fe76d966f8e2bcca16cd7eb61a6965825b45ee7092916  arm64/psql
```

The required one-byte negative used the builder's mutation switch to change
the ARM cache-line patch from `return 64` to `return 65`. The clean mutant
build changed `arm64/Image` from `314afa30...a9af` to
`a8866a3b...8bbd`, while the independent minimal initramfs remained
`b2fbb802...b4ab7`. Thus the comparison observes source changes and cannot pass
by merely reusing or recopying the old image.

The first native x86 attempt built the kernel but failed closed before
publication because its provisional RNG allowlist was not in the flake source.
The corrected GCC 15.2 retry then failed the exact TSC accounting: optimizer
drift moved dozens of reviewed sites and outlined two functions. Arming a broad
new exception list from compiler drift alone would weaken this control, so that
candidate was rejected. The locked x86 closure instead pins GCC 13 and reuses
the already reviewed Ubuntu/GCC-13 TSC and RNG baselines. Both failed runs are
invalidated and retained as fail-closed gate evidence. A separate diagnostic
using a macOS bind mount is also invalid: the host's case-insensitive filesystem
collapsed Linux case-distinct netfilter sources. Corrected runs use a native
Linux volume.

The successful ARM build above remains pre-commit smoke evidence. N5 is not
credited until exact-commit, distinct-host clean-store equality and all-three-
machine N1 boots have passed.

The final staged-source Linux/x86_64 run used locked GCC 13.4 and the existing
reviewed GCC-13 opcode baselines. Exact accounting accepted all 115 TSC sites,
all five RNG sites, and zero raw counter/RNG opcodes in setup or decompressor.
BusyBox linked statically from the declared glibc archive output, the manifest
verified, and the build emitted:

```text
ba78ba8c0a1694a8fe278400b8ca56b9f73491c3cd9ad03e4f7df72c88ac4b0e  x86_64/bzImage
49aa012124f80e8fda3bcd5873ce24470e33ae961ed42fb0ee323f5e6abf2744  x86_64/initramfs.cpio.gz
```

This remains local pre-commit validation; the GitHub runner rebuild and X2 boot
are the creditable x86 lane.

At exact implementation commit `84faefed`, the signed M1 Max HVF reference
booted the Nix ARM minimal image ten times. All normalized logs were
byte-identical at SHA-256 `6d893601...a1bba`; each run reported 38,381 portable
events, 283 schedules, 136 deliveries, 150 checkpoints, log digest
`d23091c1...6e16`, and final state hash `e3ce731a...383a`. The image and
initramfs were `314afa30...a9af` and `b2fbb802...b4ab7`, and the complete
ten-artifact manifest reverified immediately after the boots.

The first exact-source msr1 build attempt was admitted as
`harmony-n5-nix-84faefed.service` at `2026-08-29T06:59:24Z` (invocation
`7fe3048f0aa34366be5e24126a1971f6`). Its outer PID 246434 had the canonical
compute-W then benchmark-R nested-flock argv; the locks were held by PIDs
246434 and 246436, the payload began only afterward as PID 246437, and the
unit's cgroup, declared `AllowedCPUs`/`EffectiveCPUs`, and observed
`Cpus_allowed_list` were all exactly `consonance.slice` and `2-5`.
`CARGO_BUILD_JOBS`, make, and Nix jobs/cores were capped at four. The minimal
kernel built and passed its planted counter negative, but the second kernel
source extraction exhausted the 28 GiB `/tmp` tmpfs at
`2026-08-29T07:20:59Z`; systemd recorded exit 2 at `07:21:09Z`. No full
manifest was published and this run is invalidated, not counted.

The clean retry uses new store, output, and temporary-tree paths rather than
reusing the partial store. `harmony-n5-nix-84faefed-r2.service` was admitted at
`2026-08-29T07:23:34Z` (invocation
`05feb01be04248aea235606cfa6831df`). Its exact ExecStart is the canonical
`flock --exclusive ...consonance-compute.lock` then
`flock --shared ...benchmark.lock` followed by the payload; outer PID 285927
held the compute WRITE lock, PID 285929 held the benchmark READ lock, and
payload PID 285930 began at `07:23:34.401619513Z`. The unit and observed payload
were in `consonance.slice` with declared/effective/observed CPUs exactly `2-5`,
and all Cargo, make, and Nix parallelism remained at four. Its fresh extraction
tree is on the root filesystem, which had 59 GiB free, rather than the
constrained tmpfs. That change carried all three kernels through their planted
counter/LL/SC negatives and completed the NES image, then the path-backed Nix
store's user namespace denied the two `mknod(2)` calls needed by build-time
PostgreSQL `initdb`. The unit exited fail-closed at
`2026-08-29T08:35:23Z`; it published no manifest and is also invalidated.

The namespace diagnosis itself used short canonical
`consonance.slice`/CPUs-`2-5` units with compute-W then benchmark-R. A plain
outer-namespace probe created and verified character device 1:3, while the
same `/usr/bin/mknod` inside `nix --store /path shell` retained full apparent
capabilities but failed with `EPERM`. The candidate bridge then passed as
`harmony-n5-helper-integration.service`: a client inside the Nix user namespace
requested exactly `null` 1:3 and `urandom` 1:9 over FIFOs, the outer helper
validated their temporary PostgreSQL-root paths and identities, created and
verified both nodes, and the test removed them. The committed helper has no
general device-node interface; any different path, name, number, pre-existing
target, non-FIFO endpoint, or failed post-create verification stops the build.

GitHub run `33239700060` is also invalidated: its locked x86 build produced the
expected hashes, but the pre-existing serialization test could not find QEMU.
Run `33240072428` installed QEMU and again built the expected bytes, but exposed
a real sequencing error: the locked builder had already removed its private
kernel source/object workspace before the external serialization mutation gate
ran. Commit `ee0be628` moves that positive and planted-negative KUnit gate
inside the x86 locked builder, before artifact staging and cleanup; exact-head
run `33240831482` is the creditable retry. Neither failed run is counted toward
N5 or X2.

## N6 — defenses tested by attacking them

Not started.
