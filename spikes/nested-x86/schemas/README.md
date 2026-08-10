# nested-x86 canonical evidence formats

Every run-set directory (`results/<stage>/<run-set>/`) contains:

- `env.json` — L0 environment at launch: sha256 of the QEMU binary, L1 kernel,
  and appliance/probe initramfs; `kvm_intel.nested` + `kvm.enable_pmu` as read
  from sysfs at launch; the pinned cpuset; the QEMU machine line; extra kernel
  cmdline (`harmony.gates=` / `harmony.env=`); start timestamp (UTC).
- `env.json.rc` — the QEMU exit code (`qemu_rc=N`).
- `console.log` — the L1 serial console, verbatim. All in-guest evidence rides
  here between sentinels:
  - `NESTED_X86_L1_BOOT_OK` / `NESTED_X86_L1_DONE` — guest lifecycle;
  - `NESTED_X86_L2_PIN_CHECK_BEGIN/END` — in-L1 sha256 of the L2 images;
  - `NESTED_X86_PROBE_BEGIN/END` — N-0 probe JSON (strip kernel `[...]` printk
    lines before parsing);
  - `NESTED_X86_GATE_BEGIN/RC/END <name>` — one gate binary's full output and
    its exit code;
  - `N2JSON {...}` — hammer events (round-2 instruments, PR #98): `start`
    (records `backend` — must be `PatchedKvmBackend`), `progress`, `summary`
    with `deadlines` (total driven), `armed_pmi` (`d > SKID_MARGIN`, planner
    arms an overflow PMI), `mtf_only` (`d ≤ SKID_MARGIN`, no PMI armed),
    `exact`, `oracle_ok` (guest-memory oracle agreements), `mismatches`,
    `record_violations`, `final_work`, and `records{samples,lost,throttle,
    other}` counted from the perf ring. Acceptance per run:
    `exact == oracle_ok == deadlines`, zero `mismatch`/`record_violation`
    events, `lost == throttle == 0`. **The armed-PMI floor axis is
    `records.samples` — the perf-record ground truth — never a summary field
    the harness asserted about itself** (`check-recert-floors.sh` recomputes
    it per runset). *Legacy note:* pre-round-2 runsets (`*-recert-001` and
    older) emitted a single `armed` field that conflated the two deadline
    classes — the very defect the round-2 review found; their floor
    contribution is likewise taken from `records.samples` only.
  - `N3JSON {...}` — repeat-gate events: `start`, `progress`, `summary`
    (`attempted`/`identical`/`mismatches` + the reference `state_hash` and
    `observable_digest` — the cross-substrate comparison artifact), `mismatch`.
    A rep counts identical only on a CLEAN run (halted, `run_error()` empty,
    clean debug-exit-0 terminal — round-3 #2).
- `qemu-stdout.log` — QEMU's own stdout/stderr (empty on a clean run).
- `probe.json` (N-0 only) — the probe output **sentinel-wrapped as captured**
  (`NESTED_X86_PROBE_BEGIN` … `END`, possibly with interleaved kernel printk
  lines): NOT itself valid JSON (round-5 P2 — the earlier claim here was
  wrong; the retained files are immutable and stay as captured). The consumer
  seam is `../harness/extract-probe-json.sh`, which strips the sentinels and
  printk lines and validates the remainder (`python3 -m json.tool`); it
  validates all retained N-0 artifacts (runsets 002–004 `probe.json`, and the
  probe block of any console.log).
- `condition.json` / `condition-end.json` (N-2/N-3 stress run-sets) — the L0
  condition: name, stress-ng pid, cpuset, deadline count, seed, start/finish
  timestamps, and the harness rc. Round-2+ harnesses also record
  `stressor_alive_at_end` (`yes`/`no`/`n/a`), `migrations` (successful
  affinity changes only — round-4), and `migrations_failed`; a migrate
  condition additionally requires `migrations.count > 0` (the dose must have
  happened). The floor checker enforces these where present; pre-round-2
  runsets are annotated legacy, with the N-3 stress/migration dose proven
  from recorded artifacts in `../results/AUDIT-2026-07-12.md` §"N-3 dose
  audit (round-4)".
- `build-manifest.json` (round-1+ run-sets) — the appliance build manifest
  copied into the runset at launch, after pre-boot sha256 pin verification
  (`PIN_VERIFIED` in the harness log).
- `qemu.pid` — QEMU pidfile (written by QEMU; used by the migration condition).

Golden evidence is immutable; reruns create a new run-set. Raw volumes too
large for git would be content-addressed with a checked-in manifest — as of N-2
everything fits in-tree.

Build provenance: `results/n1/build-manifest.json` pins the appliance content
(source commit, gate binary hashes, patched module hashes, L2 image pins, L1
kernel hash, appliance cpio hash). The box-side source tree is rsync'd from the
spike worktree; `/root/harmony-nested/.spike-source-commit` records the commit.
