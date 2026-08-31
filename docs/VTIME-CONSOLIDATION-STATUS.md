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

Complete.

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
verified both nodes, and the test removed them. This proved the immediate
`mknod` diagnosis, but the candidate helper was not retained as the solution.

The next fresh-store run, `harmony-n5-nix-bffc15be-r3.service`, began at
`2026-08-29T08:46:29Z` (invocation
`279c595a87d44d00baf5e37596ed2d7a`) on exact commit `bffc15be`. Outer PID
381751 and benchmark-lock PID 381753 held the canonical compute-W then
benchmark-R pair; payload PID 381754 began afterward at
`08:46:29.113900017Z`. Its cgroup was `consonance.slice`, and declared,
effective, and observed CPUs were all exactly `2-5`; Cargo, make, and Nix were
capped at four jobs. The bridge created and verified the two requested device
nodes, proving that boundary, but the following `install -o 65534` failed with
`EINVAL`: the path-store user namespace also cannot map the runtime PostgreSQL
uid. The unit exited fail-closed at `10:00:31Z`, published no manifest, and is
invalidated. Rather than grow a syscall-by-syscall privilege bridge, the final
design populates the clean store first and executes its exact app from a
read-only bind at `/nix/store` inside a mount-only namespace. This preserves the
isolated closure while retaining the real root namespace required by `mknod`,
`chown`, `chroot`, and uid 65534.

The bounded launcher proof used the already-populated r3 store without starting
another image build. `harmony-n5-resolve-app-probe.service` resolved the exact
locked app as
`/nix/store/4miazr16...-harmony-build-guest-images`; then
`harmony-n5-mount-launcher-probe.service` entered a mount-only namespace, bound
that store at `/nix/store`, and reached the app's own argument parser. Its
intentional lone `--output` argument produced the expected usage exit 2, rather
than a mount, loader, or store-path failure. Both probes used the canonical
compute-W then benchmark-R ExecStart shape in `consonance.slice` on CPUs `2-5`.

GitHub run `33239700060` is also invalidated: its locked x86 build produced the
expected hashes, but the pre-existing serialization test could not find QEMU.
Run `33240072428` installed QEMU and again built the expected bytes, but exposed
a real sequencing error: the locked builder had already removed its private
kernel source/object workspace before the external serialization mutation gate
ran. Commit `ee0be628` moves that positive and planted-negative KUnit gate
inside the x86 locked builder, before artifact staging and cleanup; exact-head
run `33240831482` is the creditable retry. Neither failed run is counted toward
N5 or X2.

### Reproducibility repairs and exact final-tree evidence

The first complete distinct-host comparison was intentionally not accepted.
`harmony-n5-nix-966ce92a-r4.service` ran from `10:04:50Z` through
`11:19:26Z` under the canonical compute-exclusive then benchmark-shared
locks (outer PID 511772, payload PID 511775), in `consonance.slice` with
declared, effective, and observed CPUs exactly `2-5` and parallelism four.
It completed the full offline build and its own manifest verification, but a
comparison with the macOS-hosted ARM build found four unequal artifacts:
`harmony-tetanes-agent`, `initramfs-game.cpio.gz`, `postgres`, and
`initramfs-postgres.cpio.gz`. The other six artifacts were byte-identical.
Strings and compiler metadata localized the differences to randomized absolute
build roots in Rust crate identities and PostgreSQL compiler metadata. This is
useful fail-loud evidence, but r4 is invalidated and supplies no N5 credit.

Commits `82048751` and `c59d229f` added compiler prefix maps and a stable
PostgreSQL compiler wrapper. Two further msr1 runs were stopped rather than
allowed to consume the reserved host after local independent builds had already
proved their exact trees non-reproducible:

- `harmony-n5-nix-82048751-r5.service` began at
  `11:30:21.771581900Z`; outer PID 602573 and payload PID 602576 used the
  canonical lock order in `consonance.slice`, with declared/effective/observed
  CPUs `2-5` and four-way parallelism. Randomized Rust crate disambiguators
  still changed the NES agent. The unit was stopped at `11:35:06Z`, failed
  closed during the agent build/audit, and published no manifest.
- `harmony-n5-nix-c59d229f-r6.service` began at
  `11:50:41.883902762Z`; compute-W PID 612656 held inode `00:23:7`,
  benchmark-R PID 612658 held `00:23:6`, and payload PID 612659 was in
  `/consonance.slice/harmony-n5-nix-c59d229f-r6.service`. Its exact nested-flock
  argv, declared/effective/observed CPUs `2-5`, and four-way parallelism were
  captured. A local clean build showed that the remaining randomized source
  root still changed Rust symbol hashes, so r6 was stopped at
  `12:00:54.941229098Z` before publication; systemd completed the stop at
  `12:01:09Z`. Both locks were then verified free. No r5/r6 byte is credited.

The final repair at signed commit `74d5f9da` gives native ARM builds one
fresh, fail-if-present `/build/harmony-nix-guest` workspace. This is required
because rustc's crate identity reaches symbol hashes even after diagnostic
prefix remapping. The game image now requests GNU cpio's reproducible inode
assignment. PostgreSQL exposed a different source of nondeterminism: build-time
`initdb` writes host-time/PID/random state into `pg_control`, `pgstat.stat`, and
the first WAL segment. Baking a fixed authentication nonce or a predictable
placeholder into the published image was rejected as unsafe. Instead, the
published PGDATA is empty and the immutable guest runs ordinary `initdb` once
per boot, obtaining its nonce from the guest's seeded entropy before starting
the existing fixed workload. Thus no host-random database state and no
predictable authentication nonce is shipped.

Two independently created macOS-hosted Linux/aarch64 containers, each starting
with its own fresh Nix store, separately populated the locked input/app closure
and then executed the exact `74d5f9da` builder with `--offline`. Their complete
ten-artifact manifests were byte-identical:

```text
314afa30412f3e9ee0022913bb3dbe9ff67971f8dd0a7fcd529a853054f2a9af  arm64/Image
fb1bc7957d558d1261d587c0bfcde916087f0fe207be1117d6c9861806823c84  arm64/Image-game
08cafe8a473b56f7ad9274641cb661770bb45e11245fb504254ea6a154a499b1  arm64/Image-postgres
4b0a2bfdab4a65bbb91e377979ac736559b35a485450209b87248d6f989beacf  arm64/harmony-tetanes-agent
f8f46f57cb635eab77c7cffdfccd4ed5cc46435c82a5b807543e53f81a47e28f  arm64/initramfs-game.cpio.gz
54a0b54228c10bfc27089ff8f198b428f6e426a31f07036d90b5e9ffe467220c  arm64/initramfs-postgres.cpio.gz
b2fbb8021eef6e5e8c0b11a6bb4227fc710fe54cfe9eb2d85d4a58a1e3cb4ab7  arm64/initramfs.cpio.gz
5c2c28a0972c07a26235ceb1c08eb524be4e16da7f2594a5f43e35b4b5cbfe15  arm64/pg_ctl
2f31d42ab080308c01f6426a02af90e3fcfe1087f334a61643bcad8197449341  arm64/postgres
a78b0657c6974881924fe76d966f8e2bcca16cd7eb61a6965825b45ee7092916  arm64/psql
```

The signed-HVF PostgreSQL acceptance boot used that exact clean-store
`Image-postgres` and `initramfs-postgres.cpio.gz`. Runtime `initdb` completed,
PostgreSQL became ready, all 20 rows and the final count/sum `20/210` matched,
shutdown was clean, guest and host kernel-health scans passed, the per-entry
watchdog stayed green, and event 735,082 emitted `ARM64_PG_M3_READY`. The report
passed with 735,083 host-loop exits, 711,942 portable events, 704,678 bounded
gaps (maximum 1,000,000 V-ns), and no comparator disagreement.

The exact clean-store minimal image was then booted ten times with the signed
M1 Max HVF runner. All ten complete normalized logs were byte-identical at
SHA-256 `6d893601...a1bba`; each contained 38,381 portable events, 283
schedules, 136 deliveries, 150 checkpoints, digest `d23091c1...6e16`, and
final state hash `e3ce731a...383a`.

The final distinct-host msr1 build is
`harmony-n5-nix-74d5f9da-r7.service` (invocation
`ab3e8cba7ff24d81a1ed528e1415efb2`). Its canonical one-PID ExecStart began at
`12:50:37Z`: compute-exclusive PID 630604 acquired inode `00:23:7`, then
benchmark-shared PID 630606 acquired `00:23:6`, and payload PID 630607 began
only after both were held, at `12:50:37.905791836Z`; both-lock attestation was
recorded at `12:50:37.912827959Z`. The exact cgroup is
`/consonance.slice/harmony-n5-nix-74d5f9da-r7.service`; slice, declared
`AllowedCPUs`, `EffectiveCPUs`, and every observed `Cpus_allowed_list` are
`2-5`; Cargo, make, Nix jobs, and Nix cores are four. It is currently running
the exact `74d5f9da` offline build. It completed successfully at
`14:05:22.771387863Z` (3 h 50 m CPU, 9.7 GiB peak), rechecked every manifest
entry, and independently emitted the exact ten hashes printed above. This is
the required distinct-host clean-store equality, not a copied-artifact check.

GitHub Actions run `33252942226` completed successfully on the same exact
`74d5f9da` tree. Its locked x86 build emitted `bzImage`
`ba78ba8c...4b0e` and initramfs `49aa0121...744`; check, both X1 jobs, all six
live probes, the guest build, all four X2 replicas, and all eight hunt replicas
passed. The four X2 normalized logs were byte-identical at SHA-256
`6606865d...4478`, with 35,234 events, digest `cf4732e...36f`, zero state or
RAM-page differences, and green pvclock, ready, userspace, and placement
oracles.

### Exact-tree cross-backend checkpoint repair

The first msr1 N1 retry correctly refused credit. Unit
`harmony-n5-kvm-boot-74d5f9da-r8.service` (invocation
`c385f13e043c44e8b67e7f6c7de1c422`) began at `14:06:53.570060586Z` with
compute-W PID 712113, benchmark-R PID 712115, and payload PID 712116. Its
canonical argv, `consonance.slice`, declared/effective/observed CPUs `2-5`, and
Cargo jobs four were captured. The first boot reached readiness, but its log
SHA `dce6bb3...31df` differed from HVF `6d89360...a1bba` at exactly checkpoint
event 6,911: HVF state hash `7409db2...981c`, KVM
`81927ea...43a4`. Counts, event payloads, delivery placement, and final state
`e3ce731a...383a` otherwise agreed. The payload failed closed and r8 is
invalidated.

Canonical unit `harmony-n5-kvm-component-74d5f9da-r9.service` (invocation
`39bb90b3bd40405287fa120ca1392147`) used compute-W PID 714226,
benchmark-R PID 714228, and payload PID 714229; both locks were held at
`14:14:30.020607737Z`, with the required cgroup/CPU/parallelism evidence. The
paired component oracle localized event 6,911 to `RAM:2M..16M`; core registers,
sysregs, SIMD, debug, vtimer, interrupts, MP state, serial/devices, GIC, V-time,
and entropy all matched. r9 completed at `14:17:55.003194016Z`.

