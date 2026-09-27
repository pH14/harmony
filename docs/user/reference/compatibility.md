# System compatibility

Harmony runs a Linux guest on the same CPU architecture as the host. It does not emulate another architecture. A working hypervisor, a compatible runtime, and an admitted workload are separate requirements.

## Host capabilities

| Host | Hypervisor | OCI runs | Fault search | NES in a VM |
| --- | --- | --- | --- | --- |
| Linux x86-64 (Intel or AMD) | KVM | Available | Available | Available |
| Linux Arm64 | KVM | Available | Available | Available |
| macOS on Apple silicon | Hypervisor.framework | Available; sign the CLI | Available; use one worker | Not wired into this CLI |
| Intel macOS | — | Unsupported | Unsupported | Unsupported |
| Windows | — | Unsupported | Unsupported | Unsupported |

Native NES search uses a matching host QuickNES library and does not require a guest runtime. See [NES usage](../how-to/nes.md).

Linux needs read/write access to `/dev/kvm`. Inside a VM, the outer hypervisor must expose nested virtualization. Inside a container, access to the underlying hypervisor must still be provided; installing Docker by itself does not provide KVM.

On macOS, Hypervisor.framework permits one VM per process. Use `--workers 1` for a fault campaign.

## What preflight classifies

The CLI currently labels these host situations `proven` when it can also establish that it is **not in a container**:

- Linux x86-64 detected inside a VM.
- Linux Arm64 detected on bare metal.
- macOS Arm64 detected on bare metal.

Other situations within the supported OS/architecture combinations are `expected`, including Linux x86-64 bare metal, Arm64 nested guests, unknown nesting, and containerized hosts. This conservative classification is about recorded host evidence, not a ranking of performance or a universal proof of application behavior.

`harmony preflight` exits unsuccessfully for `expected` hosts. `harmony oci run --allow-untested` lets you explicitly try one. It cannot enable a missing hypervisor or an unsupported architecture. Fault search has no equivalent flag: it checks backend availability but does not apply the OCI support-matrix check. Validate repeatability for your specific host and workload before relying on its results.

## Guest runtime

Use the kernel and OCI initramfs from the same Harmony source version as your CLI:

| Architecture | Kernel | Container runtime image |
| --- | --- | --- |
| x86-64 | `bzImage` | `initramfs-oci.cpio.gz` |
| Arm64 | `Image` | `initramfs-oci.cpio.gz` |

Guest execution uses **one virtual CPU**. Multiple fault nodes are processes within that same VM. Increasing `--workers` runs more campaign evaluators; it does not turn a guest into a multiprocessor system.

## Workload restrictions

Use trusted, fixed Linux workloads whose dependencies and inputs you control. Harmony is experimental; successfully booting an arbitrary image does not establish that its execution is deterministic.

- Match the image architecture to the host. No cross-architecture execution is provided.
- Package files and dependencies before the run. The CLI offers no external networking, host mounts, or port publishing.
- Keep all nodes of a fault workload within the image. There is no multi-host cluster orchestration interface.
- On x86-64, generated code/JITs, self-modifying code, writable executable memory, and userspace XSAVE-family saves are outside the controlled-workload policy. XGETBV is allowed only with a proven zero selector. A language runtime that generates machine code is not a supported workload merely because it fits in an OCI image.
- On x86-64, supported dynamic loading uses `/lib64/ld-linux-x86-64.so.2` and the default library directories. RPATH/RUNPATH, loader caches, glibc-hwcaps, TLSdesc relocations, and other dynamic interpreters are outside the admission contract. A dynamically linked musl application is not equivalent to the static BusyBox `/bin/echo` example.
- Arm64 workloads also need to satisfy the controlled instruction/runtime assumptions. Ordinary registry images are not automatically qualified by host support; avoid treating a successful smoke run as certification of an arbitrary language runtime.

The runtime forces `LD_BIND_NOW=1`. Do not remove that setting or replace the patched kernel while expecting the same execution contract. For a repeatability mismatch, retain the exact inputs and [report the result](../how-to/troubleshooting.md#report-a-problem).
