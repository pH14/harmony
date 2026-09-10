<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Harmony `libvoidstar.so`

`libvoidstar.so` is the clean-room compatibility library for the public SDK
ABI used by guest workloads. It sends SDK JSON to `/dev/harmony`, obtains
seeded entropy through the driver's fixed transaction, and exposes the legacy
coverage and sanitizer callback symbols expected by instrumented programs.

Device exchanges are serialized per process. The library keeps explicit thread
identities and counters for callback thresholding. Scheduler yields start only
after the workload calls `harmony_coverage_configure`; ordinary instrumented
callbacks remain local so instrumentation does not turn every basic block into
a device transaction. Device errors fail closed:
an event is dropped and entropy returns zero rather than using host randomness.
`init_coverage_module` follows the SDK ABI and assigns non-overlapping edge
ranges to modules injected by the Go instrumentor.

Instrumented workloads may also inherit `HARMONY_EVENT_KILL_FD`. The library
reads positive `u64` arm values from that socket and kills its own process group
after that many future instrumented callbacks. This is a synchronous
instrumented-event coordinate. Zero disarms the coordinate.
Gate harnesses may additionally pass `HARMONY_EVENT_REPORT_FD`; immediately
before an armed event kill, the bridge writes the selected ordinal and the
generated global edge id as two little-endian `u64` values. Production search
does not set this diagnostic descriptor.

Build and test it with:

```sh
make -C consonance/harmony-linux/libvoidstar check
```

Linux images install the result at `/usr/lib/libvoidstar.so` and use the fixed
device path `/dev/harmony`.
