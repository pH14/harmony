# Supported environments

Choose the workload and runner first, then check the requirements of that
combination. Hardware support alone does not establish that an application
image is admitted or correctly instrumented.

| Workload and runner | Environment |
| --- | --- |
| Application / Consonance KVM | Linux with access to KVM and matching guest artifacts. |
| Application / Consonance HVF | Apple silicon macOS, an entitled executable, and matching Arm64 guest artifacts. |
| Application / Consonance UML | Linux; no hardware virtualization required. A matching UML profile is required. |
| NES / QuickNES | Supported native host and the pinned QuickNES core. |
| NES / Consonance | Currently Linux KVM with the supported guest image and runtime. |

Automatic Consonance selection considers KVM, HVF, and UML according to host
availability. A workload can impose tighter restrictions than its runner.
Use `check` with the actual recipe to establish readiness.

## Resource and replay limits

Applications run inside a single-vCPU guest. Multiple services share that
execution environment. Search can use multiple worker guests within host CPU
and memory limits. macOS has additional worker limits because concurrent HVF
VMs have caused host instability.

UML saved execution also pins host identity. Do not infer cross-host replay
from a portable input file. Keep the original executable and artifacts for an
investigation.

The [runtime README](https://github.com/pH14/harmony/blob/main/consonance/harmony-linux/README.md)
and [CLI README](https://github.com/pH14/harmony/blob/main/cli/README.md)
record detailed platform qualification. Unsupported combinations must fail
explicitly rather than silently changing the execution backend.
