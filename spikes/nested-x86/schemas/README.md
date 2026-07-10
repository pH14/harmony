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
  - `N2JSON {...}` — hammer events: `start`, `progress`, `summary`
    (`armed`/`exact`/`mismatches`/`final_work`), `mismatch` (detail string);
    acceptance requires `exact == armed` and zero `mismatch` events;
  - `N3JSON {...}` — repeat-gate events: `start`, `progress`, `summary`
    (`attempted`/`identical`/`mismatches` + the reference `state_hash` and
    `observable_digest` — the cross-substrate comparison artifact), `mismatch`.
- `qemu-stdout.log` — QEMU's own stdout/stderr (empty on a clean run).
- `probe.json` (N-0 only) — the probe JSON extracted from the console, printk
  lines stripped, revalidated as JSON.
- `condition.json` / `condition-end.json` (N-2/N-3 stress run-sets) — the L0
  condition: name, stress-ng pid, cpuset, deadline count, seed, start/finish
  timestamps, and the harness rc.
- `qemu.pid` — QEMU pidfile (written by QEMU; used by the migration condition).

Golden evidence is immutable; reruns create a new run-set. Raw volumes too
large for git would be content-addressed with a checked-in manifest — as of N-2
everything fits in-tree.

Build provenance: `results/n1/build-manifest.json` pins the appliance content
(source commit, gate binary hashes, patched module hashes, L2 image pins, L1
kernel hash, appliance cpio hash). The box-side source tree is rsync'd from the
spike worktree; `/root/harmony-nested/.spike-source-commit` records the commit.
