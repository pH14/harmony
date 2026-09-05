<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Allowed-stateful MSR payload

`msr-allowed` exercises the stateful MSR portion of the CPU contract from
inside a bare-metal guest. It iterates the generated `MSR_ALLOWED_STATEFUL`
set, reads each register, writes a contract-legal value, reads it back, and
restores the original value. Each successful write is sent to the corpus report
channel; the serial output remains a stable pass/fail shape.

The companion `contract-data` package supplies the set, legal write values, and
canonical-address helpers. Host tests verify that the generated set equals the
contract's allow-stateful set and that every value has the required reserved
bits and address form. The payload asserts that the write changes the live
value, so an ignored write cannot pass by reading the old value back.

The package is part of the standalone `payloads/` workspace and is built and
run by `../run-tests.sh`. Its digest is a hardware-gate artifact because the
report channel is not modeled by stock QEMU.