Two more canonical diagnostics made the RAM evidence non-vacuous. r10
(`e18a8bd4cca447af957cd3588a8e1667`) ran compute-W PID 715220,
benchmark-R PID 715222, payload PID 715223 from
`14:22:01.596871280Z` through `14:25:27.096079073Z`; its first boundary dump
was later overwritten by the runner's intentional final dump and is not used.
r11 (`43d0e8a25ff54e32ac2f4a4a1ce4b4d4`) stopped exactly after raw/portable
event 6,911, with compute-W PID 715888, benchmark-R PID 715890, payload PID
715891, both locks at `14:26:50.353084645Z`, and completion at
`14:27:28.633776939Z`. Both units had exact `consonance.slice` and CPU `2-5`
evidence. Comparing r11's 128 MiB KVM RAM image with the paired HVF image found
exactly one byte: guest-RAM offset `0x243950`, HVF `0x18`, KVM `0x10`.

A per-change watch then proved both guests wrote the byte at portable event
6,870 and overwrote it before the next checkpoint: HVF wrote `0x18` and KVM
`0x10`; every other watched transition aligned after accounting for HVF's
raw-only GIC exits. r14 (`81f7568e687f45ad952377970dd598ff`) built and ran
that diagnostic under compute-W PID 718163, benchmark-R PID 718164, payload
PID 718165, both locks at `14:41:58.806051201Z`, exact cgroup/CPUs, and four
Cargo jobs; it completed at `14:43:15.459559820Z`. The pinned Linux source
identified the retained value as raw redistributor capability state:
stock KVM advertises aggregate DirectLPI through `GICR_CTLR.IR` while the
userspace GIC advertised it through `GICR_TYPER.DirectLPIS`. Linux therefore
made and printed the same feature decision but retained different raw bytes on
its early stack.

The first proposed repair, signed commit `94531beb`, cleared DirectLPI instead
of moving its representation. One signed-HVF boot immediately falsified it:
the event count fell to 38,348 and the guest stopped printing DirectLPI.
GitHub run `33257903612` was cancelled, and canonical r13 (compute-W PID
717416, benchmark-R PID 717418, payload PID 717419, both locks at
`14:36:38.182933619Z`) was stopped at `14:37:34Z` after its exact build and
before a boot completed. r12, with otherwise valid canonical admission
(compute-W 717193, benchmark-R 717195, payload 717196 at
`14:35:22.401514217Z`), had already failed before compilation because its
payload omitted Cargo's explicit path. r12/r13 are invalidated and credited
for nothing.

Corrective signed commit `d069e8f6` instead publishes `GICR_TYPER=0x10` and
`GICR_CTLR.IR=1`, preserving DirectLPI through the exact stock-KVM register.
Its positive unit/integration tests pass and the planted old `TYPER=0x18`
representation is rejected. The first signed-HVF boot retained 38,381 events,
all placement/count invariants, and produced log SHA-256
`dce6bb3...31df`, byte-identical to the previously captured stock-KVM log,
including checkpoint 6,911 (`81927ea...43a4`) and final state
`e3ce731a...383a`.

Final exact-tree evidence passed. Ten signed M1 Max HVF boots from `d069e8f6`
and ten msr1 KVM boots all emitted the same complete normalized-log SHA-256
`dce6bb384b9eceac442e09a6caa0bf01ba706b63b920915738a944f7908317df`.
Every run had 38,381 portable events, 283 schedules, 136 deliveries, 150
checkpoints, normalized digest
`a8b908de4ef53a4f52e055dd72522d551c7b547b70608b89b0ac52b0b02394ce`,
final state `e3ce731ade18f07e3706cec60df7f6220e345f560010e9d1beec13021d63383a`,
and green placement/late-delivery negatives. The HVF raw count was 38,804;
KVM's was 38,381, exactly the ruled raw-only boundary difference.

On msr1,
`harmony-n5-kvm-boot-d069e8f6-r15.service` (invocation
`a3caf61fcee14db7b09f28bc47732ec5`) began under the canonical one-PID argv at
`14:50:14Z`; compute-W PID 719321, benchmark-R PID 719323, and the payload are
in `/consonance.slice/harmony-n5-kvm-boot-d069e8f6-r15.service`, with declared,
effective, and observed CPUs exactly `2-5` and Cargo jobs four. Payload PID
719324 began only after both locks. The unit completed successfully at
`15:26:16.953058162Z`, after its own ten-way `cmp` and SHA checks, consuming
41 min 11 s CPU with an 880.4 MiB peak; both locks then released.

GitHub Actions run `33258549471` passed on exact final commit `d069e8f6`:
check, both X1 jobs, all six live probes, the locked guest build, all four X2
replicas, and all eight hunt replicas. The final x86 manifest retained
`ba78ba8c...4b0e` and `49aa0121...744`. Each X2 replica ran ten 35,234-event
boots plus its smoke/component/negative checks with digest
`cf4732e6...636f`, checkpoint reference `21daf112...fecef`, ready/pvclock and
placement green, and zero component disagreement.

The exact final local tree also passed `cargo build --all-features`, all 1,171
nextest tests, Clippy for all features/targets with warnings denied, formatting,
and cargo-deny advisories/bans/licenses/sources. The first sandboxed nextest
attempt saw two loopback-listener `EPERM` errors and stopped fail-fast after
406 passes; the permission-correct rerun passed all 1,171. Likewise the first
cargo-deny attempt could not lock the sandbox-read-only advisory database; its
permission-correct rerun passed. Neither sandbox failure is counted as a code
gate result.

- **PASS:** distinct clean stores on a macOS-hosted Linux builder and native
  msr1 produced byte-identical hashes for all ten ARM artifacts; the final
  GitHub x86 build reproduced its two-artifact manifest.
- **PASS:** the deliberate one-byte kernel patch changed the Image hash while
  leaving the independent initramfs hash fixed, so the build comparison can
  fail and is not a copied-manifest oracle.
- **PASS:** final-tree N1 references passed on M1 Max HVF, msr1 KVM, and both
  GitHub stock-KVM vendor pools with exact normalized logs, checkpoint state
  sequences, and placement checks.

**N5 overall: PASS.** The locked guest closure is reproducible from clean
stores, the negative changes observable output, and the final host tree passes
all three-machine references and repository gates.

## N6 — defenses tested by attacking them

### Implementation and static attack evidence

Signed commit `2f76ffae` adds the N6 defense sweep. The machine-readable
instruction table is now the only input to a strict generator: it emits the
complete committed listing, per-architecture assembly fragments, guest row
metadata, and the verifier. The verifier accepts exactly two reports, requires
one report row for every table row and the exact expanded operation count,
requires byte-identical same-seed JSON, and requires both a hidden feature bit
and an audit rejection for each `mask-and-audit` row. Its self-test first passes
complete synthetic reports and then proves that four planted defects fail:
one missing row, a same-seed mismatch, traps disabled, and visible entropy.

The first committed table expanded to 128 arm64 and 23 x86 operations. Review
against the pinned Linux arm64 system-register definitions then found two
missing `ID_AA64AFR[01]_EL1` reads and the write forms for all 31 direct PMU
event counters and types. Signed correction `d61c33a0` adds those 64 forms,
making arm64 9/9 rows and 192 operations. A further completeness audit found
that the x86 CPUID row still exercised only leaf zero. Signed commit
`fa20fb9d` expands it to every reported basic and extended leaf, every bounded
modeled subleaf, wildcard and hypervisor-range boundaries, and out-of-range
defaults. The final x86 count is 9/9 rows and 166 operations. The generated
listing compares byte-for-byte with fresh generator output. Clang assembled
both generated fragments for their respective ELF targets. The generator
self-test passed both positive reports and rejected all eight
architecture-specific planted defects.

The guest copies each generated fragment into a reusable writable JIT page,
changes it to executable only after each emission, executes it against a
freshly zeroed result mapping, and records the signal/value and memory digest.
The page returns to writable before the next emission, so it is never W+X;
the synchronous-fault handlers are installed once for the bounded PID-1 sweep.
Separate owned images carry the
normal and traps-off kernel configurations. The x86 live CI test is ordered so
the traps-off image must fail on an observed JIT instruction escape before
either of the two traps-on runs may be credited. Signed commit `46273a3a`
orders the verifier so the observed result is checked before the configuration
label. Signed commit `63daf596` removes the guest's own `PR_SET_TSC` call: the
normal x86 positive must now be established solely by the owned kernel's
exec-time CR4.TSD policy, while the traps-off image must expose the counter.
The arm64 image builder produces the same ordered pair for HVF and KVM. An
executable-section ELF scanner rejects
arm64 `RNDR`/`RNDRRS` and x86 `RDRAND`/`RDSEED`; its positive scan and actual
planted forbidden-opcode binary run before an image can be published.

The LL/SC model passed its quiet and legally noisy side-effect-free retry
loops with the same final value. The accumulating-retry variant diverged (zero
versus three retained retries), and a planted weak comparator accepted that
defect while the complete-state comparator rejected it. The tripwire audit
matched all three retained x86 patch hashes and 15 required mechanisms; its
planted deletion of the instruction-exit ABI was rejected. Both arm64 and x86
patch series apply successfully to the exact hash-pinned Linux 6.18.35 source;
the build transcript retains every reported offset/fuzz application. Loading
the optional patched KVM modules on shared msr1 is out of scope:
it would replace the host substrate underneath Dissonance and violate the
single-tenant evidence protocol. The committed static patch/hash/mechanism
audit is therefore the N6 tripwire check; N5's stock-KVM references supply the
separate observational-inertness evidence.

### Attempts, isolation, and final-tree gates

The first exact-tree arm64 build is
`harmony-n6-arm-build-2f76ffae-r1.service` (invocation
`00a4a684acaa4998b680f5eaadae24a0`). It began at `16:02:32Z` in
`consonance.slice` with the canonical single-PID nested-flock ExecStart:
compute-exclusive PID 728708 acquired device/inode `35:7` (`00:23:7` in
`/proc/locks`), benchmark-shared PID 728710 acquired `35:6` (`00:23:6`), and
payload PID 728711 began only after both locks were held at
`16:02:32.470340927Z`. Declared `AllowedCPUs`, `EffectiveCPUs`, and every
observed `Cpus_allowed_list` were exactly `2-5`; Cargo, make, Nix jobs, and
Nix cores were capped at four. The unit was preserved until natural completion
at `16:12:53Z` and failed closed before kernel compilation: the builder required
`CONFIG_HARMONY_ARM_USER_COUNTER_TRAPS=y`, but had omitted patch `0012` that
defines the symbol. It consumed 22 minutes 40 seconds CPU with a 7 GiB peak,
published no artifact, and released both locks. It was already diagnostic
because `d61c33a0` subsequently completed the arm64 frozen listing; no r1 byte
receives N6 credit.

