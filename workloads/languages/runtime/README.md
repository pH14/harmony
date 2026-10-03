<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Shared instrumentation runtime

From the repository root, run `bash workloads/languages/build-runtime.sh`.
The script builds the generic libvoidstar ABI shim with the faults event runtime
and prints its Docker image tag. Historical etcd and SQLite recipes accept that
tag as `HARMONY_RUNTIME_IMAGE` and copy `/out/libvoidstar.so` to
`/usr/lib/libvoidstar.so`.

The stage also builds the uninstrumented park controller and GCC trace-pc
progress fixture for language acceptance. Their symbols and attestation are
copied only into the language fixture images. The runtime is rebuilt from its
current source independently of cached language layers.
