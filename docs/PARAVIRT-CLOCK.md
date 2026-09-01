# Paravirtual clock page

The optional pvclock page publishes consonance virtual time to a cooperative
guest without exposing a host clock.

A guest registers one aligned page through the frozen doorbell protocol and
then performs the required architecture time read. The host writes a canonical
initial page and refreshes it after serviced exits. The sequence field follows
the odd/write/even protocol, and canonical writes clear every reserved byte.

Published values are derived only from the exit-count virtual clock:

- `vns` is the current accumulated virtual nanoseconds;
- `guest_clock` is the integer guest-frequency projection plus the modeled
  guest clock offset;
- `guest_clock_hz`, ABI version, flags, and vCPU index are frozen contract
  values.

The page is guest RAM and therefore participates in snapshots and state hashes.
A restore republishes the restored clock value before the guest resumes.
Registration state and the guest clock parameters are serialized in
`vm-state`; unsupported or malformed registrations fail closed.

Portable tests cover canonical encoding, torn-read rejection, monotonic
refresh, registration, snapshot/restore, and a planted corrupted-page oracle.
Live ARM and x86 gates compare the page against the normalized virtual-time
trace.