Signed commit `ae8a3eb4` repairs that specific failure by applying arm64 patch
`0012` in the builder's ordered series before configuration. Diagnostic build
`harmony-n6-arm-build-ae8a3eb4-r2.service` (invocation
`de1d210ead4b4f5282534592b27e4947`) began at `16:15:42Z` with the exact
canonical ExecStart. Compute-exclusive PID 741416 acquired `35:7`/`00:23:7`,
benchmark-shared PID 741418 acquired `35:6`/`00:23:6`, and payload PID 741419
began only after both locks were held at `16:15:42.327952502Z`. Its cgroup is
`/consonance.slice/harmony-n6-arm-build-ae8a3eb4-r2.service`; declared
`AllowedCPUs`, `EffectiveCPUs`, and observed affinity are exactly `2-5`, and
the recorded Cargo parallelism is four. It is building exact
`ae8a3eb4ca76d7032dfbc747375ea12d03a7026e`. It completed naturally and
successfully at `16:49:14Z`, after 1h41m23.915s CPU with a 5.7 GiB memory peak,
then released both locks. The normal and traps-off kernels and both N6
initramfses passed their manifests; their respective SHA-256 values are
`314afa30…a9af`, `da38b6df…8e877`, `c4c7e50d…3bcad`, and
`41a3cf64…53f`. The later observed-escape, complete-CPUID, and
kernel-owned-trap corrections make r2 diagnostic only; these bytes receive no
N6 credit, but the clean completion proves the repaired ARM build path has no
remaining construction fault.

The first local N6 HVF attempts against those diagnostic r2 artifacts also
failed closed rather than being credited. Two unsigned invocations stopped at
`hv_vm_create` with `HV_DENIED`, so no guest ran. After applying the repository's
Hypervisor entitlement, the traps-off guest reached its first PMU operation and
the VMM rejected `PMCR_EL0` (`0x0030e418`) as an unruled sysreg. Exhaustive
review then identified 74 unique frozen PMU sysregs (147 table operations): the
fixed singleton registers plus `PMEVCNTR0..30`, `PMEVTYPER0..30`, and
`PMCCFILTR_EL0`. A planted adjacent `PMEVCNTR31` encoding remains default-deny.

That review also found that HVF's existing `complete_fault` implementation did
not implement its documented deny-UNDEF contract: because the trap decoder had
already advanced PC, it merely skipped the instruction. A traced diagnostic
confirmed all 147 PMU exits were being visited, then was stopped without
credit. The corrected backend now performs the AArch64 synchronous-exception
state transition itself: it restores the faulting PC into `ELR_EL1`, saves the
old PSTATE, constructs EC=UNKNOWN/IL=1 in `ESR_EL1`, derives the EL1 vector and
handler PSTATE from the source mode and `SCTLR_EL1`, and enters the vector only
after staging all exception registers. Pure tests cover EL0t, EL1t, EL1h,
SPAN/DSSBS, underflow, invalid mode, and vector overflow; the VMM test exercises
both directions of every frozen PMU encoding and the adjacent default-deny
negative.

Two further entitled diagnostic runs established that the original one-process-
per-operation guest harness, not the backend, made the exhaustive PMU row
impractical: the one-million- and three-million-event bounds both expired after
the first four rows while the 147-operation PMU row was still forking. Neither
incomplete report is credited. Signed commit `c48d310e` replaces each fork with
an in-process `sigsetjmp` recovery seam for synchronous `SIGILL`/`SIGSEGV`.
Every generated operation still executes in its own freshly emitted RX mapping
against freshly zeroed result memory, and an untrapped/hanging operation stays
bounded by the host entry watchdog. Strict C syntax checking, focused backend
and VMM tests, `cargo build --all-features`, permission-correct nextest
(1,174/1,174; 25 configured skips), all-target/all-feature Clippy with warnings
denied, formatting, and cargo-deny all pass on this candidate. The pinned
`nightly-2026-06-16` Miri gate with permissive provenance also passes for the
unsafe-bearing `vmm-backend` crate: 48 unit, three contract, two dynamic, two
exhaustive, 16 run-loop, and one vCPU-state test, with the public-API
regenerator deliberately ignored.

Exact build `harmony-n6-arm-build-c48d310e-r7.service` (invocation
`cf131bd00b924d3b8b869ca809babc94`) began at `19:26:46Z` with the canonical
single-PID nested-flock ExecStart. Compute-exclusive PID 790495 and
benchmark-shared PID 790497 acquired `35:7`/`00:23:7` and
`35:6`/`00:23:6`; payload PID 790498 began after both were held at
`19:26:46.185047482Z`. Its cgroup is
`/consonance.slice/harmony-n6-arm-build-c48d310e-r7.service`; declared
`AllowedCPUs`, `EffectiveCPUs`, and observed affinity are exactly `2-5`, and
Cargo/make parallelism is four. It is building exact
`c48d310ee05d9c5bab1b7ec488c1ac42dd401350`. It completed naturally and
successfully at `20:00:22Z`, after 1h41m33.349s CPU with a 6.3 GiB memory peak,
verified all five manifest entries, then released both locks. The Image,
traps-off Image, ordinary initramfs, N6 initramfs, and traps-off N6 initramfs
hashes are respectively `314afa30…a9af`, `da38b6df…8e877`,
`b2fbb802…b4ab7`, `a0d902cc…f8790`, and `3825be78…fa15`. The newer x86
scanner repair makes r7 diagnostic only; none of these bytes receives final N6
credit.

Exact build `harmony-n6-arm-build-c9b4a880-r10.service` (invocation
`90969e95272e432ba77dc1d13a3fabb0`) began at `20:00:59Z` with the frozen
canonical ExecStart. Compute-exclusive PID 822351 acquired `35:7`/`00:23:7`,
benchmark-shared PID 822353 acquired `35:6`/`00:23:6`, and payload PID 822354
began only after both locks were held at `20:00:59.190940048Z`. Its cgroup is
`/consonance.slice/harmony-n6-arm-build-c9b4a880-r10.service`; declared
`AllowedCPUs`, `EffectiveCPUs`, and observed affinity are exactly `2-5`, and
Cargo/make parallelism is four. The local and uploaded payload hashes are both
`7502a738…de29`. It completed naturally and successfully at `20:34:54Z`,
after 1h42m44.449s CPU with a 6.5 GiB memory peak, verified all five manifest
entries, and released both locks. Its Image, traps-off Image, ordinary
initramfs, N6 initramfs, and traps-off N6 initramfs hashes are respectively
`314afa30…a9af`, `da38b6df…8e877`, `b2fbb802…b4ab7`,
`bed03952…7a830`, and `b1922b78…d07f6`. The N6 initramfs changes from r7
are the intended section-garbage-collection/scanner repair. The later workflow
permission fix makes r10 diagnostic only.

Exact build `harmony-n6-arm-build-e98eab62-r12.service` (invocation
`f17fb5c2fcec4e398dfb91c87b9dd93e`) began at `20:35:14Z` with the canonical
single-PID nested-flock ExecStart. Compute-exclusive PID 852225 acquired
`35:7`/`00:23:7`, benchmark-shared PID 852227 acquired `35:6`/`00:23:6`, and
payload PID 852228 began only after both were held at
`20:35:14.585233779Z`. Its cgroup is
`/consonance.slice/harmony-n6-arm-build-e98eab62-r12.service`; declared
`AllowedCPUs`, `EffectiveCPUs`, and observed affinity are exactly `2-5`, and
Cargo/make parallelism is four. Local and uploaded payload hashes are both
`605d3378…d714`. It built exact
`e98eab62bb1b8318ce74e2fb049275507a76bf9d`, completed naturally and
successfully at `21:09:24Z`, consumed 1h42m43.549s CPU with a 6.6 GiB memory
peak, verified all five manifest entries, and released both locks. Its Image,
traps-off Image, ordinary initramfs, N6 initramfs, and traps-off N6 initramfs
SHA-256 values are respectively
`314afa30412f3e9ee0022913bb3dbe9ff67971f8dd0a7fcd529a853054f2a9af`,
`da38b6df37863c7595502148be4b8fdc449274c6a861f7a7e068b72f2558e877`,
`b2fbb8021eef6e5e8c0b11a6bb4227fc710fe54cfe9eb2d85d4a58a1e3cb4ab7`,
`a1a5b53178e493b87e77ddfc88260ed3593052f2eed2b1ee5e71770614fa9a21`, and
`d5a061ca71b28048dd9a1296c096fea4d2f3d0715513322847c494f0c4e641bb`.
The later x86-test-only descendant does not change these inputs, but r12 is
not claimed as final-tree evidence while N6 remains open.

GitHub x86 run `33261811807` similarly exercises exact `2f76ffae` and is
diagnostic after the listing correction. Run `33262147807` targets exact
`d61c33a0`; its check, both X1 jobs, and all six stock-KVM probes passed while
the locked guest-image build continued, but the subsequent ARM builder repair
makes it non-final. Runs for `ae8a3eb4`, `46273a3a`, and `fa20fb9d` were then
superseded by the non-vacuity/completeness corrections above. Exact
`63daf596` run `33263014120` passed its check, both X1 jobs, and all six
stock-KVM probes, then failed closed while building the guest: the raw
decompressor gate found one `0f31` byte sequence in executable `.text`.
Consequently the N6 and X2 jobs were skipped and no image or live result from
that run is credited. The existing failure named only the section and count,
not the byte location needed to decide whether it was a decoded `RDTSC` or a
byte sequence inside another instruction. Signed commit `df196e22` preserves
section-relative offsets, surrounding bytes, and any decoded counter
instruction on every such failure. Its exact diagnostic run is
`33263662667`; it reproduced the failure at decompressor `.text` byte 13,477
with context `48 8d 3d 0f 31 00 00`, a 64-bit `LEA` whose displacement happens
to contain the byte pair, and reported no decoded counter instruction. Signed
commit `6800066b` corrects the section model: setup and decompressor
`.head.text` retain the decode-independent raw zero-byte rule because they are
genuinely mode-mixed, while the compiler-generated 64-bit decompressor
`.text` requires zero decoded counter and hardware-RNG instructions. Its
self-test plants a real decoded `RDTSC` that must fail, the observed `LEA`
shape that must pass only in the 64-bit section, and the same raw bytes that
must still fail under the mixed-mode rule. No opcode is allowlisted and the
zero-instruction rule remains unchanged. Signed comment-only descendant
`82ef8999` aligns the scanner's earlier overview with that section rule;
superseded run `33264368929` was canceled and exact validation run
`33264421356` proved the corrected guest-image build and all preceding jobs
green, then failed before booting N6 because the workflow precreated
`n6-report` for `tee` while the test required that path not exist. Signed
commit `5fa4ab59` makes that idempotent setup explicit with `create_dir_all`;
the ignored live test still truncates each named report before use. Exact runs
`33270820760` (`c48d310e`) was canceled by workflow concurrency after the
successor push. Exact `5fa4ab59` run `33271005594` passed its check, both X1
jobs, and all six stock-KVM probes, then failed closed in the guest-image job:
the entropy scanner classified bytes `0f c7 /7` at `.text+0x2eb8d` in the
statically linked x86 sweep guest as `RDSEED`, so the image was not published
and the live N6 and X2 jobs were skipped. The scanner was sliding across every
byte rather than respecting x86 instruction boundaries, the same defect the
kernel scanner had already exposed with its `LEA` displacement diagnostic.
Signed commit `c9b4a880` makes semantic `objdump` disassembly authoritative for
compiler-generated x86 ELF sections while retaining the aligned raw-word ARM
scan. Its controls reject actual assembled `RDRAND` and `RDSEED`, accept the
exact opcode-shaped `LEA` displacement, and accept the generated x86 sweep
object; unused static-library sections are also garbage-collected before the
published initramfs audit. Shell syntax/lint, Python compilation, the two
semantic scanner controls, formatting, and diff hygiene pass locally. Exact
`c9b4a880` x86-vtime run `33271917378` passed its check, both X1 jobs,
all six stock-KVM probes, and the complete locked guest-image job, including
the real entropy-opcode planted negative, exact opcode-shaped `LEA` positive,
normal/traps-off image construction, manifests, and publication. Its ordered
live N6 job then failed closed before booting any guest: `/dev/kvm` was mode
`0600`, so the first traps-off `KVM_CREATE_VM` path returned `EACCES`; the two
positive runs never began. The uploaded report preserves the `Permission
denied` backend error. Signed commit `e98eab62` adds to the N6 job the same
udev-based `/dev/kvm` access setup already exercised by X1/X2. Exact
`e98eab62` x86-vtime run `33273802960` proved that permission correction and
passed the check, both X1 jobs, all six probes, the guest-image build, all four
X2 replicas, and all eight X2 hunt replicas. Its N6 job reached the traps-off
guest but received `Terminal(Shutdown)` before the harness checked for the
completion marker, so neither that negative nor either positive is credited.
The uploaded log records the terminal at the first ordered run. Signed commit
`dd3c9823` checks the serial marker immediately after every guest step, before
classifying a simultaneous terminal, and includes the complete serial log in
any genuine premature-terminal panic. Exact run `33277020049` passed every
preceding build/static job and exposed the actual ordered-negative failure:
the guest completed eight rows, then its intentionally generated `INT3` probe
raised `SIGTRAP`; the recovery seam had installed handlers only for `SIGILL`
and `SIGSEGV`, so Linux panicked on the death of PID 1 with exit code 5. The
staged correction installs and restores the same bounded `SIGTRAP` handler;
no verifier or expected result is relaxed. Only the newest exact tree can
receive final credit.

