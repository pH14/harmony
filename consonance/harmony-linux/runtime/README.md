<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Guest runtime

`init.sh` is the platform PID 1 implementation installed as
`/usr/lib/harmony/init`. The image's `/init` entrypoint delegates to it. It
mounts the required kernel filesystems, validates the host supplied OCI
control directory, and fails closed when a required facility is absent.

The launch contract has one invocation of the pinned `/usr/bin/runc`:
`run --no-pivot --bundle /harmony-oci harmony`. The OCI configuration starts
the platform supervisor as root; the supervisor reads the mounted execution
specification and applies application credentials and process settings. PID 1
forwards termination signals, preserves process output, and emits separate
startup and application exit markers before forcing the guest to reboot.

The artifact builder supplies BusyBox, `/usr/bin/runc`, the supervisor, and
the platform device nodes. Architecture specific console transport belongs in
the platform image. This script writes status to its inherited output. On x86
that is the kernel console. On ARM the fixed entrypoint routes output through
the platform MMIO console service and drains it before reboot; the OCI payload
does not receive the service's physical-memory device.

The ARM kernel enables `/dev/mem` solely for that outer console service, which
maps the modeled PL011 data register. It is absent from the OCI device mounts;
application SDK and observation access uses `/dev/harmony`.
