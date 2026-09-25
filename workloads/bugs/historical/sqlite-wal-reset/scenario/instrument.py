"""Mark SQLite's stale-header checkpoint and WAL reset for an authored test."""

import sys
from pathlib import Path


source = Path(sys.argv[1])
text = source.read_text()
needle = "        rc = walCheckpoint(pWal, db, eMode2, xBusy2, pBusyArg, sync_flags,zBuf);"
if text.count(needle) != 1:
    raise SystemExit("SQLite checkpoint call changed; review the marker placement")
reset = "static void walRestartHdr(Wal *pWal, u32 salt1){"
if text.count(reset) != 1:
    raise SystemExit("SQLite WAL reset function changed; review the marker placement")
text = ("extern void harmony_test_checkpoint_ready(void);\n"
        "extern void harmony_test_reset(void);\n"
        "extern void harmony_test_state(int, unsigned, unsigned, unsigned);\n") + text.replace(
    needle,
    "        harmony_test_checkpoint_ready();\n"
    "        harmony_test_state(0, pWal->hdr.mxFrame, walIndexHdr(pWal)->mxFrame, walCkptInfo(pWal)->nBackfill);\n"
    + needle + "\n"
    "        harmony_test_state(1, pWal->hdr.mxFrame, walIndexHdr(pWal)->mxFrame, walCkptInfo(pWal)->nBackfill);"
).replace(reset, reset + "\n  harmony_test_reset();")
source.write_text(text)
