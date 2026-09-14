# Controlled x86 execution admission

These reviewed baselines bind the exact platform, NES/Nova and bare-PostgreSQL
artifacts retained in `proofs/`. They support only the serialized default Linux
x86_64 KVM sessions in the composition manifests. They do not qualify custom
campaign/fault configurations, arbitrary OCI images, SQL, ROMs, or code changes.

The primary-agent review accepts the exact ECX-zero instruction chains under
normal trusted program control flow; it does not infer arbitrary indirect-entry
safety from linear disassembly. The reviewed glibc resolver exclusions depend
on startup eager binding and the fixed loading closure. PostgreSQL TLSdesc
exclusion separately relies on zero TLSdesc relocations across all 152 ELFs;
eager binding alone is insufficient. The checked composition binds pre-PID1
boot arguments, process arguments/environment, mounts and code inputs.

No generated code, JIT, code mutation, additional modules, loader overrides or
arbitrary interior function pointers are admitted. These are trusted workload
conditions, not a runtime security boundary. The OCI root is writable; the
scanner does not claim to enforce runtime filesystem immutability or W^X.
The retained proposal-stage reports explain the evidence and its limitations;
review acceptance is recorded in the top-level baseline reviewer fields.

Use the real `prepare-admission` example described in the NES README, then run:

```sh
python3 workloads/guest-images/verify-prepared-admission.py verify DUMP \
  --baseline workloads/guest-images/admission/nes-composition.json \
  --output NEW_REPORT_DIR
```

Use `postgres-composition.json` for the exact PostgreSQL dump. A changed input
or proof must fail verification until reviewed. A successful static admission
result is distinct from kernel/endpoint qualification and does not settle raw
restore-bitmap membership in snapshot identity. Hosted Intel, bare-metal AMD,
and broader runtime qualification remain recorded in the component README/PR.
