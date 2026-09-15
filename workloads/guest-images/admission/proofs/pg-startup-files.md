# Fixed PostgreSQL startup file closure

Read-only find of the exact image yields only
/usr/share/postgresql/17/psqlrc.sample. No active psqlrc/.psqlrc/versioned startup
file is present anywhere in the152-ELF root. Shipped psql contains compiled
/etc/postgresql-common, .psqlrc and PGSYSCONFDIR/PSQLRC handling; the exact image
and prepared environment contain no override. Fixed SQL/script/config hashes
are bound by the actual rootfs and dump. This closes the earlier system-psqlrc
inventory caveat in pg-launch-scope-review.md for this fixed trusted image,
not for arbitrary SQL/environment or added processes.
