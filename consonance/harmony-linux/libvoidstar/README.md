<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Harmony `libvoidstar.so`

`libvoidstar.so` is the clean-room compatibility library for the public SDK
ABI used by guest workloads. It sends SDK JSON to `/dev/harmony`, obtains
seeded entropy through the driver's fixed transaction, and exposes the legacy
coverage and sanitizer callback symbols expected by instrumented programs.

Device exchanges are serialized per process. Every instrumented thread counts
its coverage callbacks, keyed by its Linux thread ID, and exchanges with the VM
when the count reaches its threshold. The first threshold is one callback; the
VM supplies each later quantum. Each exchange is a VM exit, so virtual time
advances and the guest scheduler can preempt a thread that never makes a system
call. A callback made while its thread holds the device lock, for example from
a signal handler during an exchange or an entropy read, only counts; the next
callback at or past the threshold exchanges.
`harmony_coverage_configure` sets an explicit thread identity and runnable
width. A forked child resets its counter and identity.

`harmony_coverage_add(hits)` counts several callbacks at once and returns how
many more the thread may count before its next exchange. The Java runtime counts
back-edges in Java code and calls it once per batch, because a native call at
every back-edge would cost more than the loop body.

When `/dev/harmony` does not exist, the thread stops exchanging. Any other
transport error or an invalid threshold aborts the process, because continuing
would leave a loop that never exits to the VM.

The library matches the Antithesis libvoidstar ABI:

- `notify_coverage(size_t)` returns `bool` and always returns true, so the Go
  and Python SDKs keep calling on every visit.
- `notify_coverage_v2`, `coverage_lease_generation_addr` and
  `instrumentation_request_abi_version` implement the coverage lease ABI. Each
  lease grants no extra hits, so every visit calls into the library.
- `init_coverage_module` gives each module a disjoint edge range.
- Clang `trace-pc-guard` and GCC `trace-pc` callbacks pass the caller address,
  so the fault runtime can report module offsets.

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
