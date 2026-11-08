#!/usr/bin/env python3
"""Check that a nightly run on main tests the commit main pointed to when the
run was created, and say so prominently when it retests unchanged code (#1812).

The scheduled nightly ran 1329ea5883 every day from 10-29 to 11-06 because main
did not move in that window; nothing in the runs said so. This prints a GitHub
annotation either way and exits 1 when a run on main tests any other commit.
"""

from __future__ import annotations

import json
import os
import re
import sys
import urllib.error
import urllib.request
from datetime import datetime
from datetime import timedelta

MAIN_REF = "refs/heads/main"
# A nightly whose commit has been main's head for longer than this retests
# the code an earlier nightly already ran.
STALE_AFTER = timedelta(hours=24)
# GitHub picks a run's commit and stamps its creation time separately, so a
# push this shortly before the run was created may not be in it yet.
RACE_GRACE = timedelta(minutes=2)
# The Activity API lists 100 entries per page; a run is checked within
# minutes of its creation, and re-runs rarely come more than a few days later.
MAX_PAGES = 10


def parse_time(value: str) -> datetime:
    return datetime.fromisoformat(value.replace("Z", "+00:00"))


def head_at(activities: list[dict], moment: datetime) -> dict | None:
    """The newest entry at or before `moment` (`activities` are newest first)."""
    return next(
        (entry for entry in activities if parse_time(entry["timestamp"]) <= moment),
        None,
    )


def check(
    ref: str,
    sha: str,
    created_at: str,
    activities: list[dict],
) -> tuple[int, list[tuple[str, str]]]:
    """Return the exit code and the (level, message) annotations for a run.

    `activities` are main's Activity API entries, newest first as the API
    returns them: each has a `timestamp` and the commit (`after`) main
    pointed to from then on.
    """
    created = parse_time(created_at)
    current = activities[0] if activities else None
    if ref != MAIN_REF:
        head = current["after"] if current else "unknown"
        return 0, [
            ("notice", f"Manual run on {ref} at {sha}, not main (main's head: {head}).")
        ]
    if not activities:
        return 1, [("error", "The Activity API returned no history for main.")]

    at_creation = head_at(activities, created)
    if at_creation is None:
        return 0, [
            (
                "warning",
                f"Couldn't find main's head when this run was created ({created_at}); "
                f"it tests {sha}.",
            )
        ]

    head = at_creation["after"]
    since = at_creation["timestamp"]
    if sha != head:
        before = head_at(activities, created - RACE_GRACE)
        recent = {
            entry["after"]
            for entry in activities
            if created - RACE_GRACE < parse_time(entry["timestamp"]) <= created
        }
        if (before is not None and sha == before["after"]) or sha in recent:
            return 0, [
                (
                    "warning",
                    f"This nightly tests {sha}; main moved to {head} at {since}, "
                    f"moments before the run was created ({created_at}).",
                )
            ]
        return 1, [
            (
                "error",
                f"This nightly tests {sha}, but main's head was {head} when the run "
                f"was created ({created_at}). Its results don't describe main.",
            )
        ]

    annotations = [("notice", f"Testing main @ {sha} (main's head since {since}).")]
    age = created - parse_time(since)
    if age > STALE_AFTER:
        annotations.append(
            (
                "warning",
                f"main hasn't changed since {since} ({age.days} d "
                f"{age.seconds // 3600} h before this run): this nightly tests the "
                f"same commit, {sha}, as every nightly since then.",
            )
        )
    if current is not None and current["after"] != sha:
        annotations.append(
            (
                "warning",
                f"main moved to {current['after']} at {current['timestamp']}, after "
                f"this run was created (a re-run, or a merge just after it started): "
                f"its results describe {sha}, not main's current head.",
            )
        )
    return 0, annotations


def get_json(url: str, token: str) -> tuple[object, str | None]:
    """The decoded response and the next page's URL, if any; one retry."""
    try:
        return get_json_once(url, token)
    except (urllib.error.URLError, TimeoutError):
        return get_json_once(url, token)


def get_json_once(url: str, token: str) -> tuple[object, str | None]:
    request = urllib.request.Request(
        url,
        headers={
            "Accept": "application/vnd.github+json",
            "Authorization": f"Bearer {token}",
            "X-GitHub-Api-Version": "2022-11-28",
        },
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        link = response.headers.get("Link") or ""
        match = re.search(r'<([^>]+)>;\s*rel="next"', link)
        return json.load(response), match.group(1) if match else None


def main_activities(api: str, repo: str, token: str, created_at: str) -> list[dict]:
    """Main's activity, newest first, back to the last entry before `created_at`."""
    created = parse_time(created_at)
    url: str | None = f"{api}/repos/{repo}/activity?ref={MAIN_REF}&per_page=100"
    activities: list[dict] = []
    for _ in range(MAX_PAGES):
        if url is None:
            break
        page, url = get_json(url, token)
        if not isinstance(page, list):
            raise RuntimeError(f"unexpected Activity API response: {page!r}")
        activities.extend(page)
        if any(parse_time(entry["timestamp"]) <= created for entry in page):
            break
    return activities


def escape(message: str) -> str:
    """Escapes a workflow command's message."""
    return message.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")


def main() -> int:
    env = os.environ
    api = env.get("GITHUB_API_URL", "https://api.github.com")
    repo = env["GITHUB_REPOSITORY"]
    token = env["GITHUB_TOKEN"]
    try:
        run, _ = get_json(
            f"{api}/repos/{repo}/actions/runs/{env['GITHUB_RUN_ID']}", token
        )
        if not isinstance(run, dict):
            raise RuntimeError(f"unexpected workflow run response: {run!r}")
        created_at = run["created_at"]
        activities = main_activities(api, repo, token, created_at)
    except (OSError, RuntimeError, KeyError, ValueError) as err:
        print(
            f"::error title=Nightly commit::{escape(f'GitHub API request failed: {err}')}"
        )
        return 1
    code, annotations = check(
        env["GITHUB_REF"], env["GITHUB_SHA"], created_at, activities
    )
    for level, message in annotations:
        print(f"::{level} title=Nightly commit::{escape(message)}")
    summary = env.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as out:
            out.write("### Nightly commit\n\n")
            out.writelines(
                f"- **{level}**: {message}\n" for level, message in annotations
            )
    return code


if __name__ == "__main__":
    sys.exit(main())
