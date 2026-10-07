# Explore NES workloads

Harmony can explore games using the same searches, findings, and branches as
application testing. The NES workload interprets controller input and game
progress, with adapters for SMB and Nova.

Follow the [NES workload setup](https://github.com/pH14/harmony/blob/main/workloads/nes/README.md)
for ROM identification and pinned emulator requirements. You’ll need to supply
ROMs you’re entitled to use; they aren’t distributed with this documentation.

## Choose a runner

Native QuickNES is the default and requires the pinned core library. You can
also use the Consonance adapter to run a prepared game environment inside a
guest; it currently requires Linux KVM. Configure that guest image in the NES
workload options.

## Investigate a game execution

You can branch at recorded controller-action boundaries and use the retained
game evidence to guide further searches. The native runner has no guest shell
or application console logs, and its recordings don’t support rewinding by
virtual time.

The [command reference](reference/cli.md) covers the shared syntax. Which
operations are available depends on the workload and runner, and Harmony reports
an error if you request an unsupported capability.
