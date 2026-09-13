<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Harmony `libvoidstar.so`

`libvoidstar.so` is the clean-room compatibility library for the public SDK
ABI used by guest workloads. It sends SDK JSON to `/dev/harmony`, obtains
seeded entropy through the driver's fixed transaction, and exposes the legacy
coverage and sanitizer callback symbols expected by instrumented programs.

Device exchanges are serialized per process. An instrumented program that does
not configure explicit scheduler identities yields through `/dev/harmony` every
64 coverage callbacks. A process-wide counter selects the next threshold, and
callbacks that reach it serialize through the device exchange. Contending Go
threads wait for that exchange to finish so a single-vCPU guest cannot starve
the thread responsible for creating the scheduling point. A transport failure
terminates the managed process group because continuing would silently remove
deterministic scheduling points. Other device errors fail closed: an event is
dropped and entropy returns zero rather than using host randomness.

Build and test it with:

```sh
make -C consonance/harmony-linux/libvoidstar check
```

Linux images install the result at `/usr/lib/libvoidstar.so` and use the fixed
device path `/dev/harmony`.
