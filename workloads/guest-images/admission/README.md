# Controlled x86 execution admission

The schema-2 component contracts pin the platform, NES/Nova and PostgreSQL
archives, filesystem metadata and every ELF digest. The `xstate` entries bind
XGETBV selector addresses and the bytes of permitted loader resolver regions.
The scanner checks the actual dependency closure, ECX-zero selector sequences,
segment permissions and absence of TLSdesc relocations before allowing an
`unused-tlsdesc` region. Startup eager binding is required for resolver exclusions.

Composition contracts bind the prepared kernel, boot arguments, process
arguments/environment, mounts and code inputs. NES and PostgreSQL use exact
manifest hashes; the Nova A–E oracle binds its guest inputs and execution
controls. Changed guest inputs fail admission until the contract is updated.

These contracts cover fixed trusted workloads. They exclude generated code,
JITs, code mutation, additional modules, loader overrides and arbitrary interior
function pointers. They are not a runtime security boundary: a writable OCI
root is not made immutable by scanning it. Kernel XSAVE canonicalization and
snapshot continuation tests are required in addition to static admission.
See the harmony-linux README for the kernel and signal-handling restrictions.

Prepare the actual execution using the workload's `prepare-admission` command,
then verify it:

```sh
python3 workloads/guest-images/verify-prepared-admission.py verify DUMP \
  --baseline workloads/guest-images/admission/nes-composition.json \
  --output REPORT_DIR
```

Use `postgres-composition.json` for PostgreSQL. Nova's A–E commands invoke
`verify-nova-oracle-admission.sh` with their actual executable and guest input
paths immediately before execution. The oracle contract requires oracle mode
and forbids a tree-seed override. No retained evidence archive or host executable
hash is part of the guest contract.
