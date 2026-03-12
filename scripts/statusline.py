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


def get_runs() -> list[dict]:
    """Query active workflow runs from the conductor DB."""
    if not os.path.exists(DB_PATH):
        return []

    uri = f"file:{DB_PATH}?mode=ro"
    try:
        conn = sqlite3.connect(uri, uri=True, timeout=0.5)
        conn.row_factory = sqlite3.Row
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
        rows = [dict(row) for row in cursor.fetchall()]
        conn.close()
        return rows
    except sqlite3.OperationalError:
        return []


def get_waiting_gate_step(conn: sqlite3.Connection, run_id: str) -> str | None:
    """Return the gate step name for a waiting_gate run."""
    try:
        cursor = conn.cursor()
        cursor.execute(
            """
            SELECT step_name FROM workflow_run_steps
            WHERE workflow_run_id = ? AND status = 'waiting_gate'
            LIMIT 1
            """,
            (run_id,),
        )
        row = cursor.fetchone()
        return row[0] if row else None
    except sqlite3.OperationalError:
        return None


def get_worktree_label(
    conn: sqlite3.Connection, worktree_id: str | None, repo_id: str | None
) -> str:
    """Return a short label like 'umbrella/feat-login' for a worktree."""
    if not worktree_id and not repo_id:
        return ""
    try:
        cursor = conn.cursor()
        if worktree_id:
            cursor.execute(
                """
                SELECT w.slug, r.slug as repo_slug
                FROM worktrees w
                JOIN repos r ON w.repo_id = r.id
                WHERE w.id = ?
                """,
                (worktree_id,),
            )
            row = cursor.fetchone()
            if row:
                return f"{row[1]}/{row[0]}"
        if repo_id:
            cursor.execute("SELECT slug FROM repos WHERE id = ?", (repo_id,))
            row = cursor.fetchone()
            if row:
                return row[0]
    except sqlite3.OperationalError:
        pass
    return ""


def format_status_line(runs: list[dict]) -> str:
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

    # Open a second connection for auxiliary queries (still read-only)
    uri = f"file:{DB_PATH}?mode=ro"
    try:
        conn = sqlite3.connect(uri, uri=True, timeout=0.5)

        for run in sorted_runs[:5]:
            status = run["status"]
            icon = ICONS.get(status, "  ")
            wf_name = (run["workflow_name"] or "")[:24]

            label = get_worktree_label(conn, run.get("worktree_id"), run.get("repo_id"))

            elapsed = ""
            if status in ("running", "waiting_gate"):
                elapsed = format_elapsed(run.get("started_at"))
            elif status == "failed":
                elapsed = format_elapsed(run.get("started_at"))

            gate_step = ""
            if status == "waiting_gate":
                gate_step = get_waiting_gate_step(conn, run["id"]) or ""

            # Build detail line
            if status == "waiting_gate" and gate_step:
                detail = f"{icon}  gate:{gate_step:<20}  {wf_name:<24}  {label:<28}  waiting {elapsed}"
            elif status == "failed":
                detail = f"{icon}  {wf_name:<26}                            {label:<28}  failed  {elapsed}"
            else:
                detail = f"{icon}  {wf_name:<26}                            {label:<28}  {status} {elapsed}"

            lines.append(detail.rstrip())

        conn.close()
    except sqlite3.OperationalError:
        pass

    return "\n".join(lines)


def main() -> None:
    # Read and discard the JSON object from stdin (Claude Code protocol)
    try:
        stdin_data = sys.stdin.read()
        if stdin_data.strip():
            json.loads(stdin_data)
    except (json.JSONDecodeError, OSError):
        pass

    runs = get_runs()
    output = format_status_line(runs)
    if output:
        print(output)


if __name__ == "__main__":
    main()
