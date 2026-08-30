#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Generate and verify the N6 instruction-surface sweep.

The frozen TOML row is the accounting unit. A guest run emits one bounded
``N6_OPERATION`` record per executed operation followed by one compact
``N6_ROW`` JSON completion object. This verifier deliberately does not accept
a third disposition: handled rows execute every listed operation; entropy
rows prove both feature masking and executable-image rejection.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path


CLAIMS = {"execute", "mask-and-audit"}
ARCHES = ("arm64", "x86_64")
PREFIX = "N6_ROW "
OPERATION_PREFIX = "N6_OPERATION "
OPERATION_PATTERN = re.compile(
    r"arch=(\S+) row=(\S+) operation=(\d+)/(\d+) name=(.*?) result=(\S+)$"
)
TRAP_ROWS = {
    "arm64-physical-counter",
    "arm64-live-timer-programming",
    "arm64-pmu",
    "x86-tsc",
    "x86-pmu",
    "x86-monitor-mwait",
    "x86-waitpkg",
}

TRAPS_OFF_WITNESS_ROWS = {
    "arm64": "arm64-virtual-counter",
    "x86_64": "x86-tsc",
}


class SweepError(ValueError):
    """A fail-closed table or report violation."""


def cpuid_contract_operations() -> list[str]:
    """Enumerate the finite, reviewable CPUID domain frozen by the x86 contract."""
    pairs = {(leaf, 0) for leaf in range(0x21)}
    pairs.update((0x8000_0000 + offset, 0) for offset in range(9))

    # For `*` subleaf rows, probe both the ordinary and far-boundary inputs.
    wildcard_leaves = (
        0x03, 0x05, 0x06, 0x08, 0x09, 0x0C, 0x0E, 0x0F,
        0x10, 0x11, 0x12, 0x13, 0x14, 0x17, 0x18, 0x19,
        0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F, 0x20,
    )
    pairs.update((leaf, 0xFFFF_FFFF) for leaf in wildcard_leaves)

    # Explicit and bounded subleaf surfaces in cpu-msr-contract.toml.
    pairs.update((0x04, subleaf) for subleaf in (1, 2, 3, 4, 0xFFFF_FFFF))
    pairs.update((0x07, subleaf) for subleaf in (1, 0xFFFF_FFFF))
    pairs.update((0x0B, subleaf) for subleaf in (1, 2, 0xFFFF_FFFF))
    pairs.update((0x0D, subleaf) for subleaf in range(1, 64))

    # The hypervisor and out-of-range rules are constant ranges. Probe their
    # boundaries, both ECX boundaries, and two values outside max-basic-leaf.
    pairs.update(
        (leaf, subleaf)
        for leaf in (0x4000_0000, 0x4FFF_FFFF)
        for subleaf in (0, 0xFFFF_FFFF)
    )
    pairs.update(((0x0000_0021, 0), (0xFFFF_FFFF, 0)))
    return [
        f"CPUID EAX=0x{leaf:08x} ECX=0x{subleaf:08x}"
        for leaf, subleaf in sorted(pairs)
    ]


@dataclass(frozen=True)
class Row:
    """The fields whose exact shape the generated sweep accounts for."""

    identifier: str
    arch: str
    claim: str
    operations: tuple[str, ...]


