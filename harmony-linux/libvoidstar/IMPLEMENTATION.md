# `libvoidstar` implementation record

SPDX-License-Identifier: AGPL-3.0-or-later

The implementation was written from the public dynamic-symbol contract, without vendor
source reuse. `fuzz_json_data(const char *, size_t)`, `fuzz_get_random(void)`, and
`fuzz_flush(void)` match the SDK-facing ABI. Both the legacy coverage names ruled by
R-L3 and the current sanitizer guard names are exported as deterministic no-ops.

All device transactions hold one process-local mutex. The driver is authoritative for
cross-process serialization, sequence allocation, JSON attribution, and the seeded
entropy stream. Any device error fails closed: events are dropped and entropy returns
zero rather than falling back to host randomness.
