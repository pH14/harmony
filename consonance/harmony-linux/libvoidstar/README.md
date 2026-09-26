<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Harmony `libvoidstar.so`

`libvoidstar.so` is the clean-room compatibility library for the public SDK
ABI used by guest workloads. It sends SDK JSON to `/dev/harmony`, obtains
seeded entropy through the driver's fixed transaction, and exposes the legacy
coverage and sanitizer callback symbols expected by instrumented programs.

Device exchanges are serialized per process. A thread that calls
`harmony_coverage_configure` gets an explicit identity and a callback counter,
and asks the scheduler for its next threshold each time the counter reaches
the current one. A thread that never configures makes no coverage exchange,
so an instrumented program pays no device round trip per edge. Device errors fail closed:
an event is dropped and entropy returns zero rather than using host randomness.

Coverage callbacks invoke the optional weak `harmony_instrumentation_event`
hook when a workload links a compatible instrumentation runtime. The library
does not define a fault protocol or make fault decisions; workloads compose
the runtime they need with this ABI shim. Calls remain harmless when no runtime
provides the hook.

Build and test it with:

```sh
make -C consonance/harmony-linux/libvoidstar check
```

Linux images install the result at `/usr/lib/libvoidstar.so` and use the fixed
device path `/dev/harmony`.
