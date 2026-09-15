# Reviewed OCI platform, e2a9ac58

Accepted by the Codex primary agent after exact source verification, a fresh whole-archive GNU instruction/dependency scan, and the new supervisor selector/incoming-edge review. Source key: 8d0affa9201175cdc8abd1c51594dfa060463edb053b6f6ba43e2a001cae3fdb. Kernel remains 7ce25244cf1d138db1286ce61fb1c880bd224b2866ffaebd1ec19d04b2ad69b6.

The OCI archive ee6b601b9f3dc04f1547fbe5394df62933dd20e1b57139a1c0fe3d6cd40f3f70 changes only the supervisor entry. Its code changes materially; no whole-binary equivalence is claimed. The fresh scan finds three ELFs, no new dependency, writable/executable segment, executable stack or text relocation. The new supervisor has only the ECX-zero XGETBV wrapper at 0x6ce90, with instruction at 0x6ce92 and direct caller at 0xe16c. Exact selector bytes and incoming-edge evidence are retained in platform-selectors.md and platform-publisher-e2a9ac58/.

BusyBox and runc have unchanged file hashes, so their exact previous resolver/selector evidence remains applicable. Main's structured-bundle launch/recovery paths are outside the admitted bundle:null sessions; their dispatch and ordinary execution environment path were reviewed in source. The changed libvoidstar in the separate direct initramfs is not covered by this OCI component approval. No arbitrary indirect entry, generated code, mutation or runtime filesystem immutability is established.


Fresh parsed GNU inventory: platform-publisher-e2a9ac58/platform-candidate.json. Archive metadata changes were independently compared against the previous complete inventory.
