# Fixed PostgreSQL launch scope review (2026-09-14)

Read-only review; no production changes or execution tests. Current integration source HEAD is b51ebb8d036bb98e831001ca4574c8c0883db1f7 with existing uncommitted admission/build/README changes. This is later than the original loader task's 0f76f5a7; source and image correspondence is explicitly checked below where available.

## Exact image evidence

ms02 `/tmp/harmony-pg-g1-build-20260914/pg-root`:

- OCI config digest `27873751d800b7f4197c81e3bee5572ad53f57f5ff2692966669f0a2c0432200`: entrypoint `/usr/local/bin/postgres-workload.sh`, empty Cmd, only image environment `HARMONY_POSTGRES_VARIANT=postgres`, uid/gid 0:0, cwd `/var/lib/postgresql`. Layer digest `f5fa08bfbe3c5a14531997cad5cf6039f6be55bfaeedfa8dd57ee64f26dffbd7`.
- Entrypoint SHA256 `57de002a3c2aaf4988d1e5f2d412b27a3660cf40d9a59fd36506220122e9070d`, identical to current source; root-owned 0755.
- `/workload.sql` SHA256 `11fcdee04c6865abdb796585152d35c507f277daff0106f08ad46e5fcdbea480`, root-owned 0644: create ledger, then 20 INSERT/SELECT pairs using core gen_random_uuid and clock_timestamp. No LOAD, CREATE EXTENSION, CREATE FUNCTION, SET, ALTER SYSTEM, COPY PROGRAM, or psql metacommands.
- `postgresql.conf` SHA256 `dc8d1c3c73293f7fdb9dd06ce6a06452c7ad6268a9850844298bc5cb1bc3f1a4`, uid/gid70 0600: final settings jit=off, listen_addresses='', Unix socket /tmp, autovacuum=off. No active preload/include settings. `postgresql.auto.conf` contains comments only; SHA256 `0874e665ecf5c0a6135a73158ea1b71a959dafb809574810da5c9cc28c0c63ed`.
- llvmjit.so absent; plpgsql.so present. `/etc/ld.so.preload` and `/var/lib/postgresql/.psqlrc` absent. `/etc/nsswitch.conf` has only passwd:files and group:files.

## Executable controls present

`consonance/oci/src/bundle.rs:273-280` removes any image LD_BIND_NOW assignment and appends LD_BIND_NOW=1. The supervisor OCI process also has LD_BIND_NOW=1 at line388. `supervisor/src/process.rs:27-37` clears inherited environment and installs only execution-spec env. Thus host loader overrides are not inherited by the workload. `linux/oci-init.sh:6-8` exports the variable for platform init; default kernel cmdline in `consonance/client/src/session.rs:19` includes it before PID1 startup. Existing tests cover OCI binding replacement, but were not rerun in this review.

The fixed shell script exports PATH, HOME, locale/timezone and PGUSER/PGHOST/PGDATABASE, then runs `busybox setuidgid postgres postgres -D /var/lib/postgresql/data`; polls with psql `-q -c 'SELECT 1'`; runs psql `-q -At -F '|' -P pager=off -v ON_ERROR_STOP=1 -f /workload.sql`; stops with pg_ctl `-D ... -m fast -W stop`. It does not unset LD_BIND_NOW; ordinary descendants inherit it and PostgreSQL backends fork. With this exact environment and script, normal PLT startup exclusion has a concrete launch path.

The SQL/config and absence of the LLVM provider support no normal JIT generation for this workload. They do not prohibit all dlopen: initdb's plpgsql and NSS providers remain shipped; all 152 inventoried ELFs (not just DT_NEEDED-reachable ones) having zero TLSDESC relocations is therefore the useful TLSDESC condition. Normal dlopen of unchanged inventoried modules does not invalidate that condition, and LD_BIND_NOW remains effective for such dlopen per loader source review.

## Assumptions still required; not enforced globally

1. Launch exactly this image, default argv/variant, fixed SQL/config/seeded catalogs, and no extra processes or external mounts that replace executable/configuration inputs. `bundle.rs:262-263` deliberately permits command override; the image identity alone does not bind requested argv or external inputs.
2. No loader overrides in image/execution env. The generic builder preserves every image env entry except LD_BIND_NOW; it does not reject LD_PROFILE, LD_AUDIT, LD_PRELOAD, LD_LIBRARY_PATH or GLIBC_TUNABLES. Their absence is true of this inspected config, not a universal validator rule. Likewise PGOPTIONS/PSQLRC/PGSERVICE and psql startup files must stay absent/controlled; psql is not invoked with -X. This review checked the home .psqlrc only, not every compiled system psqlrc lookup path.
3. LD_BIND_NOW remains nonempty before every future exec. Preparation and the known shell path establish it, but there is no kernel-level invariant preventing admitted code from changing its environment and execing again. Custom kernel command lines must preserve the pre-PID1 setting.
4. Runtime code must stay within the admitted image plus separately admitted platform/runtime overlays. The 152-ELF root inventory is not the platform init/runc/supervisor closure. No runtime module-load allowlist or executable-map digest check was established here.
5. No code mutation, generated code, or unexpected control transfer. OCI root has `readonly:false` at bundle.rs:392; config/data are writable by postgres; shell runs as root before setuid children. Root-owned binaries provide ordinary uid separation but do not make the whole rootfs immutable. The scanner rejects writable-executable ELF segments, while explicitly stating it does not enforce runtime W^X or generated-code restrictions. No runtime prevention of mmap/mprotect-generated executable memory is established by this review.
6. SQL access remains restricted to the fixed driver. pg_hba allows local trust, including superuser connections; no TCP listener helps bound external input, but an added in-container process could connect to /tmp. jit=off and preload defaults are mutable settings, not immutable policy. No untrusted SQL is supported by this evidence.

## Conclusion

The exact normal ledger launch has concrete eager-binding and no-JIT evidence; unchanged inventoried dlopen modules preserve the zero-TLSDESC argument. The source does not currently enforce the full universal scope labels “immutable rootfs”, “no overrides”, “no generated code”, or “before every exec.” These must be digest-bound reviewed trusted-workload assumptions or gain separate enforcement before stronger claims. No admission approval is made here.

Reviewed launch implementation SHA256: bundle.rs `08ecf22ba099b266d4aaed6461699b720e5e3e50388a55f25e7c6ed2c9ff9d57`; supervisor/process.rs `69776dce08c829a6db7fcf845415bf0791ff8296be430284d19d1bbff407f9d0`. Combine with loader-review.md and the 152-ELF relocation inventory; do not substitute this report for their digest checks.
