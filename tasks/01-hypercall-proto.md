# Task 01 — `consonance/hypercall-proto`: guest↔host hypercall wire protocol

Read `tasks/00-CONVENTIONS.md` first. Touch only `consonance/hypercall-proto/`.

## Environment

Runs on: macOS and Linux. Requires: Rust + the `x86_64-unknown-none` target (compile-only
check, `rustup target add`). Does not require: `/dev/kvm`, Intel CPU, QEMU, root.

## Context

The deterministic hypervisor has **no real device models**. All guest I/O (console output,
entropy, block reads, test events) flows through a single paravirtual channel: the guest
writes a request frame into a shared memory page, executes VMCALL (a VM exit), the host
processes the frame and writes a response frame, and the guest resumes. This crate is the
**protocol layer only**: frame encoding/decoding, the guest-side client, and the host-side
dispatcher. The actual transport (shared page + VMCALL on the guest, VM-exit handler on the
host) is built later by others against the traits you define.

Because every byte of a response is replayed verbatim on re-execution, encoding must be
**bit-deterministic**: same logical message ⇒ same bytes, always.

## Wire format (normative)

All integers little-endian. A frame = 24-byte header + payload, and a full frame must fit in
one 4096-byte page (so `payload_len ≤ 4072`).

| offset | size | field | meaning |
|---|---|---|---|
| 0 | 4 | `magic` | `0x3150_4348` (`"HCP1"` as bytes `48 43 50 31` on the wire, little-endian like every field) |
| 4 | 2 | `kind` | 1 = request, 2 = response |
| 6 | 2 | `service` | see below |
| 8 | 2 | `opcode` | per-service |
| 10 | 2 | `status` | 0 in requests; response status code |
| 12 | 4 | `seq` | echoed from request into its response |
| 16 | 4 | `payload_len` | bytes of payload following the header |
| 20 | 4 | `reserved` | must be 0; receivers reject nonzero |

Status codes: `0 Ok`, `1 BadRequest`, `2 UnknownService`, `3 UnknownOpcode`, `4 OutOfRange`,
`5 Internal`. Services and opcodes:

- **Console = 1**: op `1 Write` — req payload: raw bytes; resp payload: empty.
- **Entropy = 2**: op `1 Fill` — req payload: `u32` byte count `n` (1..=4072); resp payload:
  exactly `n` bytes supplied by the host's seeded PRNG (PRNG itself is NOT in this crate; the
  host service trait implementor provides bytes).
- **Block = 3** (512-byte sectors, read-only medium):
  op `1 Capacity` — req empty; resp `u64` sector count.
  op `2 Read` — req `u64 lba` + `u32 sector_count` (1..=7, so data fits one frame); resp:
  raw sector data. Reads beyond capacity ⇒ status `OutOfRange`, empty payload.
- **Event = 4**: op `1 Emit` — req payload: `u32 event_id` + raw bytes (test/coverage events
  reported by the guest workload); resp empty. Hosts may ignore unknown ids but must ack `Ok`.

## Public API

Crate is `#![no_std]` at the core. Cargo features: `host` (implies `std`), `guest` (pure
no_std, no alloc). Default features: `["host"]`.

