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
test also runs under Miri. Live checks require nested VMX or SVM and fail when
the required artifacts or hardware are absent.

The ignored `tests/live.rs::inner_consonance_runs_inner_guest` proof uses
`NESTED_HOST_KERNEL`, `NESTED_OCI_INITRAMFS`, and `NESTED_DRIVER_IMAGE`.

```sh
NESTED_HOST_KERNEL=... NESTED_OCI_INITRAMFS=... NESTED_DRIVER_IMAGE=... \
  taskset -c CPU cargo test --release -p nested-driver --test live \
  inner_consonance_runs_inner_guest -- --ignored --exact --nocapture
```

`tests/nested_restore.rs::outer_nested_state_snapshot_matrix` captures after
step three and compares steps four through twelve with an uninterrupted
reference. It also assembles the detour's sparse capture and compares every RAM
byte with the live guest. It covers capture without restore, eight restores after an
overwriting detour, and portable export followed by killing the capture process
and importing into a new process. Its ignored `cold_snapshot_child` helper is
started by the matrix, with separate capture and import modes; it is not a
standalone qualification. Each output must agree with the independent oracle,
with one inner creation and zero imports at every boundary.
Every restored RAM page must match the cut before continuing L2, with no
in-place restore fallback.
On an oracle failure, dedicated SDK registers publish the actual bytes, the
guest's expected bytes and the step before stopping at the assertion. The
host prints those values alongside its independent expected bytes. The matrix
also reports RAM pages that differ immediately after an outer restore. Hosted
qualification retains the exact kernel, initramfs and OCI inputs with its logs
so a failed continuation can be reproduced.

The non-default `harmony_omit_nested_state` compiler configuration discards captured VMX state only
when publishing the outer snapshot. Uninterrupted and capture-only execution
must still pass; its first restore must fail. VMX loses its live VMXON/VMCS
state. Enabled SVM loses GIF; the matrix compares that control state immediately
after restore, before a later VM entry can change it. Positive SVM captures also
require EFER.SVME, a live host-save MSR, and exact GIF preservation through every
restore and cold import. The test bounds
each lifecycle wait with the production client watchdog and releases the VM
when a continuation stalls.

```sh
NESTED_HOST_KERNEL=... NESTED_OCI_INITRAMFS=... NESTED_DRIVER_IMAGE=... \
  taskset -c CPU cargo test --release -p nested-driver --test nested_restore \
  outer_nested_state_snapshot_matrix -- --ignored --exact --nocapture
```

Prefix that command with `RUSTFLAGS="--cfg harmony_omit_nested_state"` for the
expected-failure control. Portable tests bind the complete nested payload into
the codec and identities, reject mismatched contracts before mutation, and
reject publication while L2 is active. Header-buffer bounds and the mock
snapshot path also run under Miri. The live ioctls are checked on KVM.

`--sdk --search` selects the operation workload. Twenty-eight SDK entropy bytes
choose Run, Snapshot, Restore, Fork, Drop, or ExportImport, a held snapshot, one
to eight input words, and a branch seed drawn from bytes the input words never
use. A bank of one to eight snapshots keeps
every choice valid. The longer real-mode program reads its previous registers,
four memory words, a round counter and its input before producing each output.
A separate Rust oracle checks every step. Restore reads the saved registers
and memory, runs L2 without writing new inputs, checks that readback, then
restores the cut again. Run checks its first execution against the oracle,
restores the start, and requires the second execution to produce identical
outputs. The second restore is read back immediately too, so retained
RAM from the readback step fails at the restore boundary. The SDK smoke imports
and restores the cold outer root twice, then starts with the Run seed from the
hosted Intel failure before warming the inner VM. It uses the campaign's
512 MiB outer RAM configuration and follows the five-operation Intel
ExportImport failure prefix. The operation workload enables KVM's invalid
VMCS dump in the L1 kernel command line so a rejected VM entry retains the
guest, host, and control fields in the console evidence. Fork and portable import use production
control operations.
Any in-place fallback fails instead of recreating the inner VM.

Four always assertions cover the L2 oracle, restore readback, operation errors
and identical replay outputs. Thirty-six sometimes assertions cover ordered
operation pairs. State registers publish creations, imports, steps, all output
bytes, live snapshots, fork depth, operation count/type and the pair mask.
Inputs use the tracked memory-effect API so restore can use dirty-page
tracking and keep the existing inner VM in place.

The optional `host-search` feature supplies the `harmony search --package nested`
adapter. Actions select SDK entropy seeds. Outer SDK boundaries become sparse
snapshots containing live inner KVM state; restoring a campaign parent imports
that outer state and verifies its published creation/import counts before
continuing. It also checks the outer control server's fallback counter after
every branch and replay; restoring guest counters cannot hide an outer VMM
recreation. A fallback becomes preserved outer-VMM failure evidence. The cold
import and SDK smoke require zero fallbacks too. The adapter uses the standard
campaign, archive, suffix draw and
parent selector. Current snapshot count, fork depth and operation type form
state cells; coverage is reported separately. The CLI writes the campaign
stream, report and each failure with its level and input seeds. Replaying the
stream reproduces the complete outer restore sequence. The workload identity
includes the host's nested-host contract hash, so a stream recorded on a host
with different VMX or SVM capabilities is rejected before replay starts. A run
that makes no virtual-time progress within the session's 20-second host bound
stops the campaign with an error and records no failure, because the state at
that point depends on host timing.

Campaign format v2 hashes the complete job result after applying the VMM's
existing logical XSAVE identity projection to copied sidecars. Init-valued
x87/SSE presence bits can change during nested KVM execution; their raw values
remain in the original snapshots used for restore. RAM, active CPU state,
nested virtualization state, SDK events, policy, control state, observations, outcomes and
operation inputs remain checked by replay.

A failure during campaign preparation uses the searcher's failed disposition.
It retains its observation, layer and input seeds when no suffix action runs.
A failed root reset also survives the following parent restore; the next job
reset starts a new attempt. Portable adapter tests exercise preparation-failure
evidence through the generic rollout, report serialization and checkpoint
roundtrip without requiring KVM.

The hosted qualification runs a bounded search and replays its complete stream,
checking execution counts, work, coverage, stream digest and failure evidence.
It attempts replay even when the search reports a failing assertion, preserving
both exit statuses and logs while retaining the failed qualification result.
The runner must expose KVM-supported VMX or SVM with NPT and
`KVM_CAP_NESTED_STATE`. Hosted x86 labels can allocate either vendor, so the job
checks capabilities before building artifacts. Both vendors use the same inner
driver and six-operation workload; vendor-specific capability identity prevents
cross-vendor outer snapshot import.
