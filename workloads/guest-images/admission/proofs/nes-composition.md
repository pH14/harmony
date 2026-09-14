# Reviewed nes prepared composition

Accepted exact manifest 1badf764ae2c833f164bbc1677d6194476658bd3b253567ea222dc762d953dc5; prepared identity 5677677b48d2e9c7f26dff4050832b01923b7c53dc273277564f3f5928700804. Actual preparation metadata is retained in nes-prepared/. It binds the default Linux x86_64 session, kernel, ordered platform/rootfs/control archives, OCI settings and code/external inputs. The exact execution is bundle:null; no structured fault bundle is admitted.

Scope remains fixed trusted workload code and inputs, normal instruction entry, LD_BIND_NOW before each exec, no loader override, JIT/generated code, writable executable mappings or code mutation. The rootfs is writable; static admission is not runtime immutability enforcement. Other session settings, arbitrary SQL/ROMs, snapshot imports and failed-run stops are outside this review.


This refresh binds OCI platform ee6b601b9f3dc04f1547fbe5394df62933dd20e1b57139a1c0fe3d6cd40f3f70. Changed prepared files: composed-initramfs.bin, image-inputs.json, platform.cpio.gz, rootfs.cpio.gz. Session configuration, execution/control bytes and runtime configuration remain exact. The workload is the separately reviewed hosted NES image with Ubuntu glibc 2.39; its executable/loader evidence is in proofs/nes/.