def load_rows(path: Path) -> list[Row]:
    """Load and structurally validate the frozen instruction table."""
    # The repository's macOS system Python is 3.9, before stdlib ``tomllib``.
    # This document deliberately uses a strict TOML subset (integer/string/list
    # assignments and repeated [[instruction]] tables), so parse that subset
    # fail-closed rather than adding a runtime package dependency.
    document: dict[str, object] = {}
    raw_rows: list[dict[str, object]] = []
    current: dict[str, object] | None = None
    for line_number, source_line in enumerate(path.read_text().splitlines(), start=1):
        line = source_line.strip()
        if not line or line.startswith("#"):
            continue
        if line == "[[instruction]]":
            current = {}
            raw_rows.append(current)
            continue
        if "=" not in line:
            raise SweepError(f"{path}:{line_number}: unsupported TOML syntax")
        key, encoded = (part.strip() for part in line.split("=", 1))
        try:
            value = json.loads(encoded)
        except json.JSONDecodeError as error:
            raise SweepError(
                f"{path}:{line_number}: value is outside the strict TOML subset"
            ) from error
        target = document if current is None else current
        if key in target:
            raise SweepError(f"{path}:{line_number}: duplicate key {key}")
        target[key] = value
    document["instruction"] = raw_rows
    if document.get("schema") != 1:
        raise SweepError("table schema must be exactly 1")
    loaded_rows = document.get("instruction")
    if not isinstance(loaded_rows, list) or not loaded_rows:
        raise SweepError("table must contain at least one [[instruction]] row")

    rows: list[Row] = []
    seen: set[str] = set()
    for index, raw in enumerate(loaded_rows):
        if not isinstance(raw, dict):
            raise SweepError(f"row {index} is not a table")
        identifier = raw.get("id")
        arch = raw.get("arch")
        claim = raw.get("claim")
        operations = raw.get("operations")
        operation_expansions = raw.get("operation-expansions", [])
        if not isinstance(identifier, str) or not identifier:
            raise SweepError(f"row {index} has no non-empty id")
        if identifier in seen:
            raise SweepError(f"duplicate table row: {identifier}")
        seen.add(identifier)
        if arch not in ARCHES:
            raise SweepError(f"{identifier}: unsupported arch {arch!r}")
        if claim not in CLAIMS:
            raise SweepError(f"{identifier}: forbidden third claim {claim!r}")
        if not isinstance(operations, list) or not operations or not all(
            isinstance(operation, str) and operation for operation in operations
        ):
            raise SweepError(f"{identifier}: operations must be non-empty strings")
        if not isinstance(operation_expansions, list) or not all(
            isinstance(operation, str) and operation for operation in operation_expansions
        ):
            raise SweepError(f"{identifier}: operation-expansions must be strings")
        expanded_operations = list(operations)
        for expansion in operation_expansions:
            match = re.fullmatch(
                r"(MRS|MSR) (PMEVCNTR|PMEVTYPER)\[0-30\]_EL0", expansion
            )
            if match:
                direction, register = match.groups()
                expanded_operations.extend(
                    f"{direction} {register}{slot}_EL0" for slot in range(31)
                )
            elif re.fullmatch(r"MRS ID_AA64AFR[01]_EL1", expansion):
                expanded_operations.append(expansion)
            elif expansion == "CPUID frozen contract domain":
                expanded_operations.extend(cpuid_contract_operations()[1:])
            else:
                raise SweepError(f"{identifier}: unsupported operation expansion {expansion!r}")
        operations = expanded_operations
        if len(set(operations)) != len(operations):
            raise SweepError(f"{identifier}: duplicate operation")
        if claim == "mask-and-audit" and raw.get("channel") != "entropy":
            raise SweepError(f"{identifier}: only entropy may use mask-and-audit")
        rows.append(Row(identifier, arch, claim, tuple(operations)))

    for arch in ARCHES:
        if not any(row.arch == arch for row in rows):
            raise SweepError(f"table has no {arch} rows")
    return rows


def listing(rows: list[Row]) -> str:
    """Return the committed, reviewable listing derived from the table."""
    lines = [
        "# SPDX-License-Identifier: AGPL-3.0-or-later",
        "# generated by n6-instruction-sweep.py; do not edit",
    ]
    for arch in ARCHES:
        arch_rows = [row for row in rows if row.arch == arch]
        operation_count = sum(len(row.operations) for row in arch_rows)
        lines.append(
            f"ARCH\t{arch}\trows={len(arch_rows)}\toperations={operation_count}"
        )
        for row in arch_rows:
            for ordinal, operation in enumerate(row.operations, start=1):
                lines.append(
                    f"OP\t{arch}\t{row.identifier}\t{row.claim}\t"
                    f"{ordinal}/{len(row.operations)}\t{operation}"
                )
    return "\n".join(lines) + "\n"


