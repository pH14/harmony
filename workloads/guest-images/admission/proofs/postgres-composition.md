# Reviewed postgres prepared composition

Accepted exact manifest d2032e87e9cdb25242bad9997824db4af42df7202fa85865310ac8f2ab877827; prepared identity 28b0d9c068cca80dc64aea44991cdd3740647eade4b2ce3162325910014c8697. Actual preparation metadata is retained in postgres-prepared/. It binds the default Linux x86_64 session, kernel, ordered platform/rootfs/control archives, OCI settings and code/external inputs. The exact execution is bundle:null; no structured fault bundle is admitted.

Scope remains fixed trusted workload code and inputs, normal instruction entry, LD_BIND_NOW before each exec, no loader override, JIT/generated code, writable executable mappings or code mutation. The rootfs is writable; static admission is not runtime immutability enforcement. Other session settings, arbitrary SQL/ROMs, snapshot imports and failed-run stops are outside this review.


This refresh binds OCI platform ee6b601b9f3dc04f1547fbe5394df62933dd20e1b57139a1c0fe3d6cd40f3f70. Changed prepared files: composed-initramfs.bin, platform.cpio.gz. Session configuration, execution/control bytes and runtime configuration remain exact. The bare PostgreSQL 17 workload component is unchanged; this does not admit historical PostgreSQL fault images.
