# Harmony `libvoidstar.so`

This clean-room compatibility library implements the public ABI used by Antithesis SDKs.
It sends SDK JSON to `/dev/harmony`, obtains deterministic entropy with the driver's
one-byte write/eight-byte read transaction, and deliberately leaves coverage callbacks
inert until Harmony defines a coverage service.

Build and test it with `make -C harmony-linux/libvoidstar check`. Linux images install
the resulting library as `/usr/lib/libvoidstar.so`; the device path is fixed at the ABI
path `/dev/harmony` (the R-L3 fixed-transport ruling — it is not configurable).
