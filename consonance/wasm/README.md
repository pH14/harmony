<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# WebAssembly execution

The backend remains unavailable to ordinary users until admission, sessions,
services, portable snapshots and host qualification pass. The runtime experiment
selects Wasmi 0.46.0 with a complete continuation extension. The continuation
fixtures, pinned patch, source preparation and comparison transforms live in
`qualification/`. Workload builds and measured evidence live in
the workload package under `workloads/`.

The continuation records live scalar registers and call frames, stable code
positions, mutable globals, function-index tables and passive segment status.
A fresh instance replaces the old runtime during restore, without re-running
initialization. The experimental restore API accepts trusted captures only;
production artifact validation remains required before enabling the backend.