def arm64_body(operation: str) -> list[str]:
    """Return one AArch64 JIT body for an exact table operation."""
    if operation.startswith("MRS "):
        register = operation.removeprefix("MRS ").lower()
        # Spell newer optional registers as architectural encodings so an
        # older assembler cannot silently shrink the frozen listing.
        raw_mrs = {
            "cntpctss_el0": 0xD53BE0A0,
            "cntvctss_el0": 0xD53BE0C0,
            "id_aa64zfr0_el1": 0xD5380480,
            "id_aa64smfr0_el1": 0xD53804A0,
        }
        if register in raw_mrs:
            return [f".inst 0x{raw_mrs[register]:08x}", "ret"]
        return [f"mrs x0, {register}", "ret"]
    if operation.startswith("MSR "):
        register = operation.removeprefix("MSR ").lower()
        return ["mov x1, xzr", f"msr {register}, x1", "mov x0, xzr", "ret"]
    bodies = {
        "LDXR": ["ldxr x1, [x0]", "mov x0, x1", "ret"],
        "LDAXR": ["ldaxr x1, [x0]", "mov x0, x1", "ret"],
        "LDXP": ["ldxp x1, x2, [x0]", "eor x0, x1, x2", "ret"],
        "LDAXP": ["ldaxp x1, x2, [x0]", "eor x0, x1, x2", "ret"],
        # Store the value just loaded and retry without observing the retry
        # count. This is exactly N0's admitted side-effect-free boundary.
        "STXR": ["1: ldxr x1, [x0]", "stxr w2, x1, [x0]", "cbnz w2, 1b", "mov x0, xzr", "ret"],
        "STLXR": ["1: ldaxr x1, [x0]", "stlxr w2, x1, [x0]", "cbnz w2, 1b", "mov x0, xzr", "ret"],
        "STXP": ["1: ldxp x1, x2, [x0]", "stxp w3, x1, x2, [x0]", "cbnz w3, 1b", "mov x0, xzr", "ret"],
        "STLXP": ["1: ldaxp x1, x2, [x0]", "stlxp w3, x1, x2, [x0]", "cbnz w3, 1b", "mov x0, xzr", "ret"],
    }
    if operation not in bodies:
        raise SweepError(f"arm64 execute operation has no generator: {operation}")
    return bodies[operation]


def x86_body(operation: str) -> list[str]:
    """Return one x86-64 JIT body for an exact table operation."""
    cpuid = re.fullmatch(
        r"CPUID EAX=0x([0-9a-f]{8}) ECX=0x([0-9a-f]{8})", operation
    )
    if cpuid:
        leaf, subleaf = cpuid.groups()
        return [
            "pushq %rbx",
            f"movl $0x{leaf}, %eax",
            f"movl $0x{subleaf}, %ecx",
            "cpuid",
            "movl %eax, 0(%rdi)",
            "movl %ebx, 4(%rdi)",
            "movl %ecx, 8(%rdi)",
            "movl %edx, 12(%rdi)",
            "xorl %eax, %eax",
            "popq %rbx",
            "ret",
        ]
    bodies = {
        "RDTSC": ["rdtsc", "shlq $32, %rdx", "orq %rdx, %rax", "ret"],
        "RDTSCP": ["rdtscp", "shlq $32, %rdx", "orq %rdx, %rax", "ret"],
        "RDPMC": ["xorl %ecx, %ecx", "rdpmc", "shlq $32, %rdx", "orq %rdx, %rax", "ret"],
        "MONITOR": ["xorl %eax, %eax", ".byte 0x0f, 0x01, 0xc8", "ret"],
        "MWAIT": ["xorl %eax, %eax", "xorl %ecx, %ecx", ".byte 0x0f, 0x01, 0xc9", "ret"],
        "UMONITOR": ["movq %rdi, %rax", ".byte 0xf3, 0x0f, 0xae, 0xf0", "xorl %eax, %eax", "ret"],
        "UMWAIT": ["xorl %eax, %eax", "xorl %edx, %edx", "xorl %ecx, %ecx", ".byte 0xf2, 0x0f, 0xae, 0xf1", "ret"],
        "TPAUSE": ["xorl %eax, %eax", "xorl %edx, %edx", "xorl %ecx, %ecx", ".byte 0x66, 0x0f, 0xae, 0xf1", "ret"],
        "FXSAVE": [".byte 0x0f, 0xae, 0x07", "xorl %eax, %eax", "ret"],
        "FXSAVE64": [".byte 0x48, 0x0f, 0xae, 0x07", "xorl %eax, %eax", "ret"],
        "XSAVE": ["movl $3, %eax", "xorl %edx, %edx", ".byte 0x0f, 0xae, 0x27", "xorl %eax, %eax", "ret"],
        "XSAVEOPT": ["movl $3, %eax", "xorl %edx, %edx", ".byte 0x0f, 0xae, 0x37", "xorl %eax, %eax", "ret"],
        "XSAVEC": ["movl $3, %eax", "xorl %edx, %edx", ".byte 0x0f, 0xc7, 0x27", "xorl %eax, %eax", "ret"],
        "XSAVES": ["movl $3, %eax", "xorl %edx, %edx", ".byte 0x0f, 0xc7, 0x2f", "xorl %eax, %eax", "ret"],
        "exit-time RFLAGS.RF": ["int3", "ret"],
        "shift followed by interrupt-frame capture": ["movl $1, %eax", "shll $1, %eax", "int3", "ret"],
        "multiply followed by interrupt-frame capture": ["movl $7, %eax", "imull $9, %eax", "int3", "ret"],
        "PUSHF capture": [".byte 0x66, 0x9c, 0x66, 0x58", "movzwl %ax, %eax", "ret"],
        "PUSHFQ capture": ["pushfq", "popq %rax", "ret"],
        "SYSCALL R11 capture": ["movl $39, %eax", "syscall", "movq %r11, %rax", "ret"],
    }
    if operation not in bodies:
        raise SweepError(f"x86_64 execute operation has no generator: {operation}")
    return bodies[operation]


