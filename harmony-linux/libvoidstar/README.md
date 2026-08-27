# Harmony `libvoidstar.so`

This clean-room compatibility library implements the public ABI used by Antithesis SDKs.
It sends SDK JSON to `/dev/harmony`, obtains deterministic entropy with the driver's
one-byte write/eight-byte read transaction, and turns instrumented basic-block
callbacks into thresholded SDK yields. Each logical thread has an explicit stable id,
a per-thread counter, and a threshold prescribed by the host's preceding response;
crossing it performs command 1's 17-byte request/12-byte response transaction. The
response supplies both the next threshold and an index in the declared runnable set.

Build and test it with `make -C harmony-linux/libvoidstar check`. Linux images install
the resulting library as `/usr/lib/libvoidstar.so`; the device path is fixed at the ABI
path `/dev/harmony` (the R-L3 fixed-transport ruling — it is not configurable).
