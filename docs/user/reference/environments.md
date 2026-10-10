# Supported environments

The host requirements depend on what you’re testing and which runner you use:

| Workload and runner | What you’ll need |
| --- | --- |
| Application / Consonance KVM | Linux with access to KVM and matching guest artifacts. |
| Application / Consonance HVF | Apple silicon macOS, an entitled executable, and matching Arm64 guest artifacts. |
| Application / Consonance UML | Linux and a matching UML profile. Hardware virtualization isn’t required. |
| NES / QuickNES | A supported native host and the pinned QuickNES core. |
| NES / Consonance | Linux KVM with the supported guest image and runtime. |

Consonance can select KVM, HVF, or UML automatically based on the host. Individual
workloads may impose further restrictions, and application images still need
to pass preparation and admission. Run `harmony check` with your actual recipe
to check its requirements.

## Resources and saved executions

Application services share a guest with one virtual CPU. A search can use
multiple worker guests within the host’s CPU and memory limits. Harmony also
limits workers on macOS because running concurrent HVF VMs has caused host
instability.

Keep the executable and runtime artifacts that produced a finding. UML recordings
also pin the host identity, so copying the input to another host does not
establish that it can reproduce the execution there.

For detailed platform qualification, see the
[runtime README](https://github.com/pH14/harmony/blob/main/consonance/harmony-linux/README.md)
and [CLI README](https://github.com/pH14/harmony/blob/main/cli/README.md).
If a requested combination is unsupported, Harmony reports an error rather
than silently switching backends.
