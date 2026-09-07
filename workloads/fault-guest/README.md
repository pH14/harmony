# fault-guest

`fault-guest` is the guest-side half of the standalone `faults-workload`
package. It reads `/harmony/workload.json`, creates every declared node
directory, runs the optional one-time `setup` command, starts the replica commands through `fault-runtime`, and consumes
one versioned eight-byte action from the SDK payload tape at a time.

The image contract uses one vCPU. Each logical work or recovery tick advances
the supervisor clock and runs the declared check command, giving real replica
processes an explicit deterministic opportunity to drain messages before the
guest publishes state and reaches `frame_complete`. Check failures are
reported through the SDK assertion and state registers; a capped diagnostic is
written to `/harmony/last-check.stderr` and emitted on process stderr so the
host retains an earlier failure if a later check overwrites the file.

`PauseReplica` uses one millisecond of bounded guest time per `work_ticks`,
with a one-millisecond minimum. The supervisor advances those ticks while the
replica is paused, then resumes it before the common settle and check phase;
the check command is never run against the paused process. If an advance fails,
the guest still attempts to resume the replica before reporting the failure.

On x86 Linux, SDK exchanges use the platform's `/dev/harmony` driver through
the shared `hypercall-doorbell` Linux transport. The driver owns the physical
request pages and serializes concurrent callers. The prepared rootfs includes
the platform device mount. Arm64 uses the board's reserved MMIO transport.

Build this crate for the guest architecture and place the resulting
`fault-guest` binary in the OCI rootfs as the supervisor entrypoint. The host
package owns the workload JSON and the ordered payload records.