def guest_assembly(rows: list[Row], arch: str) -> str:
    """Generate the executable fragments copied by the in-guest JIT."""
    lines = ["# SPDX-License-Identifier: AGPL-3.0-or-later", ".text"]
    execute_index = 0
    body_for = arm64_body if arch == "arm64" else x86_body
    for row in rows:
        if row.arch != arch or row.claim != "execute":
            continue
        for operation in row.operations:
            symbol = f"n6_op_{execute_index}"
            lines.extend(
                [
                    f'.section .text.{symbol},"ax",@progbits',
                    f".global {symbol}_start",
                    f".global {symbol}_end",
                    f"{symbol}_start:",
                    *(f"\t{instruction}" for instruction in body_for(operation)),
                    f"{symbol}_end:",
                ]
            )
            execute_index += 1
    return "\n".join(lines) + "\n"


def c_string(value: str) -> str:
    """Encode a Python string as an ASCII-safe C string literal."""
    return json.dumps(value, ensure_ascii=True)


def guest_header(rows: list[Row], arch: str) -> str:
    """Generate row/operation descriptors from the same frozen table."""
    lines = [
        "/* SPDX-License-Identifier: AGPL-3.0-or-later */",
        "/* generated by n6-instruction-sweep.py; do not edit */",
        "#ifndef HARMONY_N6_GENERATED_H",
        "#define HARMONY_N6_GENERATED_H",
    ]
    execute_index = 0
    for row in rows:
        if row.arch != arch or row.claim != "execute":
            continue
        for _operation in row.operations:
            lines.append(f"extern const unsigned char n6_op_{execute_index}_start[];")
            lines.append(f"extern const unsigned char n6_op_{execute_index}_end[];")
            execute_index += 1
    lines.extend(["", "static const struct n6_operation n6_operations[] = {"])
    execute_index = 0
    operation_index = 0
    row_ranges: list[tuple[Row, int, int]] = []
    for row in rows:
        if row.arch != arch:
            continue
        first = operation_index
        for operation in row.operations:
            if row.claim == "execute":
                start = f"n6_op_{execute_index}_start"
                end = f"n6_op_{execute_index}_end"
                execute_index += 1
            else:
                start = "0"
                end = "0"
            lines.append(
                f"    {{{c_string(operation)}, {start}, {end}}},"
            )
            operation_index += 1
        row_ranges.append((row, first, len(row.operations)))
    lines.extend(["};", "", "static const struct n6_row n6_rows[] = {"])
    for row, first, count in row_ranges:
        lines.append(
            f"    {{{c_string(row.identifier)}, {c_string(row.claim)}, {first}, {count}}},"
        )
    lines.extend(
        [
            "};",
            f"#define N6_TABLE_ROW_COUNT {len(row_ranges)}",
            f"#define N6_TABLE_OPERATION_COUNT {operation_index}",
            "#endif",
        ]
    )
    return "\n".join(lines) + "\n"


