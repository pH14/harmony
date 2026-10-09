#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Delete remote branches whose work is already on the default branch.

A branch is deleted when no open pull request uses it and one of these holds:
a merged pull request's head is exactly the branch tip, or the tip is reachable
from the default branch. The deletion applies only while the branch still
points at the tip that was judged. Every other branch is kept and listed with the reason,
so a closed-unmerged or pull-request-less branch is never deleted here.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path
from typing import Any, Callable

DELETE = "delete"
KEEP = "keep"
PULL_REQUEST_PAGE = 20
REF_PAGE = 25
NULL_OID = "0" * 40

QUERY = """
query($owner: String!, $name: String!, $first: Int!, $after: String) {
  repository(owner: $owner, name: $name) {
    id
    defaultBranchRef { name }
    refs(refPrefix: "refs/heads/", first: $first, after: $after) {
      pageInfo { hasNextPage endCursor }
      nodes {
        name
        target { oid }
        associatedPullRequests(first: 20) {
          pageInfo { hasNextPage }
          nodes {
            number
            state
            headRefOid
            baseRefName
            headRepository { nameWithOwner }
            baseRepository { nameWithOwner }
          }
        }
      }
    }
  }
}
"""

DELETE_MUTATION = """
mutation($repositoryId: ID!, $ref: GitRefname!, $before: GitObjectID!, $after: GitObjectID!) {
  updateRefs(input: {
    repositoryId: $repositoryId,
    refUpdates: [{ name: $ref, beforeOid: $before, afterOid: $after }]
  }) { clientMutationId }
}
"""


def same_repo(candidate: str | None, repo: str) -> bool:
    return candidate is not None and candidate.lower() == repo.lower()


def classify(
    branch: dict[str, Any],
    repo: str,
    default_branch: str,
    on_default: Callable[[str], bool],
) -> tuple[str, str]:
    """Return the verdict for one branch and the reason to print beside it."""
    name = branch["name"]
    tip = branch["oid"]
    if name == default_branch:
        return KEEP, "default branch"
    if branch["truncated"]:
        return KEEP, f"more than {PULL_REQUEST_PAGE} associated pull requests"
    prs = [pr for pr in branch["prs"] if same_repo(pr["head_repo"], repo)]
    for pr in prs:
        if pr["state"] == "OPEN":
            return KEEP, f"open pull request #{pr['number']}"
    for pr in prs:
        if (
            pr["state"] == "MERGED"
            and pr["head_oid"] == tip
            and pr["base"] == default_branch
            and same_repo(pr["base_repo"], repo)
        ):
            return DELETE, f"pull request #{pr['number']} merged this tip"
    if on_default(tip):
        return DELETE, f"tip is on {default_branch}"
    if not prs:
        return KEEP, "no pull request and commits not on the default branch"
    same_tip = [pr for pr in prs if pr["head_oid"] == tip]
    if same_tip:
        return KEEP, f"pull request #{same_tip[0]['number']} closed without merging"
    return KEEP, f"commits after pull request #{prs[0]['number']}"


def gh(*args: str) -> str:
    result = subprocess.run(["gh", *args], capture_output=True, text=True)
    if result.returncode != 0:
        sys.stderr.write(result.stderr)
        raise subprocess.CalledProcessError(
            result.returncode, result.args, result.stdout, result.stderr
        )
    return result.stdout


