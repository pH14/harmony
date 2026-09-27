# Troubleshoot a run

Start with `harmony preflight --json`, keep the full error message, and use a fresh output directory for your next attempt.

## Installation and host errors

| Symptom | Action |
| --- | --- |
| `harmony: command not found` | Add the installation's `bin` directory to `PATH`; try the executable's absolute path |
| `/dev/kvm` does not exist | Enable hardware virtualization; in a VM, expose nested virtualization from the outer host |
| `/dev/kvm` is not writable | Grant your user membership in the host's KVM-access group, then log out and back in |
| macOS hypervisor creation fails | Check that the host is Apple silicon, `kern.hv_support` is 1, and the CLI has the hypervisor entitlement; re-sign after replacing it |
| Host is an untested cell | Review [the matrix](../reference/compatibility.md); use `--allow-untested` for OCI only if you intend to opt in |
| `no run loop` or unsupported host | Use a host supported by that command; an opt-in flag cannot add a missing backend |
| No guest kernel | Point `HARMONY_GUEST_DIR` directly at the architecture directory containing `Image` or `bzImage` |
| No container-capable initramfs | Install `initramfs-oci.cpio.gz` beside the selected kernel; a plain `initramfs.cpio.gz` is insufficient for OCI |
| Kernel instruction check fails while building | Confirm native Ubuntu 24.04, matching compiler baselines, and the exact source revision; retain the diagnostic instead of bypassing the check |

A partially populated `HARMONY_GUEST_DIR` containing a kernel can shadow a complete installation elsewhere. Check the actual selected paths in preflight.

## Image and startup errors

If image acquisition fails, test your container engine directly. Harmony tries Docker before Podman; a broken Docker installation can prevent a later Podman attempt. Export with the working engine and pass a local archive if necessary.

For `unrecognized image input`, use an OCI layout or a `docker image save` archive, not `docker export`. Confirm that a relative image path exists in your current directory.

For an architecture error or executable format error, rebuild or export for the host CPU. Harmony cannot run an amd64 guest on an Arm64 host.

If `container_rc` is null, inspect `supervisor_failure_rc`, `runtime_rc`, `startup_rc`, and `serial.log`. Verify that the command exists inside the image, its interpreter and libraries are present, its working directory exists, and its `USER` can access the files. An observed application exit of 127 is different from a process that never started.

## Timeouts and incomplete evidence

A host timeout is a resource limit, not an assertion violation. OCI timeout output includes a partial `serial.log` without a completed run record. Increase `--timeout` only after checking whether the application is meant to exit and whether the guest is making progress.

For fault search, inspect both `execution_failures` and `watchdog_cutoffs`. A completed CLI command can still contain cut-off executions. A checker that never completes or never emits a supported success point cannot establish recovery. Confirm your `ready`, `workload`, and `check` commands work on a small budget before increasing campaign size.

If a search finishes without finding a bug, inspect whether the checker ran and the workload made progress. A finite search with no observed violation is not proof of correctness.

## Repeatability mismatches

Compare the exact image, kernel, base runtime, command, seed, RAM, knobs, executable version, and host architecture before changing budgets. Do not compare a replay against a rebuilt application as if it were the same execution.

## Report a problem

[Open a GitHub issue](https://github.com/pH14/harmony/issues/new) with:

- Your source commit or release version, host OS/CPU, and `preflight --json` output.
- The command and full error, with sensitive values removed.
- The relevant report and serial log, plus whether the problem repeats with identical inputs.
- A minimal image recipe and workload bundle when you can share them.

Logs, images, command arguments, and reports can contain application data or credentials. Review them before posting publicly. You do not need to include private datasets or proprietary binaries to describe the failure.