def parse_operation(
    path: Path,
    line_number: int,
    line: str,
    arch: str,
    expected: dict[str, Row],
    operation_results: dict[str, list[str]],
) -> None:
    """Validate and append one ordered, table-generated operation record."""
    prefix_at = line.find(OPERATION_PREFIX)
    if prefix_at < 0:
        return
    match = OPERATION_PATTERN.fullmatch(line[prefix_at + len(OPERATION_PREFIX) :])
    if match is None:
        raise SweepError(f"{path}:{line_number}: malformed operation record")
    record_arch, identifier, ordinal_text, total_text, name, result = match.groups()
    if record_arch != arch:
        raise SweepError(
            f"{path}:{line_number}: operation arch is {record_arch!r}, want {arch!r}"
        )
    row = expected.get(identifier)
    if row is None:
        raise SweepError(f"{path}:{line_number}: unexpected operation row {identifier!r}")
    if row.claim != "execute":
        raise SweepError(f"{path}:{line_number}: mask row emitted an operation")
    ordinal = int(ordinal_text)
    total = int(total_text)
    results = operation_results.setdefault(identifier, [])
    want_ordinal = len(results) + 1
    if total != len(row.operations) or ordinal != want_ordinal:
        raise SweepError(
            f"{path}:{line_number}: {identifier} operation is {ordinal}/{total}, "
            f"want {want_ordinal}/{len(row.operations)}"
        )
    if name != row.operations[ordinal - 1]:
        raise SweepError(
            f"{path}:{line_number}: {identifier} operation {ordinal} is {name!r}, "
            f"want {row.operations[ordinal - 1]!r}"
        )
    if not result:
        raise SweepError(f"{path}:{line_number}: empty execute result")
    results.append(result)


def parse_report(path: Path, arch: str, expected: dict[str, Row]) -> dict[str, dict]:
    """Read one guest report, rejecting skips, duplicates, and loose fields."""
    found: dict[str, dict] = {}
    operation_results: dict[str, list[str]] = {}
    for line_number, line in enumerate(path.read_text().splitlines(), start=1):
        if OPERATION_PREFIX in line:
            parse_operation(
                path, line_number, line, arch, expected, operation_results
            )
            continue
        prefix_at = line.find(PREFIX)
        if prefix_at < 0:
            continue
        try:
            item = json.loads(line[prefix_at + len(PREFIX) :])
        except json.JSONDecodeError as error:
            raise SweepError(f"{path}:{line_number}: invalid JSON: {error}") from error
        if not isinstance(item, dict):
            raise SweepError(f"{path}:{line_number}: row is not an object")
        identifier = item.get("id")
        if identifier not in expected:
            raise SweepError(f"{path}:{line_number}: unexpected row {identifier!r}")
        if identifier in found:
            raise SweepError(f"{path}:{line_number}: duplicate row {identifier}")
        row = expected[identifier]
        exact = {
            "arch": arch,
            "id": row.identifier,
            "claim": row.claim,
            "operation_count": len(row.operations),
        }
        for key, value in exact.items():
            if item.get(key) != value:
                raise SweepError(
                    f"{path}:{line_number}: {identifier} {key} is {item.get(key)!r}, "
                    f"want {value!r}"
                )
        if row.claim == "execute":
            results = operation_results.get(identifier, [])
            if len(results) != len(row.operations):
                raise SweepError(
                    f"{path}:{line_number}: {identifier} did not execute every operation"
                )
            if set(item) != {"arch", "id", "claim", "operation_count", "traps_on"}:
                raise SweepError(f"{path}:{line_number}: execute row mixed claim shapes")
            if identifier in TRAP_ROWS:
                if not all(result.startswith("signal:") for result in results):
                    raise SweepError(
                        f"{path}:{line_number}: {identifier} escaped the guest trap policy"
                    )
            if item.get("traps_on") is not True:
                raise SweepError(f"{path}:{line_number}: {identifier} traps are off")
            item["results"] = results
        else:
            if item.get("feature_hidden") is not True:
                raise SweepError(f"{path}:{line_number}: {identifier} feature is visible")
            if item.get("audit_rejected") is not True:
                raise SweepError(f"{path}:{line_number}: {identifier} opcode audit accepted")
            if set(item) != {
                "arch", "id", "claim", "operation_count", "feature_hidden",
                "audit_rejected",
            }:
                raise SweepError(f"{path}:{line_number}: mask row mixed claim shapes")
        found[identifier] = item

    missing = sorted(set(expected) - set(found))
    if missing:
        raise SweepError(f"{path}: silently skipped rows: {', '.join(missing)}")
    orphaned = sorted(set(operation_results) - set(found))
    if orphaned:
        raise SweepError(f"{path}: operations without completed rows: {', '.join(orphaned)}")
    return found


