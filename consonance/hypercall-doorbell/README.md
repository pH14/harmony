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

The crate depends only on `hypercall-proto`, builds without `std`, and is
validated by protocol loopback, hostile-response, boundary, and Miri tests.