```rust
// ---- core (always available, no_std, alloc-free) ----
#[repr(u16)] pub enum ServiceId { Console = 1, Entropy = 2, Block = 3, Event = 4 }
#[repr(u16)] pub enum Status { Ok = 0, BadRequest = 1, UnknownService = 2,
                               UnknownOpcode = 3, OutOfRange = 4, Internal = 5 }

pub struct FrameHeader { /* fields per wire format */ }
pub const MAX_FRAME: usize = 4096;
pub const MAX_PAYLOAD: usize = 4072;

/// Encode a request into `buf` (caller-provided, ≥ 24 + payload.len()).
/// Returns total frame length. Errors if payload too large.
pub fn encode_request(service: ServiceId, opcode: u16, seq: u32,
                      payload: &[u8], buf: &mut [u8]) -> Result<usize, ProtoError>;
pub fn encode_response(service: ServiceId, opcode: u16, seq: u32, status: Status,
                       payload: &[u8], buf: &mut [u8]) -> Result<usize, ProtoError>;

/// Zero-copy decode: validates header, returns header + payload slice borrowed from `buf`.
/// MUST never panic, whatever the input bytes.
pub fn decode(buf: &[u8]) -> Result<(FrameHeader, &[u8]), ProtoError>;

pub enum ProtoError { /* thiserror in std builds; manual Display in no_std */ }

// ---- guest feature ----
/// Implemented later by the VMCALL shim. exchange() submits one request frame and
/// blocks until the response frame is available in `resp`.
pub trait Transport {
    type Error;
    fn exchange(&mut self, req: &[u8], resp: &mut [u8]) -> Result<usize, Self::Error>;
}

pub struct Client<T: Transport> { /* owns seq counter, starts at 1, increments per call */ }
impl<T: Transport> Client<T> {
    pub fn new(transport: T) -> Self;
    pub fn console_write(&mut self, bytes: &[u8]) -> Result<(), ClientError<T::Error>>;
    pub fn entropy_fill(&mut self, out: &mut [u8]) -> Result<(), ClientError<T::Error>>;
    pub fn block_capacity(&mut self) -> Result<u64, ClientError<T::Error>>;
    pub fn block_read(&mut self, lba: u64, out: &mut [u8]) -> Result<(), ClientError<T::Error>>;
    pub fn event_emit(&mut self, id: u32, data: &[u8]) -> Result<(), ClientError<T::Error>>;
}
// ClientError covers transport errors, protocol errors, seq mismatch, non-Ok status.
// entropy_fill / block_read may issue multiple frames internally to satisfy lengths
// beyond one frame's payload; chunking must be deterministic (fixed chunk size).

// ---- host feature (std) ----
pub trait Service {
    /// Handle one request; write response payload into `resp_payload`, return
    /// (Status, payload_len). Must be deterministic given identical inputs and own state.
    fn handle(&mut self, opcode: u16, payload: &[u8], resp_payload: &mut [u8])
        -> (Status, usize);

    /// Snapshot support: serialize all state that influences future responses
    /// (e.g. the entropy PRNG position). Encoding must be byte-deterministic.
    /// Stateless services return an empty Vec.
    fn save_state(&self) -> Vec<u8>;
    /// Restore previously saved state. Must accept exactly what save_state
    /// produced; reject anything else with Status-style failure (no panics).
    fn restore_state(&mut self, state: &[u8]) -> Result<(), ProtoError>;
}

pub struct Dispatcher { /* service registry */ }
impl Dispatcher {
    pub fn new() -> Self;
    pub fn register(&mut self, id: ServiceId, svc: Box<dyn Service>);
    /// Decode req from `req_buf`, route, encode response into `resp_buf`,
    /// return response frame length. Malformed input yields an encoded error
    /// response — never an Err panic path. Normative edge cases:
    /// - request header parses but is invalid (bad version/reserved/len): error
    ///   response echoing the raw service/opcode/seq fields, Status::BadRequest;
    /// - request header unparseable (too short, bad magic): error response with
    ///   service = 0, opcode = 0, seq = 0, Status::BadRequest;
    /// - resp_buf too small for the service's response payload: error response
    ///   with Status::Internal and empty payload (callers normally pass a full
    ///   page; this is defined behavior, not UB);
    /// - resp_buf smaller than one header (24 bytes): return 0 — the transport
    ///   layer treats 0 as a transport-level error (RAX = 0 in the VMCALL ABI).
    pub fn dispatch(&mut self, req_buf: &[u8], resp_buf: &mut [u8]) -> usize;

    /// Snapshot all registered services into one blob: services in ascending
    /// ServiceId order, each as (u16 id, u32 len, bytes). Byte-deterministic.
    pub fn save_state(&self) -> Vec<u8>;
    /// Restore into an identically-registered Dispatcher (same service ids);
    /// mismatched registration or malformed blob is an error.
    pub fn restore_state(&mut self, state: &[u8]) -> Result<(), ProtoError>;
}
```

Also provide reference host services used in tests and later in the real VMM:
`ConsoleSink` (collects bytes into a `Vec<u8>`), `SeededEntropy`, `MemBlockDevice` (backed by
a `Vec<u8>`, length must be a multiple of 512).

`SeededEntropy` uses exactly this xorshift64\* (normative — replay across implementations
depends on it; the golden-bytes gate pins it):

```text
state: u64, initialized to seed, except seed == 0 maps to 0x9E3779B97F4A7C15
next(): state ^= state >> 12; state ^= state << 25; state ^= state >> 27;
        return state.wrapping_mul(0x2545F4914F6CDD1D)
```

Output bytes are each `next()` value in little-endian order, truncating the final word.

## Acceptance gates

Beyond the standard gates in conventions:

1. **Golden bytes**: tests with hand-written expected byte sequences for at least one
   request and one response of every service/opcode (assert exact `[u8]` equality — this
   pins the wire format against accidental change).
2. **Round-trip property test**: arbitrary valid (service, opcode, seq, payload) encodes
   then decodes to identical values.
3. **Adversarial decode property test**: `decode()` on arbitrary byte strings (and on valid
   frames with single-byte mutations) never panics and never returns payloads outside `buf`;
   `dispatch()` on the same inputs, and with `resp_buf` sizes from 0 to 4096, always follows
   the normative edge-case behavior and never panics.
4. **End-to-end loopback test**: `Client` over a `Transport` that calls `Dispatcher`
   directly, exercising every client method against the reference services; plus: two
   identical sessions (same seed, same call sequence) produce byte-identical transcripts of
   all frames.
5. `cargo build -p hypercall-proto --no-default-features --features guest
   --target x86_64-unknown-none` succeeds (proves the guest half is genuinely no_std;
   install the target via rustup).
6. **Service snapshot round-trip test**: drive `SeededEntropy` through some requests, call
   `Dispatcher::save_state`, drive it further and record the responses, then
   `restore_state` and re-drive the same requests ⇒ byte-identical responses (the PRNG
   stream resumes exactly). Also: save/restore with mismatched service registration errors
   cleanly, and `save_state` is byte-identical across two equally-driven dispatchers.

## Non-goals

VMCALL mechanics, shared-page layout/doorbell, interrupt-driven (push) input, write support
for block, any async. Do not implement a PRNG beyond the documented xorshift64\* reference.