def verify(rows: list[Row], arch: str, first: Path, second: Path) -> str:
    """Verify two same-seed guest runs and return their count attestation."""
    expected = {row.identifier: row for row in rows if row.arch == arch}
    first_rows = parse_report(first, arch, expected)
    second_rows = parse_report(second, arch, expected)
    for identifier in expected:
        if first_rows[identifier] != second_rows[identifier]:
            raise SweepError(f"same-seed mismatch in row {identifier}")
    operations = sum(len(row.operations) for row in expected.values())
    return (
        f"N6_SWEEP_OK arch={arch} table_rows={len(expected)} "
        f"exercised_rows={len(first_rows)} operations={operations} runs=2"
    )


def traps_off_witness(path: Path, arch: str, row: Row) -> tuple[str, ...]:
    """Read the one early row that proves a traps-off image exposes live state."""
    expected = {row.identifier: row}
    operation_results: dict[str, list[str]] = {}
    for line_number, line in enumerate(path.read_text().splitlines(), start=1):
        if OPERATION_PREFIX in line:
            prefix_at = line.find(OPERATION_PREFIX)
            candidate = line[prefix_at + len(OPERATION_PREFIX) :]
            if f"row={row.identifier} " in candidate:
                parse_operation(
                    path, line_number, line, arch, expected, operation_results
                )
            continue
        prefix_at = line.find(PREFIX)
        if prefix_at < 0:
            continue
        try:
            item = json.loads(line[prefix_at + len(PREFIX) :])
        except json.JSONDecodeError as error:
            raise SweepError(f"{path}:{line_number}: invalid JSON: {error}") from error
        if not isinstance(item, dict) or item.get("id") != row.identifier:
            continue
        exact = {
            "arch": arch,
            "id": row.identifier,
            "claim": "execute",
            "operation_count": len(row.operations),
            "traps_on": False,
        }
        for key, value in exact.items():
            if item.get(key) != value:
                raise SweepError(
                    f"{path}:{line_number}: {row.identifier} {key} is "
                    f"{item.get(key)!r}, want {value!r}"
                )
        if set(item) != {"arch", "id", "claim", "operation_count", "traps_on"}:
            raise SweepError(f"{path}:{line_number}: witness row mixed claim shapes")
        results = operation_results.get(row.identifier, [])
        if len(results) != len(row.operations):
            raise SweepError(
                f"{path}:{line_number}: {row.identifier} did not execute every operation"
            )
        if all(result.startswith("signal:") for result in results):
            raise SweepError(
                f"{path}:{line_number}: {row.identifier} did not expose live state"
            )
        return tuple(results)
    raise SweepError(f"{path}: missing traps-off witness row {row.identifier}")


def verify_traps_off(rows: list[Row], arch: str, first: Path, second: Path) -> str:
    """Require two independent traps-off runs to expose non-repeatable live state."""
    identifier = TRAPS_OFF_WITNESS_ROWS[arch]
    row = next(row for row in rows if row.arch == arch and row.identifier == identifier)
    first_results = traps_off_witness(first, arch, row)
    second_results = traps_off_witness(second, arch, row)
    if first_results == second_results:
        raise SweepError(
            f"traps-off witness {identifier} repeated; live-state divergence not observed"
        )
    return (
        f"N6_TRAPS_OFF_REJECTED arch={arch} row={identifier} "
        f"operations={len(row.operations)} runs=2"
    )


