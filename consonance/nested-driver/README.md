# Inner Consonance driver

`nested-driver` is a composition workload linking the production `vmm-core`
and `KvmBackend`. It creates one VM with 64 KiB RAM and one vCPU. Its handwritten
16-bit L2 loop reads four prior page values and three registers before updating
them, then writes byte `0x5a` to COM1. The independently specified wrapping
arithmetic oracle checks every completed port boundary. A changed prior word
changes the next result; a fresh write cannot hide a broken restore.

`nested-driver` runs twelve steps directly on Linux KVM. `--sdk` publishes
creation/import counts and step progress through Harmony's SDK and emits a
lifecycle point after every completed inner run. Two additional state registers
carry the fourteen output bytes at that exact boundary, independently of
asynchronous console draining. Those points leave L1 outside
nested guest mode while its live inner VM remains allocated. It never recreates
the VM or imports an inner snapshot in this proof.

`build-image.sh OUTPUT` builds the static x86 musl driver and packages a minimal
OCI layout. The standard platform init and supervisor launch it through runc.
The host must prepare the image with `LaunchRequest::with_kvm()` and boot a
matching nested-host kernel and CPU contract. Ordinary guests do not expose KVM.

This crate belongs to dependency group `composition_apps_tests`: composing an
engine as a workload intentionally joins the engine, guest SDK, and OCI test
surfaces. No hypervisor bindings or hardware logic are duplicated here.

Run portable checks with `cargo test -p nested-driver`. The mapping composition
test also runs under Miri. Live checks require Intel nested VMX and fail when
the required artifacts or hardware are absent.

The ignored `tests/live.rs::inner_consonance_runs_inner_guest` proof uses
`NESTED_HOST_KERNEL`, `NESTED_OCI_INITRAMFS`, and `NESTED_DRIVER_IMAGE`.
On ms02 the matching guest kernel, rebuilt current OCI runtime, and static
musl driver passed all twelve L2 steps in 14.31 seconds. Direct and nested
execution both ended with bytes `401200c00a52cfeb562200c0b8d8`, one creation,
and zero imports. The static binary scan found no RDTSC/RDTSCP/RDRAND/RDSEED.
A stale cached runtime from another experiment initially requested a park device;
rebuilding the current runtime corrected that input mismatch. The musl build
also exposed the backend’s glibc-specific ioctl argument type; inferring the
libc request type fixes the static build without changing request encodings.

```sh
NESTED_HOST_KERNEL=... NESTED_OCI_INITRAMFS=... NESTED_DRIVER_IMAGE=... \
  taskset -c CPU cargo test --release -p nested-driver --test live \
  inner_consonance_runs_inner_guest -- --ignored --exact --nocapture
```

`tests/nested_restore.rs::outer_nested_state_snapshot_matrix` captures after
step three and compares steps four through twelve with an uninterrupted
reference. It covers capture without restore, eight restores after an
overwriting detour, and portable export followed by killing the capture process
and importing into a new process. Its ignored `cold_snapshot_child` helper is
started by the matrix, with separate capture and import modes; it is not a
standalone qualification. Each output must agree with the independent oracle,
with one inner creation and zero imports at every boundary.

The ms02 positive matrix passed with a 4224-byte nested VMX state, valid VMXON
and current VMCS addresses, and the same final bytes as the direct proof. The
non-default `omit-nested-state` build discards captured VMX state only when
publishing the outer snapshot. Uninterrupted and capture-only execution must
still pass; its first restored continuation must fail. On ms02 it caused L1's
`kvm_spurious_fault` kernel BUG after the restore, as expected from discarded
VMX state. The host-side test uses
the production client watchdog with a twenty-second bound at each lifecycle
point, so a stalled continuation yields diagnostics and releases the VM.

```sh
NESTED_HOST_KERNEL=... NESTED_OCI_INITRAMFS=... NESTED_DRIVER_IMAGE=... \
  taskset -c CPU cargo test --release -p nested-driver --test nested_restore \
  outer_nested_state_snapshot_matrix -- --ignored --exact --nocapture
```

Repeat that command with `--features omit-nested-state` before `--test` for the
expected-failure control. Portable tests bind the complete nested payload into
the codec and identities, reject mismatched contracts before mutation, and
reject publication while L2 is active. Header-buffer bounds and the mock
snapshot path also run under Miri. The live ioctls are checked on KVM.

`--sdk --search` selects the operation workload. Twenty SDK entropy bytes choose
Run, Snapshot, Restore, Fork, Drop, or ExportImport, a held snapshot, one to
eight input words, and a branch seed. A bank of one to eight snapshots keeps
every choice valid. The longer real-mode program reads its previous registers,
four memory words, a round counter and its input before producing each output.
A separate Rust oracle checks every step. Restore reads the saved registers
and memory, runs L2 without writing new inputs, checks that readback, then
restores the cut again. Run checks identical outputs from two executions of
the same inputs. Fork and portable import use production control operations.
Any in-place fallback fails instead of recreating the inner VM.

Four always assertions cover the L2 oracle, restore readback, operation errors
and identical replay outputs. Thirty-six sometimes assertions cover ordered
operation pairs. State registers publish creations, imports, steps, all output
bytes, live snapshots, fork depth, operation count/type and the pair mask.
Inputs use the tracked memory-effect API. The initial smoke used the bulk
page-write API, which marks the whole RAM image dirty and caused an in-place
restore fallback; switching APIs corrected that composition error.

The short ms02 direct smoke exercised all six operation types. The outer SDK
smoke also exercised all six types and matched the same two-operation
continuation after an overwriting detour and outer restore, with one creation
and no additional inner imports caused by the outer restore. Its run took
1.20 seconds with one CPU and 256 MiB of L1 RAM. The static musl workload passed
the hardware-counter and RNG instruction admission scan.

The optional `host-search` feature supplies the `harmony search --package nested`
adapter. Actions select SDK entropy seeds. Outer SDK boundaries become sparse
snapshots containing live inner KVM state; restoring a campaign parent imports
that outer state and verifies its published creation/import counts before
continuing. The adapter uses the standard campaign, archive, suffix draw and
parent selector. Current snapshot count, fork depth and operation type form
state cells; coverage is reported separately. The CLI writes the campaign
stream, report and each failure with its level and input seeds. Replaying the
stream reproduces the complete outer restore sequence.

The two-execution ms02 CLI smoke completed five SDK operations, covered four
of the 36 ordered pairs (`100002c00`), and found no failures. Replaying its
complete campaign reproduced that result. This is a short smoke; the required
100-execution, five-minute-bounded search runs in GitHub qualification.
An Ubuntu 22.04 capacity probe on Intel Xeon Platinum 8370C exposed VMX and an
8320-byte nested-state bound. The tested Ubuntu 24.04 runner exposed AMD SVM
and failed the Intel preflight. Qualification selects Ubuntu 22.04 and retains
the hardware check; it does not assume every allocation supports VMX.
