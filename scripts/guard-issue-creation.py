#!/usr/bin/env python3

import json
import re
import sys


ISSUE_CREATE = re.compile(r"(?<![\w-])gh\s+(?:--repo\s+\S+\s+|-R\s+\S+\s+)*issue\s+create\b")
ACKNOWLEDGED_CREATE = re.compile(
    r"(?<![\w-])HARMONY_INDEPENDENT_ISSUE=1\s+"
    r"gh\s+(?:--repo\s+\S+\s+|-R\s+\S+\s+)*issue\s+create\b"
)
MESSAGE = (
    "Before filing a GitHub issue, apply the README's branch-discard test. "
    "If this branch were discarded and the work would no longer matter, keep it "
    "in the current task or pull request and resolve it before merging. "
    "Only for independent work, rerun the issue-creation command with "
    "HARMONY_INDEPENDENT_ISSUE=1."
)


def is_issue_creation(event):
    tool_name = event.get("tool_name", "")
    tool_input = event.get("tool_input", {})
    if not isinstance(tool_input, dict):
        return False
    if tool_name == "Bash":
        command = tool_input.get("command", "")
        return isinstance(command, str) and bool(ISSUE_CREATE.search(command))
    return tool_name.startswith("mcp__") and bool(
        re.search(r"(?:^|_)create_issue$", tool_name, re.IGNORECASE)
    )


def acknowledged(event):
    tool_input = event.get("tool_input", {})
    command = tool_input.get("command", "") if isinstance(tool_input, dict) else ""
    return isinstance(command, str) and bool(ACKNOWLEDGED_CREATE.search(command))


def main():
    event = json.load(sys.stdin)
    if is_issue_creation(event) and not acknowledged(event):
        print(
            json.dumps(
                {
                    "hookSpecificOutput": {
                        "hookEventName": "PreToolUse",
                        "permissionDecision": "deny",
                        "permissionDecisionReason": MESSAGE,
                    }
                }
            )
        )


if __name__ == "__main__":
    main()
