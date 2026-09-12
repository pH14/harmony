#!/usr/bin/env python3
"""Strip or detect disallowed comments in Rust source files.

Preserved comments:
  - SPDX-License-Identifier lines
  - // SAFETY: blocks and /// # Safety doc sections (required by the project
    unsafe-block documentation rule and by clippy::missing_safety_doc)

Usage:
  strip-comments.py           # strip in-place, exit 0
  strip-comments.py --check   # exit 1 if any disallowed comments exist
"""

import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(
    subprocess.check_output(
        ["git", "rev-parse", "--show-toplevel"], text=True
    ).strip()
)


def is_preserved(line: str, prev_was_safety: bool) -> bool:
    stripped = line.lstrip()
    if not stripped.startswith("//"):
        return False
    comment_text = stripped[2:].strip()
    if "SPDX-License-Identifier" in comment_text:
        return True
    if comment_text.upper().startswith("SAFETY:"):
        return True
    if prev_was_safety and comment_text and not comment_text[0].isupper():
        return True
    return False


def strip_inline_comment(line: str) -> str:
    stripped = line.lstrip()
    if stripped.startswith("//"):
        return line
    if "SAFETY:" in line:
        return line

    idx = 0
    in_string = False
    in_char = False
    in_raw_string = False
    escape = False

    while idx < len(line) - 1:
        ch = line[idx]
        if escape:
            escape = False
            idx += 1
            continue
        if ch == '\\' and (in_string or in_char):
            escape = True
            idx += 1
            continue
        if in_raw_string:
            if ch == '"':
                in_raw_string = False
            idx += 1
            continue
        if in_string:
            if ch == '"':
                in_string = False
            idx += 1
            continue
        if in_char:
            if ch == "'":
                in_char = False
            idx += 1
            continue
        if ch == '"':
            in_string = True
            idx += 1
            continue
        if ch == "'" and idx + 1 < len(line) and line[idx + 1] not in (' ', ',', ')', ']', '}', ';', ':'):
            in_char = True
            idx += 1
            continue
        if ch == 'r' and idx + 1 < len(line):
            j = idx + 1
            while j < len(line) and line[j] == '#':
                j += 1
            if j < len(line) and line[j] == '"':
                in_raw_string = True
                idx = j + 1
                continue
        if ch == '/' and line[idx + 1] == '/':
            if not in_string and not in_char and not in_raw_string:
                code_part = line[:idx].rstrip()
                if code_part.strip():
                    return code_part + "\n" if line.endswith("\n") else code_part
                else:
                    return line
        idx += 1
    return line


def safety_doc_lines(lines: list[str]) -> set[int]:
    """Indices of `/// # Safety` doc lines, which clippy requires on unsafe fns."""
    keep: set[int] = set()
    block: list[int] = []
    for i, line in enumerate(lines):
        if line.lstrip().startswith("///"):
            block.append(i)
            continue
        for j, k in enumerate(block):
            if lines[k].lstrip()[3:].strip().lower() == "# safety":
                keep.update(block[j:])
                break
        block = []
    return keep


def process_file(path: Path, check_only: bool) -> list[tuple[int, str]]:
    try:
        original = path.read_text()
    except (OSError, UnicodeDecodeError):
        return []

    lines = original.splitlines(keepends=True)
    keep_safety = safety_doc_lines(lines)
    result: list[str] = []
    violations: list[tuple[int, str]] = []
    modified = False
    prev_was_safety = False
    consecutive_blank = 0

    for lineno, line in enumerate(lines, 1):
        stripped = line.lstrip()

        if stripped.startswith("//"):
            if lineno - 1 in keep_safety or is_preserved(line, prev_was_safety):
                prev_was_safety = "SAFETY:" in stripped.upper() or prev_was_safety
                result.append(line)
                consecutive_blank = 0
                continue
            else:
                prev_was_safety = False
                modified = True
                violations.append((lineno, stripped.rstrip()))
                continue
        else:
            prev_was_safety = False

        if "//" in line and lineno - 1 not in keep_safety:
            new_line = strip_inline_comment(line)
            if new_line != line:
                violations.append((lineno, stripped.rstrip()))
                line = new_line
                modified = True

        if line.strip() == "":
            consecutive_blank += 1
            if consecutive_blank <= 1:
                result.append(line)
        else:
            consecutive_blank = 0
            result.append(line)

    if not modified:
        return []

    if check_only:
        return violations

    while result and result[-1].strip() == "":
        result.pop()
    if result and not result[-1].endswith("\n"):
        result[-1] += "\n"

    new_content = "".join(result)
    if new_content != original:
        path.write_text(new_content)

    return violations


def get_rs_files() -> list[Path]:
    result = subprocess.run(
        ["git", "ls-files", "-z"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=True,
    )
    return sorted(
        REPO_ROOT / p
        for p in result.stdout.split("\0")
        if p.endswith(".rs") and ".claude/" not in p and "/target/" not in p
    )


def main() -> int:
    check_only = "--check" in sys.argv

    files = get_rs_files()
    total_violations = 0

    for path in files:
        violations = process_file(path, check_only)
        if violations:
            rel = path.relative_to(REPO_ROOT)
            if check_only:
                for lineno, text in violations:
                    print(f"{rel}:{lineno}: {text}")
            else:
                print(f"  stripped: {rel}")
            total_violations += len(violations)

    if check_only and total_violations > 0:
        print(
            f"\n{total_violations} disallowed comment(s) found.\n"
            "Code comments are not allowed, including /// and //! doc comments.\n"
            "Allowed exceptions: // SPDX-License-Identifier, // SAFETY: blocks.\n"
            "If the content is strictly required high-level context, put it in\n"
            "the nearest README.md or in docs/ instead of a code comment.\n"
            "Run `python3 scripts/strip-comments.py` to auto-fix."
        )
        return 1

    if not check_only:
        print(f"\n{total_violations} comment(s) stripped across {len(files)} files")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
