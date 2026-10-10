<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# SQLite 3.50.1, held out

This case runs the [SQLite general workload](../sqlite-wal-general/README.md)
unchanged against the official SQLite 3.50.1 amalgamation, built with the
general case's flags and coverage. The image checks the archive's SHA-256 and
that `sqlite3.h` names the 3.50.1 release check-in. No focused workload exists
for it, and neither the workload nor the search was changed while looking at
it.

SQLite 3.50.2 fixed a WAL bug present since 3.11.0: rolling back to a savepoint
after a transaction had spilled dirty pages to the WAL could leave frames that
recovery reads as invalid, so a process kill before the next checkpoint could
lose transactions committed after it ([forum report](https://sqlite.org/forum/forumpost/b490f726db),
[release notes](https://sqlite.org/releaselog/3_50_2.html)). The general
workload's savepoints, large transactions and small page caches reach that
code.

The scored assertion is `sqlite preserves acknowledged commits`, with `sqlite
general compared committed rows` as its evidence; `integrity_check` is
reported beside it. See [DISCOVERY.md](../DISCOVERY.md#held-out-cases) for how
the held-out cases were chosen.
