# Fault SDK adapter

`fault-sdk` is the optional guest-side compatibility layer for the fault
workload. `hypercall-proto` and `harmony-sdk` deliberately expose only the
opaque SDK opcode-3 request and the generic event/assertion surface. Import
`FaultClientExt` or `FaultSdkExt` when a fault package needs the buggify or
network-flow request codecs.

Both codecs use SDK opcode 3. Buggify uses namespace 7, a `u32` point payload,
and the point as request id. Network flow uses namespace 4, an 18-byte
`src:u32, dst:u32, conn:u64, event:u16` payload, and the connection as request
id. Responses remain package-owned opaque bytes; the buggify adapter accepts
exactly one data byte and the network adapter requires a non-empty data answer.