def synthetic_report(rows: list[Row], arch: str) -> str:
    """Create a valid report solely for verifier negative-control tests."""
    lines: list[str] = []
    for row in rows:
        if row.arch != arch:
            continue
        item: dict[str, object] = {
            "arch": arch,
            "id": row.identifier,
            "claim": row.claim,
            "operation_count": len(row.operations),
        }
        if row.claim == "execute":
            trapped = row.identifier in TRAP_ROWS
            results = [
                f"signal:{11 if arch == 'x86_64' else 4}"
                if trapped
                else f"synthetic-{index}"
                for index in range(len(row.operations))
            ]
            item["traps_on"] = True
            for ordinal, (operation, result) in enumerate(
                zip(row.operations, results), start=1
            ):
                lines.append(
                    f"{OPERATION_PREFIX}arch={arch} row={row.identifier} "
                    f"operation={ordinal}/{len(row.operations)} name={operation} "
                    f"result={result}"
                )
        else:
            item["feature_hidden"] = True
            item["audit_rejected"] = True
        lines.append(PREFIX + json.dumps(item, sort_keys=True, separators=(",", ":")))
    return "\n".join(lines) + "\n"


def expect_failure(label: str, action) -> None:
    """Require a planted negative to be rejected."""
    try:
        action()
    except SweepError:
        print(f"N6_NEGATIVE_OK {label}")
        return
    raise SweepError(f"planted negative unexpectedly passed: {label}")


