#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Report which functions a historical search's parks landed in.

A search records the module offset of every site where an event park fired
(`park_sites` in campaign-summary.json). This maps those offsets to functions
through the image's top-level `/symbols/*.sym.tsv` tables, read from a saved
OCI image archive or a directory, and reports the most-visited functions and
whether the case's `oracle.fix_functions` were reached. It is a diagnostic
for reading campaigns; nothing it computes reaches the search.

    historical-reach.py --symbols IMAGE.oci|DIR SEARCH... [--case CASE.json]
"""

from __future__ import annotations

import argparse
import bisect
import collections
import io
import json
import sys
import tarfile
from pathlib import Path

NEAREST_LIMIT = 1 << 16


def tables_from_archive(path: Path) -> dict[str, bytes]:
    found: dict[str, bytes] = {}
    with tarfile.open(path) as outer:
        for member in outer.getmembers():
            if not member.isfile():
                continue
            data = outer.extractfile(member).read()
            try:
                layer = tarfile.open(fileobj=io.BytesIO(data))
            except tarfile.TarError:
                continue
            with layer:
                for entry in layer.getmembers():
                    name = entry.name.lstrip("./")
                    if entry.isfile() and name.startswith("symbols/") and name.count("/") == 1 and name.endswith(".sym.tsv"):
                        found[name] = layer.extractfile(entry).read()
    return found


def tables_from_directory(path: Path) -> dict[str, bytes]:
    return {f"symbols/{table.name}": table.read_bytes() for table in sorted(path.glob("*.sym.tsv"))}


def parse(tables: dict[str, bytes]) -> tuple[list[int], list[str]]:
    symbols = []
    for data in tables.values():
        for line in data.decode(errors="replace").splitlines():
            fields = line.split("\t")
            if len(fields) == 2:
                try:
                    symbols.append((int(fields[0], 16), fields[1].strip()))
                except ValueError:
                    continue
    symbols.sort()
    return [address for address, _ in symbols], [name for _, name in symbols]


def resolve(addresses: list[int], names: list[str], offset: int) -> str | None:
    index = bisect.bisect_right(addresses, offset) - 1
    if index < 0 or offset - addresses[index] > NEAREST_LIMIT:
        return None
    return names[index]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--symbols", type=Path, required=True, help="saved OCI image archive or symbols directory")
    parser.add_argument("--case", type=Path, help="case.json whose oracle.fix_functions to look for")
    parser.add_argument("--function", action="append", default=[], help="another function to look for")
    parser.add_argument("--top", type=int, default=15)
    parser.add_argument("searches", nargs="+", type=Path, help="search output directories")
    args = parser.parse_args()

    tables = tables_from_directory(args.symbols) if args.symbols.is_dir() else tables_from_archive(args.symbols)
    if not tables:
        print(f"no top-level symbol tables in {args.symbols}", file=sys.stderr)
        return 1
    addresses, names = parse(tables)
    wanted = list(args.function)
    if args.case:
        wanted += json.loads(args.case.read_text())["oracle"].get("fix_functions") or []

    for search in args.searches:
        summary = json.loads((search / "campaign-summary.json").read_text())
        sites = summary.get("park_sites") or {}
        functions: collections.Counter[str] = collections.Counter()
        unresolved = 0
        for key, count in sites.items():
            name = resolve(addresses, names, int(key))
            if name is None:
                unresolved += 1
            else:
                functions[name] += int(count)
        print(f"## {search}")
        print(f"park sites {len(sites)}, resolved {len(sites) - unresolved}, functions {len(functions)}")
        for name in wanted:
            print(f"  {name}: {'reached' if functions[name] else 'not reached'} ({functions[name]} landings)")
        for name, count in functions.most_common(args.top):
            print(f"  {count:6d} {name}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
