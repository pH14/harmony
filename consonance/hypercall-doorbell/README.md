# hypercall-doorbell

`hypercall-doorbell` is the guest-side `no_std` transport for the
`hypercall-proto` channel. It stages one request in a fixed guest-physical page,
rings one architecture-specific doorbell, and copies the host's response from a
second fixed page. The host-side exit handler and dispatcher live elsewhere.

## ABI

The request and response pages are each 4 KiB and are addressed by `REQ_GPA`
(`0xE000`) and `RESP_GPA` (`0xF000`). `DOORBELL_PORT` (`0x0CA1`) is the frozen
x86 port identity. One exchange is one `OUT`: the request length is carried in
`EAX`, the host services the request and writes a complete response frame, and
the guest derives the response length from that frame's header.

`RealIoDoorbell` emits the x86 port-I/O instruction. `MmioDoorbell` provides the
corresponding volatile 32-bit register store for an MMIO doorbell (used by
arm64). `IoDoorbell` is the seam used by loopback tests and alternate VMM
integrations. The public `VmcallTransport` name is retained for API stability;
the production mechanism is the port/MMIO doorbell described above.

## Exchange safety

`VmcallTransport::exchange` rejects requests larger than one page, clears both
shared pages, copies the request, rings the doorbell, checks response magic, and
validates the host-declared total length before copying any response bytes. A
missing frame returns `HostRejected`; a length that exceeds the page or caller
buffer returns `BadResponseLength`. No malformed host length can cause a
partial copy or an out-of-bounds access.

Constructors taking GPAs are unsafe because each GPA must be a distinct,
page-aligned, initialized, read/write, identity-mapped page that remains valid
for the transport lifetime. The doorbell must service those same pages, and
caller buffers must not alias them. The `unsafe` pointer logic is isolated in
the transport and covered through the `IoDoorbell` loopback seam.

The crate depends only on `hypercall-proto` in its default build and remains
`no_std`. Linux guest supervisors that cannot safely map the fixed pages may
enable the `linux-device` feature. It adds
`hypercall_doorbell::linux::DeviceTransport::open()`, which opens the
kernel-owned `/dev/harmony` device and exchanges frames through its synchronous
ioctl UAPI. The feature is Linux-only and adds `std` plus `libc`; it does not
change the raw mapped-page transport or its privileged guest contract.

The Linux UAPI adapter validates both caller lengths before issuing ioctl and
validates the driver-reported response length afterward. Its pure validation
and UAPI framing helper accepts the ioctl as a closure, so malformed lengths,
driver errors, and boundary cases are covered without requiring a device.
The crate is validated by protocol loopback, malformed-response, boundary, and
Miri tests.

## Observation mappings

With `linux-device`, `observation::Observation::create` obtains a zero-initialized
kernel-owned mapping and an opaque handle from `/dev/harmony`. The object owns
both the mapping and its descriptor; `bytes` provides exclusive mutable access.
Applications do not reserve physical pages or discover physical addresses.
Mappings are limited to 2 MiB each and sixteen live mappings per guest.

The driver records registration and revocation through the observation event
contract in `hypercall-proto`. Reads occur only while the VM is stopped, after
the producer's SDK execution boundary. The allocation is ordinary guest RAM, so
memory snapshots and the recorded SDK event history restore its bytes and handle
together. No host allocation or independent observation replay log is introduced.
The kernel's file reference remains alive through every VMA. If revocation
cannot be delivered, the kernel retains that allocation against the live-region
limit until VM teardown rather than freeing memory still named by host evidence.

The mapped slice's bounds are exercised under Miri. Allocation, mapping lifetime,
and cross-process access require the platform's real Linux guest tests.