def self_test(rows: list[Row]) -> None:
    """Exercise the verifier's positive path and meaningful planted negatives."""
    longest_result = "value:ffffffffffffffff:mem:ffffffffffffffff"
    for row in rows:
        for ordinal, operation in enumerate(row.operations, start=1):
            record = (
                f"{OPERATION_PREFIX}arch={row.arch} row={row.identifier} "
                f"operation={ordinal}/{len(row.operations)} name={operation} "
                f"result={longest_result}"
            )
            if len(record.encode("ascii")) >= 256:
                raise SweepError(
                    f"{row.identifier} operation {ordinal} exceeds guest record buffer"
                )
    with tempfile.TemporaryDirectory(prefix="harmony-n6-") as directory:
        root = Path(directory)
        for arch in ARCHES:
            good = synthetic_report(rows, arch)
            first = root / f"{arch}-first.log"
            second = root / f"{arch}-second.log"
            first.write_text(good)
            second.write_text(good)
            print(verify(rows, arch, first, second))

            lines = good.splitlines()
            missing = root / f"{arch}-missing.log"
            missing.write_text("\n".join(lines[1:]) + "\n")
            expect_failure(
                f"{arch}-missing-row",
                lambda a=arch, p=missing, q=second: verify(rows, a, p, q),
            )

            execute_index = next(
                index
                for index, line in enumerate(lines)
                if line.startswith(OPERATION_PREFIX)
            )
            mismatched = root / f"{arch}-mismatch.log"
            lines[execute_index] = re.sub(
                r"result=\S+$", "result=planted-different-result", lines[execute_index]
            )
            mismatched.write_text("\n".join(lines) + "\n")
            expect_failure(
                f"{arch}-same-seed-mismatch",
                lambda a=arch, p=first, q=mismatched: verify(rows, a, p, q),
            )

            trap_row = next(
                row for row in rows if row.arch == arch and row.identifier in TRAP_ROWS
            )
            trap_index = next(
                index
                for index, line in enumerate(good.splitlines())
                if line.startswith(PREFIX)
                and json.loads(line[len(PREFIX) :])["id"] == trap_row.identifier
            )
            trap_lines = good.splitlines()
            item = json.loads(trap_lines[trap_index][len(PREFIX) :])
            item["traps_on"] = False
            trap_lines[trap_index] = PREFIX + json.dumps(
                item, sort_keys=True, separators=(",", ":")
            )
            for index, line in enumerate(trap_lines):
                if line.startswith(
                    f"{OPERATION_PREFIX}arch={arch} row={trap_row.identifier} "
                ):
                    trap_lines[index] = re.sub(
                        r"result=\S+$", "result=value:0000000000000001", line
                    )
            traps_off = root / f"{arch}-traps-off.log"
            traps_off.write_text("\n".join(trap_lines) + "\n")
            expect_failure(
                f"{arch}-traps-off",
                lambda a=arch, p=traps_off, q=second: verify(rows, a, p, q),
            )

            witness_identifier = TRAPS_OFF_WITNESS_ROWS[arch]
            witness_item = {
                "arch": arch,
                "id": witness_identifier,
                "claim": "execute",
                "operation_count": len(
                    next(row.operations for row in rows if row.identifier == witness_identifier)
                ),
                "traps_on": False,
            }
            witness_row = next(row for row in rows if row.identifier == witness_identifier)
            witness_results = [
                f"value:{index + 1:016x}" for index in range(len(witness_row.operations))
            ]
            witness_first = root / f"{arch}-traps-off-witness-first.log"
            witness_lines = [
                f"{OPERATION_PREFIX}arch={arch} row={witness_identifier} "
                f"operation={ordinal}/{len(witness_row.operations)} name={operation} "
                f"result={result}"
                for ordinal, (operation, result) in enumerate(
                    zip(witness_row.operations, witness_results), start=1
                )
            ]
            witness_lines.append(
                PREFIX + json.dumps(witness_item, sort_keys=True, separators=(",", ":"))
            )
            witness_first.write_text("\n".join(witness_lines) + "\n")
            witness_results[0] = "value:ffffffffffffffff"
            witness_lines[0] = re.sub(
                r"result=\S+$", f"result={witness_results[0]}", witness_lines[0]
            )
            witness_second = root / f"{arch}-traps-off-witness-second.log"
            witness_second.write_text("\n".join(witness_lines) + "\n")
            print(verify_traps_off(rows, arch, witness_first, witness_second))
            expect_failure(
                f"{arch}-traps-off-repeated",
                lambda a=arch, p=witness_first: verify_traps_off(rows, a, p, p),
            )

            mask_index = next(
                index
                for index, line in enumerate(good.splitlines())
                if line.startswith(PREFIX)
                and json.loads(line[len(PREFIX) :])["claim"] == "mask-and-audit"
            )
            mask_lines = good.splitlines()
            item = json.loads(mask_lines[mask_index][len(PREFIX) :])
            item["feature_hidden"] = False
            mask_lines[mask_index] = PREFIX + json.dumps(
                item, sort_keys=True, separators=(",", ":")
            )
            visible = root / f"{arch}-visible.log"
            visible.write_text("\n".join(mask_lines) + "\n")
            expect_failure(
                f"{arch}-visible-entropy",
                lambda a=arch, p=visible, q=second: verify(rows, a, p, q),
            )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--table", type=Path, default=Path("docs/determinism-instructions.toml")
    )
    subparsers = parser.add_subparsers(dest="command", required=True)
    subparsers.add_parser("listing")
    assembly_parser = subparsers.add_parser("guest-assembly")
    assembly_parser.add_argument("--arch", choices=ARCHES, required=True)
    header_parser = subparsers.add_parser("guest-header")
    header_parser.add_argument("--arch", choices=ARCHES, required=True)
    verify_parser = subparsers.add_parser("verify")
    verify_parser.add_argument("--arch", choices=ARCHES, required=True)
    verify_parser.add_argument("--run", type=Path, action="append", required=True)
    traps_off_parser = subparsers.add_parser("verify-traps-off")
    traps_off_parser.add_argument("--arch", choices=ARCHES, required=True)
    traps_off_parser.add_argument("--run", type=Path, action="append", required=True)
    subparsers.add_parser("self-test")
    args = parser.parse_args()

    try:
        rows = load_rows(args.table)
        if args.command == "listing":
            sys.stdout.write(listing(rows))
        elif args.command == "guest-assembly":
            sys.stdout.write(guest_assembly(rows, args.arch))
        elif args.command == "guest-header":
            sys.stdout.write(guest_header(rows, args.arch))
        elif args.command == "verify":
            if len(args.run) != 2:
                raise SweepError("verify requires exactly two --run reports")
            print(verify(rows, args.arch, args.run[0], args.run[1]))
        elif args.command == "verify-traps-off":
            if len(args.run) != 2:
                raise SweepError("verify-traps-off requires exactly two --run reports")
            print(verify_traps_off(rows, args.arch, args.run[0], args.run[1]))
        else:
            self_test(rows)
    except (OSError, SweepError) as error:
        print(f"N6_SWEEP_FAIL: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
