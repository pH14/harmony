# Exact UML785 core investigation

This branch replaces the existing UML workflow with a manual-only diagnostic.
It must not be merged. It downloads the five immutable files from run
39259407907, artifact
`uml-artifacts-7e18c3820d644d44aa3e55e4655391895cfcd784-x86_64`.
No UML kernel, qualifier, profile or fixture is rebuilt or modified. SHA-256
constants in `run-uml785-core.py` are checked before and after execution.

The ordinary GitHub runner UID must have no effective capabilities and report
an AMD CPU. An Intel allocation is blocked, not AMD evidence. The original
launch/replay warmup precedes one to three complete checkpoint suites. Execution
stops after the first failed suite. Each command, report and console output is
retained. The outcome distinguishes the original `checkpoint_import_values`
`Signaled(11)` error from other failures. No reproduction is a diagnostic
result, not a passing qualification or an explanation of the original failure.

There is no syscall or page-fault tracing. ASLR and CPU affinity are unchanged.
The diagnostic changes the ordinary process core soft limit to unlimited,
temporarily installs an executable-filtered core pipe and raises
`core_pipe_limit` to four. Both host core settings are saved before changes and
restored in an always step; the ephemeral runner also bounds their lifetime if
the job is forcibly terminated. A positive pipe limit lets the collector inspect
the still-present crashing process in `/proc`.

The root-owned collector accepts a core only when the kernel-supplied UID,
independently read process UID and exact executable SHA identify the ordinary
fixture qualifier or UML kernel. Other processes are discarded before any
memory is retained. The qualifier receives only PATH, owned HOME/TMPDIR and C
locale variables; the production UML launcher separately clears its child
environment. No CI tokens, ambient environment, unrelated processes, source
snapshots or normal demand-page faults are collected. The core may contain
only this repository's qualification fixture and its host process state.

Pipe core dumps ignore `RLIMIT_CORE`, so the collector independently limits
each core to 2 GiB, all cores to 4 GiB, and the file count to two. Truncation is
recorded explicitly; it is not a complete core. A bounded debugger attempt also
extracts whatever notes/registers remain readable in a truncated core; its
output starts with the truncation status. Automatic script loading and
debuginfod are disabled. The crashing process's maps, status and coredump_filter
are retained independently of core memory segments, bounded to 1 MiB each.
No environ data is read. This preserves mapping evidence when the default core
filter omits a shared file-backed restored stack. The upload allowlist includes core metadata/bytes,
bounded backtraces/registers/maps, host CPU/kernel/ASLR/core settings, and the
owned qualifier reports/console. Temporary work trees and raw RAM images from
successful cases are excluded. Retention is seven days.

The Linux [core sysctl documentation](https://www.kernel.org/doc/html/latest/admin-guide/sysctl/kernel.html)
defines `%P`, `%u`, pipe collectors and `core_pipe_limit` process lifetime.
The [Linux man-pages core documentation](https://www.kernel.org/pub/linux/docs/man-pages/book/man-pages-6.15.pdf)
documents the pipe exception to `RLIMIT_CORE`.

Parent review is required before manually dispatching this investigation.
PR802's source and shipping workflow remain unchanged.