Exact `7252d9e4` x86-vtime run
[33280935097](https://github.com/pH14/harmony/actions/runs/33280935097)
completed fully green. Its check, both X1 jobs, all six stock-KVM probes,
locked guest-image build and planted entropy negative, four X2 replicas, and
eight X2 hunt replicas all passed. The ordered N6 job first ran the traps-off
image and rejected its observed result, then credited two traps-on reports:
`N6_SWEEP_OK arch=x86_64 table_rows=9 exercised_rows=9 operations=166
runs=2`. The downloaded positive logs are byte-identical at SHA-256
`ed0bc38b77448a7d7d08d7f53ee2349bbd519a28cb962c02893d16881273b583`;
the traps-off log is
`0133377b9c6cf16d8ba53034fe0f0d47d51dc3b1a958d9ca3537be59d72750af`
and visibly records `traps_on:false`, the unconfined counter result, all nine
rows, and the completion marker. Thus x86 satisfies both the negative-first
and exact-row-count clauses on the exact code tree.

Two msr1 diagnostics associated with that x86 failure are explicitly
non-evidence. `harmony-n6-x86-diag-e98eab62-r14.service` used the canonical
compute-exclusive then benchmark-shared ExecStart in `consonance.slice`, with
declared/effective/observed CPUs `2-5`; outer PID 887678 and benchmark PID
887680 held both locks before payload PID 887681 began at
`21:43:19.532485829Z`. It exited before computation because the service path
did not contain Cargo. Corrected unit
`harmony-n6-x86-diag-e98eab62-r15.service` (invocation
`1aacf0baea1e4d009e5bd38ff7377cb0`) began at `21:44:37Z`; outer PID 887873
held compute-exclusive, PID 887875 held benchmark-shared, the cgroup was
`/consonance.slice/harmony-n6-x86-diag-e98eab62-r15.service`, the exact
ExecStart and all three CPU views were `2-5`, and Cargo jobs were four. It
completed at `21:47:56Z` with zero tests because msr1 is aarch64 and the live
x86 test is architecture-gated. It therefore provides only protocol evidence,
not N6 evidence; no further x86 computation was attempted on msr1.

The optimized one-runner harness was syntax-checked natively on arm64 by
`harmony-n6-harness-syntax-dd3-r16.service` (invocation
`624eef653acc4ed3b4ab41dd4cfbd9bd`). The canonical outer PID 900783 held
compute-exclusive and PID 900785 held benchmark-shared before payload PID
900786 began at `23:24:34.813855779Z`; its cgroup was
`/consonance.slice/harmony-n6-harness-syntax-dd3-r16.service`, declared,
effective, and observed CPUs were exactly `2-5`, and strict
`-Wall -Wextra -Werror -fsyntax-only` completed at `23:24:35.141588110Z`.
That is construction evidence only. Signed commit `7252d9e4` carries the
reusable W^X page, per-operation zeroing, once-installed `SIGILL`/`SIGSEGV`/
`SIGTRAP` recovery, and no verifier relaxation. Exact ARM image build
`harmony-n6-arm-build-7252d9e4-r17.service` (invocation
`1debd6580a0e4d67920fbc8924dce2ec`) began at `23:26:06Z`. Compute-exclusive
PID 901000 and benchmark-shared PID 901001 held `35:7` and `35:6` before
payload PID 901002 began at `23:26:06.713260179Z`; its cgroup is
`/consonance.slice/harmony-n6-arm-build-7252d9e4-r17.service`, exact canonical
ExecStart is recorded, declared/effective/observed CPUs are `2-5`, and build
parallelism is four. The local/uploaded payload SHA-256 is
`287f2ba9…0dfe`; exact commit is
`7252d9e46cd9c02f061107d877f4add57ee050c7`. It completed naturally and
successfully at `00:00:54Z`, consumed 1h43m27.897s CPU with a 7 GiB peak,
verified all five manifest entries, and released both locks. The normal Image,
traps-off Image, ordinary initramfs, N6 initramfs, and traps-off N6 initramfs
SHA-256 values are respectively
`314afa30412f3e9ee0022913bb3dbe9ff67971f8dd0a7fcd529a853054f2a9af`,
`da38b6df37863c7595502148be4b8fdc449274c6a861f7a7e068b72f2558e877`,
`b2fbb8021eef6e5e8c0b11a6bb4227fc710fe54cfe9eb2d85d4a58a1e3cb4ab7`,
`3681c063a5e56845137f6ff1951ad59e1471c77e44ed22dc3666192f6a5c9ef9`,
and `0bbe507595edc8f77e5c0404c39e32c3479ac0754de18082d99a6b18ded8d409`.
The five downloaded M1 Max copies reproduce those hashes byte-for-byte.

The standing-rule defect ledger is complete. Issues
[#214](https://github.com/pH14/harmony/issues/214) through
[#227](https://github.com/pH14/harmony/issues/227) record, respectively, the
incomplete arm64 frozen listing, vacuous CPUID expansion, traps-off verifier
ordering, guest-self-enabled TSC trap, omitted arm64 builder patch, x86 kernel
scanner false positive, insufficient scanner diagnostic, CI report-path
collision, initramfs scanner false positive, missing CI KVM permission, serial
marker/terminal ordering, missing `SIGTRAP` recovery, ARM harness event
amplification, and HVF default-deny instruction skip. Each issue names the
finding evidence and signed repair and is closed completed; no defect found in
N6 exists only in this transient work log.

At exact `7df229e3`, the full local repository chain passes: all-feature
build, permission-correct nextest (1,174/1,174; 25 configured skips),
all-target/all-feature Clippy with warnings denied, formatting, and cargo-deny.
The emitted Clippy output contains only Cargo's existing informational warning
about unmatched `clippy.toml` disallowed paths, not a denied code warning.
Cargo-deny passed advisories, bans, licenses, and sources. The first sandboxed
nextest attempt stopped after 405 passes when two telemetry listener tests
received `EPERM`; its local-socket-permission rerun passed. The first
cargo-deny attempt could not lock the sandbox-read-only advisory database; its
permission-correct rerun passed. Neither environment-denied attempt is counted
as a repository gate. The N6 static chain also passes on this tree: generator
self-test reports arm64
9/9 rows and 192 operations and x86 9/9 rows and 166 operations while rejecting
all eight planted defects; fresh listing output is byte-identical to the
committed TSV; both generated assemblies compile; the x86 assembly is entropy
clean; LL/SC quiet/noisy convergence, accumulating divergence, and comparator
negative pass; and the tripwire audit reports three patch hashes, 15 mechanisms,
and a rejected removed-ABI negative.

The exact-`7252d9e4` local release `hvf_boot` was rebuilt and ad-hoc signed with the
repository Hypervisor entitlement; the extracted entitlement contains
`com.apple.security.hypervisor=true`, and the signed executable SHA-256 is
`a28d746b…cd2c`. Its exact-e98 traps-off run began first and emitted four of
nine rows before spending its remaining budget at the PMU row. It was not
stopped: after 111 minutes it exited naturally with
`HVF boot watchdog reached 5000000 events before /init marker`. The incomplete
report receives no credit. Separate signed diagnostic binaries against the
same guest bytes showed all 147 PMU deny completions originated at EL0 and
advanced through the distinct generated read/write operations; no nested EL1
fault was observed. Clockevent tracing also showed normal PPI 27
acknowledge-and-reprogram traffic, not a stuck interrupt. The evidence isolated
the cost to the original harness's roughly nine syscalls per operation plus
deterministic syscall ticks and the resulting timer-service delay. Those
diagnostics were bounded and stopped after their stated trace targets; they
receive no sweep credit. Signed `7252d9e4` therefore reuses one result mapping,
one W^X JIT mapping, and once-installed fault handlers while preserving
per-operation zeroing and watchdog bounds. No ARM guest result is credited
until a final exact-tree traps-off image completes and is rejected before the
two normal runs.

The exact-`7252d9e4` traps-off run began at `00:02:56Z` and was preserved to
its natural completion. It again emitted only the first four rows, then exited
at `02:19:30Z` with `HVF boot watchdog reached 5000000 events before /init
marker`. The incomplete 5,468-byte transcript is retained but receives no N6
credit. Read-only host inspection near the end proved this was an active HVF
loop rather than a stuck wrapper: PID 68229 had accumulated 109m54s CPU after
2h10m elapsed. Review identified the remaining amplification: two
`mprotect` syscalls for each of 192 operations. Issue
[#228](https://github.com/pH14/harmony/issues/228) records the exact failure.

Signed repair `7df229e3` pre-emits every generated operation into its own page
while the complete mapping is RW, performs one whole-mapping transition to RX,
and thereafter executes only from those immutable pages. Per-operation result
zeroing, `sigsetjmp` recovery, operation identity, W^X, and the unchanged host
watchdog remain intact; no expected result or verifier rule changes. The
generator positives, all eight planted sweep negatives, LL/SC positive and
negatives, formatting, and diff hygiene pass locally. Native arm64 strict-C
unit `harmony-n6-harness-syntax-preemit-r19.service` (invocation
`9f6ca13e5d57474995f65ea4a198dc03`) used the canonical nested-flock ExecStart
in `consonance.slice`: compute-exclusive PID 946577, benchmark-shared PID
946579, payload PID 946580, both locks held at `02:23:45.357095018Z`, lock
identities `35:7` then `35:6`, and declared/effective/observed CPUs exactly
`2-5`. The uploaded C and generated-header hashes were
`3e2df334…9709` and `e8074448…35a2`; `-Wall -Wextra -Werror -fsyntax-only`
passed and the unit completed at `02:23:45.665563893Z`.

Exact ARM build `harmony-n6-arm-build-7df229e3-r20.service` (invocation
`35f1ba110c764a06afee2f71400948bb`) began at `02:25:55Z`. Its ExecStart is
the frozen single-PID nested-flock form in `consonance.slice`; compute-exclusive
PID 946858 and benchmark-shared PID 946860 acquired `35:7` then `35:6` before
payload PID 946861 began at `02:25:56.026265495Z`. Declared `AllowedCPUs`,
`EffectiveCPUs`, and observed `Cpus_allowed_list` are exactly `2-5`, build
parallelism is four, and local/uploaded payload SHA-256 is
`9980f5bf…c0e5`. It built exact
`7df229e3e0beea0dfb1b633f7122c45f3182b2ab`, completed naturally at
`02:59:58Z`, consumed 1h42m48.728s CPU with a 6.6 GiB peak, verified all five
manifest entries, and released both locks. Image, traps-off Image, ordinary
initramfs, N6 initramfs, and traps-off N6 initramfs hashes were
`314afa30…a9af`, `da38b6d…8e877`, `b2fbb802…b4ab7`, `c0f3bbb6…b637`, and
`32968c7c…e78b`; the downloaded copies matched. Exact x86-vtime run
[33287917944](https://github.com/pH14/harmony/actions/runs/33287917944)
targeted the same commit but was superseded by the correction below.

The exact-`7df229e3` entitled HVF negative booted, then emitted
`N6_GUEST_FAIL execute-operation-has-no-code` before `N6_GUEST_BEGIN`.
Pre-emission had traversed the intentionally code-less `mask-and-audit`
entropy entries as if every table operation were executable. The failed guest
had entered its permanent fail loop, so it was stopped after the conclusive
marker; its transcript receives no credit. Issue
[#229](https://github.com/pH14/harmony/issues/229) records the failure. Signed
commit `8d84dce7` skips entries whose start and end are both null during
pre-emission, rejects inconsistent half-null entries, and retains the runtime
null-code guard whenever an `execute` row invokes an operation.

Corrected native arm64 strict-C unit
`harmony-n6-harness-syntax-preemit-r21.service` (invocation
`55610638ad5043f6bd0fc181f548e7c0`) used the canonical nested-flock ExecStart
in `consonance.slice`: compute-exclusive PID 979201, benchmark-shared PID
979203, payload PID 979204, both locks held at `03:04:46.211306443Z`, lock
identities `35:7` then `35:6`, and declared/effective/observed CPUs exactly
`2-5`. The uploaded C and generated-header hashes were
`9c873e18…5f08` and `e8074448…35a2`; strict `-Werror` syntax checking passed
and the unit completed at `03:04:46.501564188Z`.

Exact ARM build `harmony-n6-arm-build-8d84dce7-r22.service` (invocation
`682e942ffb6743249d93bf59bf4d5a24`) began at `03:06:22Z`. Its ExecStart is
the canonical nested-flock form in `consonance.slice`; compute-exclusive PID
979432 and benchmark-shared PID 979433 acquired `35:7` then `35:6` before
payload PID 979434 began at `03:06:22.877596649Z`. Declared/effective/observed
CPUs are exactly `2-5`, build parallelism is four, and local/uploaded payload
SHA-256 is `17275ac7…f735`. It built exact
`8d84dce7cec68dffb83119a31e0bd490807bcb3f`, completed naturally at
`03:40:18Z`, consumed 1h42m35.606s CPU with a 6.6 GiB peak, verified all five
manifest entries, and released both locks. Image, traps-off Image, ordinary
initramfs, N6 initramfs, and traps-off N6 initramfs SHA-256 values are
`314afa30412f3e9ee0022913bb3dbe9ff67971f8dd0a7fcd529a853054f2a9af`,
`da38b6df37863c7595502148be4b8fdc449274c6a861f7a7e068b72f2558e877`,
`b2fbb8021eef6e5e8c0b11a6bb4227fc710fe54cfe9eb2d85d4a58a1e3cb4ab7`,
`65fc2765890c4dd3a74e94f22afc563cff0116e7083ee0bd2e67b7d960589f4b`,
and `28214beb248e3a44618bf475bfd1d3dff348e948fb8260202f6d65588796e239`;
the five downloaded M1 Max copies reproduce those hashes. Exact x86-vtime run
[33289463105](https://github.com/pH14/harmony/actions/runs/33289463105)
completed fully green against the same commit: check, both X1 jobs, all six
stock-KVM probes, locked guest image, ordered N6 job, all four X2 replicas,
and all eight X2 hunt replicas passed. Its N6 verdict is
`N6_SWEEP_OK arch=x86_64 table_rows=9 exercised_rows=9 operations=166
runs=2`. The two positive reports are byte-identical at SHA-256
`4f87c4d9ba6c04ae8de903a2ebfec4239ee9c2a85446887bf08926b4d8b432c9`;
the traps-off report hash is
`ff2f0fdb3c99e98f2db774c6a00c1cfddd3dea2731273c57877933ad6d141f2e`.
The ordered negative visibly records `traps_on:false`, the escaped counter
result, every row, and the completion marker before the two positives are
credited.

The exact-`8d84dce7` M1 Max traps-off run was preserved to its natural
five-million-event watchdog after nearly two hours. It emitted the first four
rows; report SHA-256 is
`781a728f5f16a70636fc1bd2b81ebe438900ae01591f80b1aacf2d23ec832318`.
Running the exact verifier over that transcript rejects at line 92 with
`arm64-virtual-counter escaped the guest trap policy`; verdict SHA-256 is
`bee1df35692d43347f502ea0230aa76cb04f85659557175de23c90bfbb64723a`.
That proved the then-current negative check could see the exposed counter, but
the subsequent traps-on run exposed a deeper verifier defect before reaching
the long PMU row: with `traps_on:true`, `MRS CNTVCT_EL0` returned a value while
`MRS CNTVCTSS_EL0` signalled. The run was already invalid under the verifier's
signal-only rule and was stopped; its 5,466-byte transcript receives no
positive credit.

Issue [#231](https://github.com/pH14/harmony/issues/231) records why the
signal-only rule is wrong. Reachable N1/HVF cannot provide an EL2 CNTVCT trap;
the owned kernel instead denies direct EL0 access and emulates the ordinary
counter read from the deterministic pvclock page. The positive obligation is
therefore byte equality across two same-seed handled results, not a fabricated
`SIGILL` requirement. The repair removes only `arm64-virtual-counter` from the
signal-only set and adds a dedicated two-independent-run traps-off verifier:
both reports must identify the exact early counter row, mark traps off, execute
both operations, expose at least one value, and differ. A planted pair with
identical values is rejected. Physical counters, live timer programming, and
all 147 PMU forms remain signal-only; table rows, operations, positive
completeness, and same-seed comparison are unchanged. The updated self-test
passes 9/9 rows and 192 operations on arm64 and 9/9 rows and 166 operations on
x86 while rejecting the existing planted defects plus the repeated-value
traps-off negative.

The first exact-`e23d6064` ARM build launch,
`harmony-n6-arm-build-e23d6064-r24.service`, is explicit non-evidence. It did
acquire compute-exclusive PID 1074091 and benchmark-shared PID 1074093 in the
required order before payload PID 1074094, in `consonance.slice` with all
three CPU views `2-5` and jobs four, but the payload contained an incorrectly
expanded commit ID and exited before creating a worktree or starting any
build. Corrected unit `harmony-n6-arm-build-e23d6064-r25.service` (invocation
`18322f21007f43c4b4c4f9ddf116e48a`) began at `12:40:11Z` with the canonical
one-PID ExecStart. Compute-exclusive PID 1074299 and benchmark-shared PID
1074301 acquired lock identities `35:7` then `35:6` before payload PID 1074302
began; both were held by `12:40:11.856905047Z`. Its cgroup is
`/consonance.slice/harmony-n6-arm-build-e23d6064-r25.service`, declared
`AllowedCPUs`, `EffectiveCPUs`, and observed `Cpus_allowed_list` are exactly
`2-5`, Cargo jobs are four, and uploaded payload SHA-256 is
`0e307d4444f355d94bc9499d716a9c8eca98e5ef29d8a959452a6b17988289a7`.
It checked out exact `e23d60646f60dd82a394e2dff399d79463cb95a0`; completion
was natural at `13:14:47Z` after 1h42m36.641s CPU and a 6.3 GiB peak, all
manifest entries verified, and both locks were released. Image, traps-off
Image, ordinary initramfs, N6 initramfs, and traps-off N6 initramfs SHA-256
values are
`314afa30412f3e9ee0022913bb3dbe9ff67971f8dd0a7fcd529a853054f2a9af`,
`da38b6df37863c7595502148be4b8fdc449274c6a861f7a7e068b72f2558e877`,
`b2fbb8021eef6e5e8c0b11a6bb4227fc710fe54cfe9eb2d85d4a58a1e3cb4ab7`,
`5c1cc8497b57b022eb39c7e82ec6e2823799f9f6386c67be591ab64d8c8e2655`,
and `c07bf6ee718fbd774326fee4bc16c08c71d3f5c86dd028acc0c6314d51793970`;
the downloaded M1 Max copies reproduce those hashes.

Two independent exact-`e23d6064` M1 Max traps-off witnesses then stopped
immediately after the complete ordinary virtual-counter row, before the
physical-counter row. Each records `traps_on:false`, both generated operations,
one returned counter value, and one `SIGILL`; their counter values differ as
required. The dedicated verifier reports `N6_TRAPS_OFF_REJECTED arch=arm64
row=arm64-virtual-counter operations=2 runs=2`. Witness and verdict SHA-256
values are `fa03b7ce101b08653e2bf8394b65fb6a06cbbdc91f28d594ac60e38cf6ef6793`,
`5d7e02ab31b15f24af4649cb900661baab854e8fa8d0a94313559205cd666640`,
and `f4857a87772605d0cb31949ee8441d6a20790c87644df598555862e99ce9f5e7`.
An initial marker choice matched the row being collected and stopped within
its JSON; that truncated attempt is non-evidence and was replaced rather than
credited.

The first exact-`e23d6064` traps-on M1 Max sweep then ran naturally to the
raised 10,000,000-event watchdog after about four hours. It again completed
only the first four rows and never emitted the `arm64-pmu` row or terminal
marker; its 5,466-byte transcript has SHA-256
`42b8fe799f66a76dcf690d9f7b43601241906664c1a751db3af2919c838acf65` and
receives no positive credit. This disproves the assumption that merely
doubling the event bound closed issue #228. Because the guest buffered a
whole row before emitting anything, the transcript could not distinguish a
single blocking PMU operation from slow progress through the frozen matrix.
The repair at `5c2a97793b1cd8e8c7ec70bdfd2d06e490289fa9` therefore adds a
deterministic `N6_OPERATION` record after every generated operation, including
architecture, row, ordinal, exact frozen operation name, and result, while
retaining the final `N6_ROW` and 9/9 verifier contract. The generator/verifier
self-test remains green on both architectures and rejects every planted
negative. Issue #228 records the failed exact run and repair rationale; no
identical retry is attempted.

Exact repair build `harmony-n6-arm-build-5c2a9779-r27.service` (invocation
`8baa34ea07bb412dafdc71b184f6c5cc`) started at `17:28:04Z` with canonical
ExecStart `/usr/bin/flock --exclusive
/run/lock/harmony-msr1-consonance-compute.lock /usr/bin/flock --shared
/run/lock/harmony-msr1-benchmark.lock
/root/harmony-n6-5c2a9779-arm-build-r27-payload.sh`. Compute-exclusive PID
1136035 acquired lock identity `35:7`, benchmark-shared PID 1136036 acquired
`35:6`, and payload PID 1136037 began only after both were held at
`17:28:04.054386901Z`. The unit is in
`/consonance.slice/harmony-n6-arm-build-5c2a9779-r27.service`; declared
`AllowedCPUs`, observed `EffectiveCPUs`, and `Cpus_allowed_list` are all
exactly `2-5`; Cargo jobs are four. Local and uploaded payload SHA-256 is
`62ab6cb394034d2e5aee30623fa4ca499aca4fcf3a1b2961c8226716934782e3`.
It checked out exact `5c2a97793b1cd8e8c7ec70bdfd2d06e490289fa9`; completion
was natural at `18:02:01Z` after 1h42m42.730s CPU and a 6.4 GiB peak, every
manifest entry verified, and both locks were released. Image, traps-off Image,
ordinary initramfs, N6 initramfs, and traps-off N6 initramfs SHA-256 values are
`314afa30412f3e9ee0022913bb3dbe9ff67971f8dd0a7fcd529a853054f2a9af`,
`da38b6df37863c7595502148be4b8fdc449274c6a861f7a7e068b72f2558e877`,
`b2fbb8021eef6e5e8c0b11a6bb4227fc710fe54cfe9eb2d85d4a58a1e3cb4ab7`,
`b28766ad5ce02551941bca68c8fb5efd5d08a07c3c594c80d6c7fd7c6879f8c1`,
and `547b4306a70bd5511e2618fa4019d6a3d4b161c4671f47c9a02ba44fa1058f60`;
the five downloaded M1 Max copies reproduce those hashes. The ad-hoc-signed
release `hvf_boot` remains byte-identical to the exact-`e23d6064` build at
SHA-256 `a28d746baabac8c19d2c75bde6d652570255ab5b99ca5f3af5ff0c87b32ecd2c`;
the only intervening source change is the guest C file embedded in the new
initramfs.

The first marker-bounded exact-`5c2a9779` M1 diagnostic selected PMU operation
1 but was intentionally stopped after nine minutes: it had emitted the first
three operation records and then spent the remainder attempting to emit the
ordinary virtual-counter row. Repeating the already-disproved ten-million-
event ceiling to its natural four-hour watchdog would not have distinguished
a PMU operation and receives no credit. A replacement bounded run selected
virtual-counter operation 2 and stopped successfully at raw event 41,444
(41,018 portable events), proving operation execution and boot are not the
cost center. Its 6,350,436-byte normalized trace has SHA-256
`d722cbab10689581b85e345136cc23a4fcc0c729eb305302cd0f0de9ef07e007`.
Together with the prior run's stall immediately after the same short operation
record, this isolates the no-UART-interrupt ARM substrate's failure mode to the
subsequent multi-result JSON line; the 147-result PMU row could never complete
within an honest bound.

The repair makes each bounded, generated `N6_OPERATION` record authoritative
and changes `N6_ROW` into a compact completion record. The verifier reconstructs
each row's results only after requiring the exact architecture, row, ordered
ordinal/total, frozen operation name, non-empty result, and compact row
completion; missing, duplicate, reordered, renamed, or orphaned operations
fail closed. Same-seed comparison remains byte-for-byte over the reconstructed
result lists, signal-only rows remain signal-only, all positive execute rows
now require `traps_on:true`, and mask-and-audit rows retain their exact shape.
The operation buffer's worst-case encoded length is checked against its 256-byte
guest bound by the self-test. The self-test again passes arm64 9/9/192 and x86
9/9/166 while rejecting missing operations, result mutation, traps-off escape,
repeated traps-off values, and visible entropy.

Native arm64 strict-C unit `harmony-n6-compact-syntax-r28.service` used the
canonical nested-flock ExecStart in `consonance.slice`. Compute-exclusive PID
1170029 acquired `35:7`, benchmark-shared PID 1170030 then acquired `35:6`, and
payload PID 1170031 began only after both were held at
`18:47:01.057024443Z`. Declared `AllowedCPUs`, observed `EffectiveCPUs`, and
`Cpus_allowed_list` were exactly `2-5`; jobs were capped at four. Uploaded
guest C, generated header, and payload SHA-256 values were `70de5273…1364`,
`e8074448…35a2`, and `15e744f7…07d`; `-std=gnu11 -Wall -Wextra -Werror
-fsyntax-only` passed and the unit completed naturally in 374 ms. The local
macOS x86 syntax attempt is non-evidence because that host lacks Linux
`sys/auxv.h`; the exact Linux x86 workflow remains the portability gate.

Signed compact-report commit `14d24c8da2709b9f767515b299f753cff0824126`
is pushed. Exact repair build `harmony-n6-arm-build-14d24c8d-r29.service`
(invocation `df97b8ae77944f12bb55ec8ab6377656`) began at `18:49:33Z`
with the canonical one-PID nested-flock ExecStart in `consonance.slice`.
Compute-exclusive PID 1170361 acquired lock identity `35:7`, benchmark-shared
PID 1170363 then acquired `35:6`, and payload PID 1170364 began only after both
were held at `18:49:33.165322316Z`. Declared `AllowedCPUs`, observed
`EffectiveCPUs`, and `Cpus_allowed_list` are exactly `2-5`; Cargo and make
parallelism are four. Local and uploaded payload SHA-256 is
`f536154907a182a0c4639d3016822cedad5a60ff8d191539ba458a54d26f5682`.
The unit fetched and checked out exact `14d24c8d`; completion and artifact
was natural at `19:23:30Z` after 1h42m42.952s CPU and a 6.6 GiB peak, every
manifest entry verified, and both locks were released. Image, traps-off Image,
ordinary initramfs, N6 initramfs, and traps-off N6 initramfs SHA-256 values are
`314afa30412f3e9ee0022913bb3dbe9ff67971f8dd0a7fcd529a853054f2a9af`,
`da38b6df37863c7595502148be4b8fdc449274c6a861f7a7e068b72f2558e877`,
`b2fbb8021eef6e5e8c0b11a6bb4227fc710fe54cfe9eb2d85d4a58a1e3cb4ab7`,
`f90871c2e2f201b7562adcd1acbbdc6a64f78e0b1e0bd88763f1ca7847b47223`,
and `114ff4de9fb72e85c3fbe488e49a21e27c0bdb0ae89b87ae2e3a0a6fa0dbec9b`;
the five downloaded M1 Max copies reproduce those hashes.

The first compact exact-`14d24c8d` M1 traps-off diagnostic emitted the begin
marker, counter-frequency operation and compact row, then both ordinary
virtual-counter operation records. It again stopped making UART-visible
progress before the compact virtual-counter row and was intentionally stopped;
it receives no negative credit. This disproves the narrower long-line diagnosis:
the ruled ARM substrate's absent UART interrupt delivery fills the userspace
TTY queue cumulatively after three report writes, so shortening individual
records cannot make the report transport live. The repair selects `/dev/kmsg`
before the first N6 marker, using the nonblocking kernel log ring rather than
the interrupt-dependent userspace TTY; it retains stdout only as a fail-safe if
opening or writing the kernel log fails. Host marker matching is unchanged
because it already searches each serial line for the N6 prefix.

Native arm64 strict-C unit `harmony-n6-kmsg-syntax-r30.service` used canonical
compute-exclusive PID 1202911, benchmark-shared PID 1202912, then payload PID
1202913 in `consonance.slice`; both lock identities `35:7` then `35:6` were
held at `19:31:01.086216555Z`. Declared/effective/observed CPUs were exactly
`2-5`, with jobs four. Uploaded guest C, generated header, and payload SHA-256
values were `623e3d75…dc5a`, `e8074448…35a2`, and `90536731…6f92`;
`-std=gnu11 -Wall -Wextra -Werror -fsyntax-only` passed and the unit completed
naturally in 368 ms.

Signed kmsg transport commit `393d13ee3b5a2efaa195f2ba1b46d99a1e4f843a`
is pushed. Exact build `harmony-n6-arm-build-393d13ee-r31.service`
(invocation `267c0b12a34d45aca79aaa1ae78d6be4`) began at `19:33:01Z`
with the canonical nested-flock ExecStart in `consonance.slice`.
Compute-exclusive PID 1203194 acquired `35:7`, benchmark-shared PID 1203196
then acquired `35:6`, and payload PID 1203197 began only after both were held
at `19:33:01.790913499Z`. Declared/effective/observed CPUs are exactly `2-5`;
Cargo and make parallelism are four. Local and uploaded payload SHA-256 is
`221bef867a6c0b951fb9504a9b8e9bd8265adb8b5344c8bfbd3d44889b3a3e2c`.
The unit fetched and checked out exact `393d13ee`; completion and artifact
hashes are recorded below.

That r31 build completed naturally at `20:07:02Z`, after 1h42m49.242s CPU and
a 6.5 GiB peak; every manifest entry verified and both locks were released.
Image, traps-off Image, ordinary initramfs, N6 initramfs, and traps-off N6
initramfs SHA-256 values are respectively
`314afa30412f3e9ee0022913bb3dbe9ff67971f8dd0a7fcd529a853054f2a9af`,
`da38b6df37863c7595502148be4b8fdc449274c6a861f7a7e068b72f2558e877`,
`b2fbb8021eef6e5e8c0b11a6bb4227fc710fe54cfe9eb2d85d4a58a1e3cb4ab7`,
`0f99522f1a7351e73aaf4b40792a07e44da13e532700cdbfd406732598ef4e01`,
and `0e8586a584eac05b61c512fa2dee786f2a9531202573f9c5b8ba7b8b0cbb02ce`.
The exact images and ordinary initramfs were downloaded and hash-checked. The
two local N6 initramfs names were accidentally overwritten by diagnostic
preflight copies; those copies were immediately renamed and receive no exact-
tree credit. The authoritative exact N6 initramfses remain intact under
`/root/harmony-n6-393d13ee-r31-output` on msr1.

The exact r31 traps-off boot disproved `/dev/kmsg` as a solution: it emitted
the counter row and both virtual-counter operation records, then again blocked
before the virtual-counter completion record. Kernel-log writes synchronously
flush through the same interrupt-less console on this ruled ARM substrate, so
the run was stopped and receives no N6 credit. Direct PL011 diagnostics were
also rejected rather than promoted into the guest. A no-libc syscall probe
showed that opening `/dev/mem` fails before any mapping is possible (refined
probe exit value `11`); no `/dev/mem` node or direct-MMIO code remains in the
tree. Units `harmony-n6-direct-uart-r32.service`,
`harmony-n6-direct-uart-r33.service`, and
`harmony-n6-pl011-channel-r35.service` used the canonical compute-exclusive
then benchmark-shared lock form in `consonance.slice`, with exact declared,
effective, and observed CPUs `2-5`; they were diagnostics only and are not N6
evidence.

The resulting ARM transport is deliberately bounded: the guest executes all
192 table-generated operations without intermediate console writes, hashes
each ordered row's identifier, claim, operation names, and results into a
64-bit FNV-1a-style digest, and emits one final summary containing all nine
digests plus exact row, operation, trap-row, and mask-row counts. A traps-off
image emits one early virtual-counter witness containing its digest and an
explicit exposed-live-state bit. The verifier reconstructs the frozen rows
from those ordered digests, requires two same-seed summaries to match, and
rejects missing summaries, a planted row mismatch, traps-off policy, a
repeated traps-off witness, and visible entropy. Its self-test passes all
those positives and planted negatives for ARM and retains the detailed x86
protocol and negatives unchanged.

The first native strict-C attempt for this design,
`harmony-n6-summary-syntax-r36.service` (invocation
`4e4dea13fce443f98a45433b33ec13bf`), correctly failed with warnings denied:
the normal build saw a traps-off-only `exposed` variable as set but unused.
It used canonical compute-exclusive PID `1238993`, benchmark-shared PID
`1238994`, then payload PID `1238996`; both lock identities `35:7` then `35:6`
were held at `20:39:33.828124279Z`, with cgroup `consonance.slice`, exact
declared/effective/observed CPUs `2-5`, and jobs four. It is invalidated and
not counted. The variable was narrowed to the traps-off build and replacement
unit `harmony-n6-summary-syntax-r37.service` (invocation
`58c269d5b8184b0b8ec6ba9147582bd1`) passed both normal and traps-off
`-std=gnu11 -Wall -Wextra -Werror -fsyntax-only` checks. Its canonical PIDs
were `1239154`, `1239155`, and `1239156`; locks `35:7` then `35:6` were held
at `20:40:24.195273040Z`; slice and all three CPU observations were exactly
`consonance.slice` and `2-5`. Guest C and generated-header SHA-256 values were
`8e497b7b…23fe` and `83c5bfcc…a37e`.

Signed source commit `843305f9abdc60763331d4b9972b0817cebee7bb` is
pushed. Exact build `harmony-n6-arm-build-843305f9-r38.service` (invocation
`92cdf6f89b224c95b1980df8d2821e02`) began at `20:41:57Z`. Its exact
canonical ExecStart has compute-exclusive PID `1239379`, benchmark-shared PID
`1239381`, then payload PID `1239382`; lock identities `35:7` then `35:6` were
both held before payload at `20:41:57.227604865Z`. Cgroup, declared,
effective, and observed affinity are exactly `consonance.slice` and CPUs
`2-5`, Cargo/make parallelism is four, and the detached checkout attested exact
`843305f9`.

r38 completed naturally at `21:15:44Z`, consuming 1h42m8.531s CPU with a
6.6 GiB peak; all five manifest entries verified and both locks were released.
Image, traps-off Image, ordinary initramfs, compact N6 initramfs, and compact
traps-off N6 initramfs SHA-256 values are respectively
`314afa30412f3e9ee0022913bb3dbe9ff67971f8dd0a7fcd529a853054f2a9af`,
`da38b6df37863c7595502148be4b8fdc449274c6a861f7a7e068b72f2558e877`,
`b2fbb8021eef6e5e8c0b11a6bb4227fc710fe54cfe9eb2d85d4a58a1e3cb4ab7`,
`7e23c556b2810090d5b05d9f0764d9b8790839eb2e5101878bc56ff9b5be5c25`,
and `86b0453269a29e61f4f6ea530bcc2094f6ea31d8013328b6fcb86686cd40c8ab`.
Because `46a75b20` later repaired the x86 build, these otherwise valid bytes
are diagnostic rather than final-tree evidence.

Final exact-tree KVM campaign `harmony-n6-arm-kvm-843305f9-r39.service`
(invocation `da9b69fe4cd04e00b27b481d0aa4b2bd`) was admitted at
`20:49:11Z` behind r38 as the required canonical one-PID waiter. PID `1247585`
is the sole cgroup member and its exact argv is compute-exclusive first,
benchmark-shared second, then the payload; its journal contains no payload
output while r38 owns compute. The unit is in `consonance.slice` with declared
`AllowedCPUs=2-5`. It was stopped cleanly at `20:58:34Z` without ever acquiring
compute or starting its payload after exact x86 CI found that `digest_text`,
used only by the ARM summary, was not architecture-gated and therefore failed
the x86 N6 guest's warnings-denied build. r39 is explicitly invalidated and
receives no compute or N6 credit; active r38 was preserved untouched.

Signed repair commit `46a75b20ee3345d0bcaf58e0ae5a4f20f3ed23fd`
adds the missing arm64 gate around that helper. The Mac lacks an x86 Linux C
sysroot, so its attempted native cross-syntax check failed before parsing the
guest at missing system header `sys/auxv.h` and is not counted. Linux CI run
`33334339950` is the meaningful negative: all ordinary checks and probes
passed, the generated self-tests passed, and the locked x86 image build then
failed exactly at `digest_text defined but not used`; ordered live N6 and X2
jobs were correctly skipped. A new exact Linux CI run is attached to the
repair commit.

Corrected exact ARM build `harmony-n6-arm-build-46a75b20-r40.service`
(invocation `725b64e06c5547d9a80d3a74c8009391`) was admitted at
`20:59:10Z` behind preserved r38 as a canonical one-PID compute-lock waiter.
PID `1256639` is the sole cgroup member, with exact argv compute-exclusive
first, benchmark-shared second, then the payload; its journal contains no
payload output and its cgroup is `consonance.slice` with declared
`AllowedCPUs=2-5`. After r38 released compute, the same outer PID acquired
compute-exclusive `35:7`, benchmark-shared PID `1271321` acquired `35:6`, and
payload PID `1271323` began only after both were held at
`21:15:44.525929941Z`. Declared/effective/observed affinity is exactly `2-5`,
jobs are four, and the detached worktree attested exact `46a75b20`. Completion
was natural and successful at `21:49:40Z`, after 1h42m34.002s CPU with a
6.6 GiB memory peak; all five manifest entries passed and both locks were
released. The Image, traps-off Image, ordinary initramfs, N6 initramfs, and
traps-off N6 initramfs SHA-256 values are respectively
`314afa30412f3e9ee0022913bb3dbe9ff67971f8dd0a7fcd529a853054f2a9af`,
`da38b6df37863c7595502148be4b8fdc449274c6a861f7a7e068b72f2558e877`,
`b2fbb8021eef6e5e8c0b11a6bb4227fc710fe54cfe9eb2d85d4a58a1e3cb4ab7`,
`9cc6cc5ac203d54768c5f21726c4ad460f77a15e0c59a4cc7fb6204a661793fc`,
and `b3474757138f0c2b7dad932eb816c6ec45ccfc885732e7af36829407c2129c20`.
The later live-test-only `5f25dd3e` descendant makes r40 diagnostic; none of
these bytes receives final N6 credit.

Exact CI run `33335103825` then built the corrected locked x86 image and passed
all static N6 controls, but its ordered live negative exposed a second gate
defect. The Rust test invoked the full positive verifier on one traps-off
report; that verifier rejected the earlier `x86-cpuid` row's
`traps_on:false`, masking the intended `x86-tsc` live-counter escape. The job
failed honestly with `negative failed for the wrong reason: x86-cpuid traps
are off`, and no positive was credited. [Issue #233](https://github.com/pH14/harmony/issues/233)
records the failure. Signed commit
`5f25dd3e817e6f2843079318440ceb3823d88cfa` repairs the live test by booting
two independent traps-off guests, invoking the existing dedicated
`verify-traps-off` command, requiring its `N6_TRAPS_OFF_REJECTED` TSC-divergence
verdict, and only then running two positives. The target compiles locally,
formatting passes, and all ARM/x86 verifier positives and planted negatives
remain green. A new exact CI run is attached to that commit.

Final exact-tree ARM build `harmony-n6-arm-build-5f25dd3e-r42.service`
(invocation `c502eadf8b8443ac8e2bd386dcfa432d`) was admitted at
`21:42:28Z` behind preserved r40 as a canonical one-PID waiter. PID `1298241`
is its only cgroup member and its complete argv is compute-exclusive first,
benchmark-shared second, then the payload. Its journal contains no payload
output; cgroup and declared CPU set are `consonance.slice` and exactly `2-5`.
r40 is now diagnostic because the standing re-run rule requires final-tree
evidence. When r40 released compute, the same outer PID acquired
compute-exclusive `35:7`, benchmark-shared PID `1303397` acquired `35:6`, and
payload PID `1303399` began only after both were held at
`21:49:40.490139134Z`. The cgroup is
`/consonance.slice/harmony-n6-arm-build-5f25dd3e-r42.service`; declared,
effective, and observed affinity are exactly `2-5`, jobs are four, and the
detached checkout attested exact
`5f25dd3e817e6f2843079318440ceb3823d88cfa`. Completion and artifact hashes
were natural and successful at `22:23:41Z`, after 1h42m42.754s CPU with a
6.6 GiB memory peak; every manifest entry passed and both locks were released.
The Image, traps-off Image, ordinary initramfs, N6 initramfs, and traps-off N6
initramfs SHA-256 values are respectively
`314afa30412f3e9ee0022913bb3dbe9ff67971f8dd0a7fcd529a853054f2a9af`,
`da38b6df37863c7595502148be4b8fdc449274c6a861f7a7e068b72f2558e877`,
`b2fbb8021eef6e5e8c0b11a6bb4227fc710fe54cfe9eb2d85d4a58a1e3cb4ab7`,
`15208f2fa8d74507f7dc0d04db338e048f13d486d4168ac8fb68a1ebd0a987db`,
and `153304c82221a713f4a6b5973ce70b8927c8baad15ca5a2d14bd06187a5be385`.

The first exact r42 live attempts then exposed one final gate defect rather
than receiving credit. Rebuilding `hvf_boot` removed its Hypervisor entitlement,
so two local invocations failed before guest creation at `hv_vm_create`; those
logs are retained and invalid. After applying the repository entitlement, two
fresh HVF traps-off boots each reached the marker, but the host runner stopped
as soon as the marker prefix appeared, before the rest of its serial line had
arrived. Both logs therefore contained the prefix joined directly to the host
oracle summary, and `verify-traps-off` correctly rejected them with “expected
exactly one ARM traps-off witness, found 0”; no positive began. Exact KVM unit
`harmony-n6-arm-kvm-5f25dd3e-r43.service` (invocation
`a75c0f6b6ab849b2b711b8d9974de030`) independently reproduced the same failure
after two boots. It held compute-exclusive PID `1335629` on `35:7`,
benchmark-shared PID `1335631` on `35:6`, and payload PID `1335632` from
`22:24:39.767638474Z` through natural failure at `22:33:37Z`; cgroup,
declared/effective/observed CPU set, and jobs were respectively
`consonance.slice`, exactly `2-5`, and four. The failed unit consumed
14m05.906s CPU with a 902.7 MiB peak and then released both locks.
[Issue #234](https://github.com/pH14/harmony/issues/234) records the defect.

Signed commit `61af860239214748a6a5e35718bfa5ab4f3b773a` repairs both ARM
boot runners by recognizing a configured marker only within a complete
newline-terminated serial line. Each binary carries a planted partial-line
test plus its complete-line positive; both pass, as do the full build,
all-target/all-feature Clippy, formatting, and diff hygiene. An entitled local
diagnostic using the repaired runner and r42 bytes proves the mechanism before
spending another image build: two traps-off runs produced distinct complete
virtual-counter witnesses and the dedicated rejection verdict, then two
traps-on runs produced byte-identical 9/9-row, 192-operation reports. Their
traps-off log hashes are `ee19c5dc…e082` and `105767fb…87a4`; both positive
logs hash to `712c8549…f607`. This is diagnostic only because the standing
exact-tree rule requires artifacts built from the repaired commit.

Exact repaired-tree build `harmony-n6-arm-build-61af8602-r45.service`
(invocation `c803f743148746d5b1812a64032185ac`) began at `22:34:18Z` with
the canonical ExecStart. Compute-exclusive PID `1338023`, benchmark-shared
PID `1338025`, and payload PID `1338026` held `35:7` then `35:6` from
`22:34:18.853484125Z`; its cgroup is `consonance.slice`, declared,
effective, and every observed CPU list are exactly `2-5`, parallelism is four,
and the detached checkout attests exact `61af8602`. Completion and artifact
were natural and successful at `23:08:10Z`, after 1h42m25.272s CPU with a
6.5 GiB memory peak; all five manifest entries passed and both locks were
released. The Image, traps-off Image, ordinary initramfs, N6 initramfs, and
traps-off N6 initramfs SHA-256 values are respectively
`314afa30412f3e9ee0022913bb3dbe9ff67971f8dd0a7fcd529a853054f2a9af`,
`da38b6df37863c7595502148be4b8fdc449274c6a861f7a7e068b72f2558e877`,
`b2fbb8021eef6e5e8c0b11a6bb4227fc710fe54cfe9eb2d85d4a58a1e3cb4ab7`,
`7e6e737fa37980f9cdc96332b1ab5bd07e6846e777b3c9056f76d2d8c9d96afc`,
and `d7ef40c190c0f1cb2954fbd97125163d60c7afa097e47d206e054da94af7ed99`.

Final exact-artifact M1 Max run r48 used the entitlement-signed `61af8602`
`hvf_boot`. Its two traps-off boots completed first and the dedicated verifier
rejected them for distinct complete `arm64-virtual-counter` witnesses; their
log hashes are `34e1a0aa6ee5baae6211601739c8c5512b558b9e391bc0dfab446b74d3637f29`
and `6723b2554efc2c57aecd3331e0e736a701a8923e4bc1368712d47105cd8dd817`.
Only after that verdict did two traps-on boots execute all 9/9 rows and 192
operations; both complete reports are byte-identical with SHA-256
`9b55f34d6daef9028832bef1f8cb08ad1cca2a5a145d0d7788e7e22286614095`.
The negative and positive verdict files hash to `f4857a87…9f5e7` and
`1a05e5f2…72cb`, respectively.

Exact KVM unit `harmony-n6-arm-kvm-61af8602-r47.service` (invocation
`af947efc891a450eafe9b70b28768f98`) began at `23:09:00Z` with canonical
compute-exclusive PID `1370123`, benchmark-shared PID `1370125`, and payload
PID `1370126`; lock identities are `35:7` then `35:6`, cgroup is
`consonance.slice`, every declared/effective/observed CPU list is exactly
`2-5`, and Cargo parallelism is four. Its two traps-off boots completed and
the negative verifier emitted `N6_TRAPS_OFF_REJECTED` before the first positive
boot began. A host power interruption then rebooted msr1 during that positive.
The service did not complete naturally, so the entire r47 attempt is
invalidated and receives no milestone credit despite its completed negative.

After the reboot, an audit at `23:42Z` found no surviving benchmark or compute
lock, verified that the detached checkout remained exact `61af8602`, and
re-hashed all five r45 artifacts to the values above. Fresh replacement unit
`harmony-n6-arm-kvm-61af8602-r49.service` (invocation
`62a8a2c179be4b3a88a62a4476a60138`) began at
`23:42:47.450115121Z`. Its canonical one-PID waiter is compute-exclusive PID
`1317` on post-reboot lock identity `35:3`, followed by benchmark-shared PID
`1319` on `35:4`; payload PID `1320` began only after both locks. The cgroup is
`/consonance.slice/harmony-n6-arm-kvm-61af8602-r49.service`, declared and
effective CPUs and every observed `Cpus_allowed_list` are exactly `2-5`, and
Cargo parallelism is four. It completed naturally and successfully at
`23:57:04.162135410Z`, consuming 14m13.848s CPU with a 467.3 MiB peak, and
released both locks. Its two traps-off logs differ, as required, at SHA-256
`b9fb93a90098a117b7e3521189812452e1a2f69b28ceab16028dbf4fc1aea2ae`
and `0313f4e37a30ee4414f27121963e64b1b6d75a02636c5735bd5afc18df66e814`;
the negative verifier emitted `N6_TRAPS_OFF_REJECTED` before either positive
received credit. The two traps-on logs are byte-identical at
`3cb25dd0f4ac005be883e74faf590437bdbc728c5e7aa4d10fdf1bc41ab39ea5`
and report 9/9 rows and 192 operations. Independently copied report files
re-hashed to those same values. The negative and positive verdict hashes are
`f4857a87772605d0cb31949ee8441d6a20790c87644df598555862e99ce9f5e7`
and `1a05e5f2ac6bbfcc122050442039ec5df6a2dd84efffdd075b3dd6155eca72cb`,
the same verdict bytes as the final HVF run.

Exact x86 CI run `33337143247` for parent implementation commit `5f25dd3e`
completed green: its 36m19s locked image job, ordered N6 live negative and two
positives, all check/X1/probe jobs, four X2 replicas, and eight X2 hunt jobs
passed. Exact successor run `33339380055` on `61af8602` also completed green:
its locked guest-image job, ordered N6 live negative and two positives, check,
both X1 jobs, all six probes, all four X2 replicas, and all eight hunt replicas
passed. Its N6 job `99337021431` reports all 9/9 x86 rows and 166 operations;
the uploaded report artifact digest is
`c052c220598ba0e8ceff39e1f19f48fe9151ea4a93e2c6cc3242c0282b445838`.
Exact-head secret-scan run `33339380052` also completed successfully.

On exact implementation tree `61af8602`, `cargo build
--all-features`, all-target/all-feature Clippy with warnings denied, formatting,
and cargo-deny advisories, bans, licenses, and sources passed. The permission-
correct nextest run passed all 1,176 tests (25 configured skips), including the
two planted complete-line runner tests added by `61af8602`. The first
sandboxed nextest attempt stopped after 405 passes when two telemetry listener
tests received `EPERM`; the rerun with local socket permission passed. The
first cargo-deny attempt could not lock the sandbox-read-only advisory database;
its permission-correct rerun passed. Neither environment-denied attempt is
counted as a repository gate. Pinned `nightly-2026-06-16` Miri with
`-Zmiri-permissive-provenance` passed the exact final tree for the
unsafe-bearing `vmm-backend` crate: 48 unit, three contract, two dynamic, two
exhaustive, 16 run-loop, and one vCPU-state test; the public-API regenerator is
the sole deliberate ignore. The final-tree N6 self-test, LL/SC boundary
control, tripwire audit, and byte-for-byte generated-listing comparison also
pass: both architecture sweeps exercise 9/9 rows (ARM 192 operations, x86 166),
the LL/SC positive converges while the accumulating negative diverges, all 15
retained tripwire mechanisms are present, and every planted negative is
rejected. Updating the table's verification metadata leaves the regenerated
362-line listing byte-identical.

**N6 overall: PASS.** Every retained verification item has a meaningful
positive and planted negative. The ARM traps-off checks failed before either
HVF or KVM positive was credited; each backend then exercised 9/9 rows and 192
operations twice. Exact-head x86 CI likewise rejected traps-off first and then
exercised 9/9 rows and 166 operations twice. The LL/SC accumulating negative
diverges, the retained three-patch/15-mechanism tripwire audit fails when an
exit ABI is removed, both frozen-table listings are complete, and the N0
*untested* markings in `docs/DETERMINISM.md` now reflect the tested evidence.

## Main reconciliation and Dissonance isolation (2026-08-31)

The completed milestone tree was reconciled with current `main` at
`d09d9d3878650cea7bf16edccdd3cb2a0ab8c445` by a normal merge. The complete
`dissonance/` subtree was resolved from that `main` commit, not from the older
virtual-time branch history. A path-limited tree comparison against `main`
is empty: the pull request therefore carries zero Dissonance file changes.
The shared `quality.yml` resolution retains both the current-main Dissonance
workspace gate and the Consonance `harmony-linux` relocation.

The reconciled tree passes `cargo build --all-features`, formatting, all-target
all-feature Clippy with warnings denied, cargo-deny, and all 1,176 nextest tests
(25 configured skips). The permission-denied sandbox attempts for the two
loopback listener tests and the advisory-database lock were rerun with the
required local access and passed. Pinned `nightly-2026-06-16` Miri with
`-Zmiri-permissive-provenance` passes `vmm-backend`, including 48 unit, three
contract, two dynamic, two exhaustive, 16 run-loop, and one vCPU-state test.
The N6 self-test, generated-listing byte comparison, LL/SC boundary control,
and three-patch/15-mechanism tripwire audit also pass unchanged after the
merge.

Two archived evidence launchers, `scripts/run-m6-concurrency.sh` and
`consonance/harmony-linux/scripts/virtual-time-m2-oracle.sh`, name the retired
current-main Dissonance binaries `m6-concurrency` and
`smb-vtime-continuation`. They preserve the commands used for the recorded M2
and M6 evidence, but are not final-tree repository gates. Reintroducing those
obsolete Dissonance implementations would violate the isolation decision;
future live reruns must use a separately versioned Dissonance checkout or a
new Consonance-owned oracle rather than silently restoring stale search code.

The first post-reconciliation public-API CI job exposed that the committed
`vmm-backend` snapshot had been generated on aarch64 Linux even though the job
and its documentation define the frozen surface as x86-64 Linux. That snapshot
contained `LiveKvm` and omitted the x86-only `KvmBackend` and
`PatchedKvmBackend`. The snapshot was regenerated with cargo-public-api 0.52.0,
pinned nightly `2026-06-16`, and explicit target
`x86_64-unknown-linux-gnu`; a second independent generation compares
byte-for-byte. The guard now skips non-x86-64 targets explicitly instead of
mis-comparing architecture-specific concrete backends.
