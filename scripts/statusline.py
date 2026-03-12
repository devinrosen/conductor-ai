#!/usr/bin/env python3
"""
Conductor status line for Claude Code.

Reads active workflow runs from ~/.conductor/conductor.db in read-only mode
and outputs a status summary for the Claude Code status bar.

Protocol: Claude Code writes a JSON object to stdin; this script reads it
(and ignores it) then writes a plain-text status line to stdout.
"""

import json
import os
import sqlite3
import sys
from datetime import datetime, timezone


DB_PATH = os.path.expanduser("~/.conductor/conductor.db")

# Status display order (lower = higher priority)
STATUS_ORDER = {"waiting_gate": 0, "running": 1, "failed": 2, "completed": 3}

# Icons per status
ICONS = {
    "waiting_gate": "⏳",
    "running": "▶ ",
    "failed": "✗ ",
    "completed": "✓ ",
}


def format_elapsed(started_at: str | None) -> str:
    """Format elapsed time since started_at (ISO 8601 string)."""
    if not started_at:
        return ""
    try:
        # Parse ISO 8601; Python 3.6 fromisoformat doesn't handle trailing Z
        started_at = started_at.replace("Z", "+00:00")
        start = datetime.fromisoformat(started_at)
        if start.tzinfo is None:
            start = start.replace(tzinfo=timezone.utc)
        elapsed = datetime.now(tz=timezone.utc) - start
        total_secs = int(elapsed.total_seconds())
        if total_secs < 0:
            return ""
        hours, remainder = divmod(total_secs, 3600)
        minutes, seconds = divmod(remainder, 60)
        if hours > 0:
            return f"{hours}h {minutes}m"
        if minutes > 0:
            return f"{minutes}m {seconds}s"
        return f"{seconds}s"
    except (ValueError, OverflowError):
        return ""


def open_db() -> sqlite3.Connection | None:
    """Open the conductor DB in read-only mode. Returns None if unavailable."""
    if not os.path.exists(DB_PATH):
        return None
    uri = f"file:{DB_PATH}?mode=ro"
    try:
        conn = sqlite3.connect(uri, uri=True, timeout=0.5)
        conn.row_factory = sqlite3.Row
        return conn
    except sqlite3.OperationalError:
        return None


def get_runs(conn: sqlite3.Connection) -> list[dict]:
    """Query active workflow runs from the conductor DB."""
    try:
        cursor = conn.cursor()
        # Fetch root runs (no parent) that are active or recently completed
        cursor.execute(
            """
            SELECT
                id,
                workflow_name,
                status,
                started_at,
                ended_at,
                worktree_id,
                repo_id
            FROM workflow_runs
            WHERE parent_workflow_run_id IS NULL
              AND status IN ('running', 'waiting_gate', 'failed', 'completed')
            ORDER BY started_at DESC
            LIMIT 20
            """
        )
        return [dict(row) for row in cursor.fetchall()]
    except sqlite3.OperationalError:
        return []


def _batch_worktree_labels(
    conn: sqlite3.Connection, runs: list[dict]
) -> dict[str, str]:
    """Return {worktree_id: 'repo_slug/wt_slug'} for all worktree_ids in runs."""
    worktree_ids = [r["worktree_id"] for r in runs if r.get("worktree_id")]
    labels: dict[str, str] = {}
    if not worktree_ids:
        return labels
    try:
        placeholders = ",".join("?" * len(worktree_ids))
        cursor = conn.cursor()
        cursor.execute(
            f"""
            SELECT w.id, w.slug, r.slug AS repo_slug
            FROM worktrees w
            JOIN repos r ON w.repo_id = r.id
            WHERE w.id IN ({placeholders})
            """,
            worktree_ids,
        )
        for row in cursor.fetchall():
            labels[row[0]] = f"{row[2]}/{row[1]}"
    except sqlite3.OperationalError:
        pass
    return labels


def _batch_repo_labels(
    conn: sqlite3.Connection, runs: list[dict]
) -> dict[str, str]:
    """Return {repo_id: 'repo_slug'} for runs that have no worktree_id."""
    repo_ids = [
        r["repo_id"]
        for r in runs
        if not r.get("worktree_id") and r.get("repo_id")
    ]
    labels: dict[str, str] = {}
    if not repo_ids:
        return labels
    try:
        placeholders = ",".join("?" * len(repo_ids))
        cursor = conn.cursor()
        cursor.execute(
            f"SELECT id, slug FROM repos WHERE id IN ({placeholders})",
            repo_ids,
        )
        for row in cursor.fetchall():
            labels[row[0]] = row[1]
    except sqlite3.OperationalError:
        pass
    return labels


