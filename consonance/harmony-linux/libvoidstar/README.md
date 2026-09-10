<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Harmony `libvoidstar.so`

`libvoidstar.so` is the clean-room compatibility library for the public SDK
ABI used by guest workloads. It sends SDK JSON to `/dev/harmony`, obtains
seeded entropy through the driver's fixed transaction, and exposes the legacy
coverage and sanitizer callback symbols expected by instrumented programs.

Device exchanges are serialized per process. Unconfigured instrumented
workloads use one process-wide callback counter and yield at the library's
fixed 64-event cadence. This bounds compute-only execution on every backend
without a machine counter or an operator setting. A workload that calls
`harmony_coverage_configure` replaces that fallback with its explicit logical
thread identities and runnable sets. The high bit of a thread identity is
reserved for the fallback's deterministic per-process streams. Device errors fail closed:
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
