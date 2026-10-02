<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# WebAssembly guest transport

`WasmTransport` implements the existing hypercall client transport over the
versioned `harmony_v1.request` import. It preserves service/opcode, sequence,
payload and protocol error semantics. `ImportedRequest` is available on wasm32;
other targets can supply a `Request` implementation for codec checks.

The host bounds and validates the two linear-memory ranges. Imported requests
can suspend with a live guest stack; buffers therefore remain part of the
portable continuation until the host completes the import atomically.
