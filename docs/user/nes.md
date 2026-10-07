# Explore NES workloads

NES exploration uses the same search, finding, and branch vocabulary with a
different workload package. Game-specific adapters interpret controller input,
progress, and outcomes. The current package recognizes SMB and Nova.

Start with the [NES workload setup](https://github.com/pH14/harmony/blob/main/workloads/nes/README.md),
which owns ROM identification and the pinned emulator requirements. Supply ROMs
you are entitled to use; the documentation site does not distribute them.

## Choose execution

Native QuickNES is the default runner. It needs the pinned core library.
The Consonance adapter instead runs the prepared game environment inside a guest
and currently requires Linux KVM. Its guest image belongs to the workload's
options; generic CLI settings do not contain game-specific image fields.

## Know which investigation tools apply

Recorded controller actions and retained game evidence support branching and
further search. A guest shell and application console logs are not capabilities
of the native NES runner. Virtual-time rewinds are not available for its
recordings. Use recorded action boundaries and the package's evidence instead.

[Command help](reference/cli.md) describes the shared syntax; the package decides
which operations it supports. An unsupported capability should produce an
explicit error.