def fetch_branches(repo: str) -> tuple[str, str, list[dict[str, Any]]]:
    owner, name = repo.split("/", 1)
    default_branch = ""
    repo_id = ""
    branches: list[dict[str, Any]] = []
    after = None
    while True:
        args = [
            "api", "graphql",
            "-f", f"query={QUERY}",
            "-f", f"owner={owner}",
            "-f", f"name={name}",
            "-F", f"first={REF_PAGE}",
        ]
        if after:
            args += ["-f", f"after={after}"]
        data = json.loads(gh(*args))["data"]["repository"]
        default_branch = data["defaultBranchRef"]["name"]
        repo_id = data["id"]
        for node in data["refs"]["nodes"]:
            branches.append({
                "name": node["name"],
                "oid": node["target"]["oid"],
                "truncated": node["associatedPullRequests"]["pageInfo"]["hasNextPage"],
                "prs": [
                    {
                        "number": pr["number"],
                        "state": pr["state"],
                        "head_oid": pr["headRefOid"],
                        "base": pr["baseRefName"],
                        "head_repo": (pr["headRepository"] or {}).get("nameWithOwner"),
                        "base_repo": (pr["baseRepository"] or {}).get("nameWithOwner"),
                    }
                    for pr in node["associatedPullRequests"]["nodes"]
                ],
            })
        page = data["refs"]["pageInfo"]
        if not page["hasNextPage"]:
            break
        after = page["endCursor"]
    return default_branch, repo_id, sorted(branches, key=lambda branch: branch["name"])


def delete_branch(repo_id: str, name: str, tip: str) -> None:
    """Delete the branch only if it still points at the tip that was judged."""
    gh(
        "api", "graphql",
        "-f", f"query={DELETE_MUTATION}",
        "-f", f"repositoryId={repo_id}",
        "-f", f"ref=refs/heads/{name}",
        "-f", f"before={tip}",
        "-f", f"after={NULL_OID}",
    )


def reachable_from(ref: str) -> Callable[[str], bool]:
    def check(oid: str) -> bool:
        return subprocess.run(
            ["git", "merge-base", "--is-ancestor", oid, ref],
            capture_output=True,
        ).returncode == 0

    return check


def render_summary(
    deleted: list[tuple[str, str]],
    failed: list[tuple[str, str]],
    kept: list[tuple[str, str]],
    apply: bool,
) -> str:
    verb = "Deleted" if apply else "Would delete"
    lines = [f"## {verb} {len(deleted)} branches", ""]
    lines += [f"- `{name}`: {reason}" for name, reason in deleted]
    if failed:
        lines += ["", f"## Failed to delete {len(failed)} branches", ""]
        lines += [f"- `{name}`: {reason}" for name, reason in failed]
    interesting = [(n, r) for n, r in kept if r not in ("default branch",)
                   and not r.startswith("open pull request")]
    lines += ["", f"## Kept {len(interesting)} branches without an open pull request", ""]
    lines += [f"- `{name}`: {reason}" for name, reason in interesting]
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", required=True, help="owner/name")
    parser.add_argument("--apply", action="store_true",
                        help="delete branches instead of listing them")
    parser.add_argument("--max-deletions", type=int, default=200)
    parser.add_argument("--summary", type=Path,
                        help="file that receives the Markdown summary")
    args = parser.parse_args()

    default_branch, repo_id, branches = fetch_branches(args.repo)
    on_default = reachable_from(f"origin/{default_branch}")

    deleted: list[tuple[str, str]] = []
    failed: list[tuple[str, str]] = []
    kept: list[tuple[str, str]] = []
    for branch in branches:
        verdict, reason = classify(branch, args.repo, default_branch, on_default)
        if verdict == KEEP:
            kept.append((branch["name"], reason))
            continue
        if len(deleted) + len(failed) >= args.max_deletions:
            kept.append((branch["name"], "deletion limit reached; next run"))
            continue
        if not args.apply:
            deleted.append((branch["name"], reason))
            continue
        try:
            delete_branch(repo_id, branch["name"], branch["oid"])
        except subprocess.CalledProcessError as error:
            failed.append((branch["name"], error.stderr.strip().splitlines()[-1]))
        else:
            deleted.append((branch["name"], reason))

    summary = render_summary(deleted, failed, kept, args.apply)
    print(summary, end="")
    if args.summary:
        with args.summary.open("a") as handle:
            handle.write(summary)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
