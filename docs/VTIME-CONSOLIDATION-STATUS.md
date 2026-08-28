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

### Portable verification completed on the N2 tree

```text
cargo build --workspace --all-features
PASS

cargo nextest run --workspace --all-features --no-fail-fast
1159 passed, 25 skipped

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
complete command rerun with localhost-socket permission passed all 1,159
tests, matching N0's already-recorded execution-environment distinction.

The removed public items (`Backend::run_until`, deadline exit/counters,
branch-ratio state, the exact-stop control error, and ARM's raw branch event)
and the renamed pvclock flag are reflected in the regenerated public-API
snapshots. The control-proto and vtime regenerators pass locally; the Linux
surface snapshots are re-run on msr1 before this milestone can close.

Searches over the non-historical tree find no retired modules, symbols,
feature flags, patch names, or file names. A case-insensitive filename search
for the retired qualifier returns only `docs/PRESCRIPTIVE-VTIME-STATUS.md`.
Text matches outside the plan/status ledgers are links to that historical
record or the six occurrences of the frozen v1 log token in the encoder,
dumpers, and byte-comparison oracle described above. The workflow display
name and concurrency group are `x86-virtual-time`.

### Verification still required before N2 can be marked PASS

- pinned Miri matrix on this exact tree;
- Linux public-API regeneration/check;
- N1 boot and NES references on HVF and msr1 KVM, including direct byte
  comparison; and
- the X-series stock-KVM reference on both GitHub Actions vendor pools.

**N2 overall: IN PROGRESS.** The implementation and portable gates are green;
the milestone remains open until all exact-tree machine evidence above is
recorded.

## N3 — fast

Not started.

## N4 — the guest is part of Consonance

Not started.

## N5 — reproducible guest builds

Not started.

## N6 — defenses tested by attacking them

Not started.
