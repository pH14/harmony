# arm64 vendor implementation

The arm64 vendor layer maps KVM and Hypervisor.framework exits into the shared
VMM contract. It models the board's GICv3, architectural timer, serial device,
doorbell, pvclock, and deterministic sysregister surface.

Each serviced exit advances the shared virtual clock by its frozen duration.
WFI with a pending modeled timer uses the deterministic idle jump. Linux KVM and
macOS HVF emit the same normalized event classes and snapshot record set.

The stock `Arm64KvmBackend` intentionally does not create an in-kernel VGIC or
inject interrupts; that limitation is the settled project design. The portable
mock and live HVF/KVM gates cover the supported composition.
