# Harmony Linux guest implementation

The guest image is part of consonance's determinism boundary. Build scripts pin
the kernel source, patch series, userland inputs, and artifact hashes.

The kernel patches confine userspace to the deterministic instruction subset,
provide the doorbell and pvclock interfaces, and keep host-driven time and
entropy out of guest-visible state. x86 and arm64 configs disable unsupported
devices and asynchronous facilities. The tiny init launches the selected
workload and reports through the frozen channels.

The VMM advances virtual time from normalized VM exits. A halted guest wakes at
its modeled timer deadline; a runnable guest advances only when an exit is
serviced. Guest time reads use the pvclock or trapped architecture clock and
never a host clock.

Build and manifest checks reject an unpinned input or mismatched artifact.
Boot, database, container, SDK, and NES scenarios are live determinism oracles;
their normalized logs and state hashes are compared across same-seed runs and,
where recorded, across hosts.
