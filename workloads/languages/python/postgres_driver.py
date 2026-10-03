# SPDX-License-Identifier: AGPL-3.0-or-later
import subprocess
import sys
from pathlib import Path

from antithesis.assertions import always, reachable
from antithesis._internal import coverage

PGBIN = "/usr/lib/postgresql/bin/"
AS_POSTGRES = ["setpriv", "--reuid=70", "--regid=70", "--clear-groups"]
PSQL = AS_POSTGRES + [PGBIN + "psql", "-h", "/tmp", "-U", "postgres", "-d", "faultlab",
                      "-v", "ON_ERROR_STOP=1", "-qtAX"]


def knob(name, default):
    values = [item.split("=", 1)[1] for item in Path("/proc/cmdline").read_text().split()
              if item.startswith(name + "=")]
    return int(values[-1]) if values else default


def run(command):
    return subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                          text=True, check=False)


def sql(*statements):
    command = list(PSQL)
    for statement in statements:
        command.extend(("-c", statement))
    result = run(command)
    if result.returncode:
        with open("/run/hook.err", "a") as log:
            log.write(result.stdout)
    return result.returncode


def check():
    ready = run(AS_POSTGRES + [PGBIN + "pg_isready", "-h", "/tmp", "-U", "postgres", "-d", "faultlab"])
    if ready.returncode:
        reachable("amcheck found the server down")
        return 0
    result = run(AS_POSTGRES + [PGBIN + "pg_amcheck", "-h", "/tmp", "-U", "postgres", "-d", "faultlab",
                              "--heapallindexed", "--index=cic_k_idx"])
    missing = "lacks matching index tuple" in result.stdout
    if missing or result.returncode == 0:
        reachable("amcheck compared the index")
        always(not missing, "every heap tuple has an index entry")
    else:
        reachable("amcheck failed without a finding")
    return 0


def main(hook):
    resolver = coverage.activate("/symbols/python-postgres.sym.tsv")
    if resolver is None:
        raise RuntimeError("PostgreSQL driver coverage did not activate")
    if hook == 1:
        rows = knob("faultlab.churn_rows", 20)
        slices = knob("faultlab.churn_slices", 2)
        rounds = knob("faultlab.churn_rounds", 1200)
        status = sql("SET synchronous_commit = off;", f"CALL churn({rows}, {slices}, {rounds});")
        message = "churn finished"
    elif hook == 2:
        status = sql("DROP INDEX IF EXISTS cic_k_idx;")
        if status == 0:
            status = sql("CREATE INDEX CONCURRENTLY cic_k_idx ON cic(k);")
        message = "concurrent index build finished"
    elif hook == 3:
        return check()
    elif hook == 4:
        status = sql("VACUUM cic;")
        message = "vacuum finished"
    else:
        raise ValueError(f"unknown hook {hook}")
    if status == 0:
        reachable(message)
    return status


if __name__ == "__main__":
    raise SystemExit(main(int(sys.argv[1])))