def _batch_gate_steps(
    conn: sqlite3.Connection, runs: list[dict]
) -> dict[str, str]:
    """Return {run_id: step_name} for all waiting_gate runs."""
    run_ids = [r["id"] for r in runs if r["status"] == "waiting_gate"]
    steps: dict[str, str] = {}
    if not run_ids:
        return steps
    try:
        placeholders = ",".join("?" * len(run_ids))
        cursor = conn.cursor()
        cursor.execute(
            f"""
            SELECT workflow_run_id, step_name
            FROM workflow_run_steps
            WHERE workflow_run_id IN ({placeholders})
              AND status = 'waiting_gate'
            """,
            run_ids,
        )
        for row in cursor.fetchall():
            # Keep first gate step per run (there should only be one active)
            if row[0] not in steps:
                steps[row[0]] = row[1]
    except sqlite3.OperationalError:
        pass
    return steps


def format_status_line(runs: list[dict], conn: sqlite3.Connection) -> str:
    """Build the multi-line status output."""
    if not runs:
        return ""

    # Count by status
    counts: dict[str, int] = {}
    for run in runs:
        counts[run["status"]] = counts.get(run["status"], 0) + 1

    # Build summary header
    parts = []
    if counts.get("running", 0):
        parts.append(f"{counts['running']} running")
    if counts.get("waiting_gate", 0):
        n = counts["waiting_gate"]
        parts.append(f"{n} gate{'s' if n > 1 else ''} waiting")
    if counts.get("failed", 0):
        parts.append(f"{counts['failed']} failed")

    if not parts:
        return ""

    header = f"\u29e1 conductor   {' · '.join(parts)}"
    lines = [header]

    # Sort runs: waiting_gate first, then running, failed, completed
    sorted_runs = sorted(
        runs, key=lambda r: (STATUS_ORDER.get(r["status"], 99), r.get("started_at") or "")
    )

    display_runs = sorted_runs[:5]

    # Batch auxiliary lookups to avoid N+1 queries
    worktree_labels = _batch_worktree_labels(conn, display_runs)
    repo_labels = _batch_repo_labels(conn, display_runs)
    gate_steps = _batch_gate_steps(conn, display_runs)

    for run in display_runs:
        status = run["status"]
        icon = ICONS.get(status, "  ")
        wf_name = (run["workflow_name"] or "")[:24]

        # Resolve label: prefer worktree label, fall back to repo label
        wt_id = run.get("worktree_id")
        repo_id = run.get("repo_id")
        if wt_id and wt_id in worktree_labels:
            label = worktree_labels[wt_id]
        elif repo_id and repo_id in repo_labels:
            label = repo_labels[repo_id]
        else:
            label = ""

        elapsed = ""
        if status in ("running", "waiting_gate", "failed"):
            elapsed = format_elapsed(run.get("started_at"))

        gate_step = gate_steps.get(run["id"], "") if status == "waiting_gate" else ""

        # Build detail line
        if status == "waiting_gate" and gate_step:
            detail = f"{icon}  gate:{gate_step:<20}  {wf_name:<24}  {label:<28}  waiting {elapsed}"
        elif status == "failed":
            detail = f"{icon}  {wf_name:<26}                            {label:<28}  failed  {elapsed}"
        else:
            detail = f"{icon}  {wf_name:<26}                            {label:<28}  {status} {elapsed}"

        lines.append(detail.rstrip())

    return "\n".join(lines)


def main() -> None:
    # Read and discard the JSON object from stdin (Claude Code protocol)
    try:
        stdin_data = sys.stdin.read()
        if stdin_data.strip():
            json.loads(stdin_data)
    except (json.JSONDecodeError, OSError):
        pass

    conn = open_db()
    if conn is None:
        return
    try:
        runs = get_runs(conn)
        output = format_status_line(runs, conn)
    finally:
        conn.close()
    if output:
        print(output)


if __name__ == "__main__":
    main()
