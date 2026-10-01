# Inner Consonance driver

`nested-driver` is a composition workload linking the production `vmm-core`
and `KvmBackend`. It creates one VM with 64 KiB RAM and one vCPU. Its handwritten
16-bit L2 loop reads four prior page values and three registers before updating
them, then writes byte `0x5a` to COM1. The independently specified wrapping
arithmetic oracle checks every completed port boundary. A changed prior word
changes the next result; a fresh write cannot hide a broken restore.

`nested-driver` runs twelve steps directly on Linux KVM. `--sdk` publishes
creation/import counts and step progress through Harmony's SDK and emits a
lifecycle point after every completed inner run. Those points leave L1 outside
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

The ignored `tests/live.rs::inner_consonance_runs_l2` proof uses
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
  inner_consonance_runs_l2 -- --ignored --exact --nocapture
```
