#!/usr/bin/env python3
"""Tests for statusline.py logic."""

import sqlite3
import tempfile
import os
import sys
import unittest
from pathlib import Path

# Allow importing statusline from the same directory
sys.path.insert(0, str(Path(__file__).parent))

from statusline import (
    format_elapsed,
    format_status_line,
    get_runs,
    _batch_gate_steps,
)


def make_db() -> sqlite3.Connection:
    """Create an in-memory SQLite DB with the minimal conductor schema."""
    conn = sqlite3.connect(":memory:")
    conn.row_factory = sqlite3.Row
    conn.executescript(
        """
        CREATE TABLE repos (
            id TEXT PRIMARY KEY,
            slug TEXT NOT NULL
        );
        CREATE TABLE worktrees (
            id TEXT PRIMARY KEY,
            repo_id TEXT NOT NULL REFERENCES repos(id),
            slug TEXT NOT NULL
        );
        CREATE TABLE workflow_runs (
            id TEXT PRIMARY KEY,
            workflow_name TEXT,
            status TEXT NOT NULL,
            started_at TEXT,
            ended_at TEXT,
            worktree_id TEXT,
            repo_id TEXT,
            parent_workflow_run_id TEXT,
            target_label TEXT
        );
        CREATE TABLE workflow_run_steps (
            id TEXT PRIMARY KEY,
            workflow_run_id TEXT NOT NULL REFERENCES workflow_runs(id),
            step_name TEXT NOT NULL,
            status TEXT NOT NULL
        );
        """
    )
    return conn


class TestFormatElapsed(unittest.TestCase):
    def test_seconds(self):
        from datetime import datetime, timezone, timedelta
        started = (datetime.now(tz=timezone.utc) - timedelta(seconds=45)).isoformat()
        result = format_elapsed(started)
        self.assertEqual(result, "45s")

    def test_minutes(self):
        from datetime import datetime, timezone, timedelta
        started = (datetime.now(tz=timezone.utc) - timedelta(minutes=3, seconds=7)).isoformat()
        result = format_elapsed(started)
        self.assertEqual(result, "3m 7s")

    def test_hours(self):
        from datetime import datetime, timezone, timedelta
        started = (datetime.now(tz=timezone.utc) - timedelta(hours=2, minutes=15)).isoformat()
        result = format_elapsed(started)
        self.assertEqual(result, "2h 15m")

    def test_none_returns_empty(self):
        self.assertEqual(format_elapsed(None), "")

    def test_invalid_returns_empty(self):
        self.assertEqual(format_elapsed("not-a-date"), "")

    def test_z_suffix_handled(self):
        from datetime import datetime, timezone, timedelta
        started = (datetime.now(tz=timezone.utc) - timedelta(seconds=10)).strftime("%Y-%m-%dT%H:%M:%SZ")
        result = format_elapsed(started)
        self.assertEqual(result, "10s")


class TestGetRuns(unittest.TestCase):
    def test_returns_active_runs(self):
        conn = make_db()
        conn.execute(
            "INSERT INTO workflow_runs VALUES (?,?,?,?,?,?,?,?,?)",
            ("r1", "deploy", "running", "2024-01-01T00:00:00", None, None, None, None, None),
        )
        conn.execute(
            "INSERT INTO workflow_runs VALUES (?,?,?,?,?,?,?,?,?)",
            ("r2", "release", "waiting", "2024-01-01T01:00:00", None, None, None, None, None),
        )
        conn.commit()
        runs = get_runs(conn)
        ids = {r["id"] for r in runs}
        self.assertIn("r1", ids)
        self.assertIn("r2", ids)

    def test_excludes_child_runs(self):
        conn = make_db()
        conn.execute(
            "INSERT INTO workflow_runs VALUES (?,?,?,?,?,?,?,?,?)",
            ("parent", "wf", "running", "2024-01-01T00:00:00", None, None, None, None, None),
        )
        conn.execute(
            "INSERT INTO workflow_runs VALUES (?,?,?,?,?,?,?,?,?)",
            ("child", "wf-child", "running", "2024-01-01T00:00:00", None, None, None, "parent", None),
        )
        conn.commit()
        runs = get_runs(conn)
        ids = {r["id"] for r in runs}
        self.assertIn("parent", ids)
        self.assertNotIn("child", ids)

    def test_active_runs_prioritized_over_completed_in_limit(self):
        """Active runs must appear before completed runs within the LIMIT."""
        conn = make_db()
        # Insert 20 completed runs with recent timestamps
        from datetime import datetime, timezone, timedelta
        now = datetime.now(tz=timezone.utc)
        for i in range(20):
            ts = (now - timedelta(seconds=i)).isoformat()
            conn.execute(
                "INSERT INTO workflow_runs VALUES (?,?,?,?,?,?,?,?,?)",
                (f"completed_{i}", "wf", "completed", ts, ts, None, None, None, None),
            )
        # Insert one active run with an old timestamp
        old_ts = (now - timedelta(hours=5)).isoformat()
        conn.execute(
            "INSERT INTO workflow_runs VALUES (?,?,?,?,?,?,?,?,?)",
            ("active_old", "wf", "running", old_ts, None, None, None, None, None),
        )
        conn.commit()
        runs = get_runs(conn)
        ids = {r["id"] for r in runs}
        self.assertIn("active_old", ids, "Active run must appear even when 20 completed runs exist")

    def test_target_label_returned_from_db(self):
        """target_label column is fetched and returned by get_runs."""
        conn = make_db()
        conn.execute(
            "INSERT INTO workflow_runs VALUES (?,?,?,?,?,?,?,?,?)",
            ("r1", "deploy", "running", "2024-01-01T00:00:00", None, None, None, None, "my-repo/feat-x"),
        )
        conn.commit()
        runs = get_runs(conn)
        self.assertEqual(len(runs), 1)
        self.assertEqual(runs[0]["target_label"], "my-repo/feat-x")


class TestBatchGateSteps(unittest.TestCase):
    def test_returns_waiting_step_name(self):
        conn = make_db()
        conn.execute(
            "INSERT INTO workflow_runs VALUES (?,?,?,?,?,?,?,?,?)",
            ("run1", "wf", "waiting", "2024-01-01T00:00:00", None, None, None, None, None),
        )
        conn.execute(
            "INSERT INTO workflow_run_steps VALUES (?,?,?,?)",
            ("step1", "run1", "approve-pr", "waiting"),
        )
        conn.commit()
        runs = [{"id": "run1", "status": "waiting"}]
        steps = _batch_gate_steps(conn, runs)
        self.assertEqual(steps["run1"], "approve-pr")

    def test_ignores_non_waiting_runs(self):
        conn = make_db()
        runs = [{"id": "run1", "status": "running"}]
        steps = _batch_gate_steps(conn, runs)
        self.assertEqual(steps, {})


class TestFormatStatusLine(unittest.TestCase):
    def test_empty_when_no_runs(self):
        conn = make_db()
        result = format_status_line([], conn)
        self.assertEqual(result, "")

    def test_empty_when_only_completed_runs(self):
        conn = make_db()
        runs = [{"id": "r1", "status": "completed", "workflow_name": "wf",
                 "started_at": "2024-01-01T00:00:00", "ended_at": "2024-01-01T01:00:00",
                 "worktree_id": None, "repo_id": None, "target_label": None}]
        result = format_status_line(runs, conn)
        self.assertEqual(result, "")

    def test_running_run_appears_in_output(self):
        conn = make_db()
        runs = [{"id": "r1", "status": "running", "workflow_name": "deploy",
                 "started_at": "2024-01-01T00:00:00", "ended_at": None,
                 "worktree_id": None, "repo_id": None, "target_label": None}]
        result = format_status_line(runs, conn)
        self.assertIn("1 running", result)
        self.assertIn("deploy", result)

    def test_waiting_run_shows_gate_waiting(self):
        conn = make_db()
        conn.execute(
            "INSERT INTO workflow_runs VALUES (?,?,?,?,?,?,?,?,?)",
            ("run1", "release", "waiting", "2024-01-01T00:00:00", None, None, None, None, None),
        )
        conn.execute(
            "INSERT INTO workflow_run_steps VALUES (?,?,?,?)",
            ("step1", "run1", "approve-deploy", "waiting"),
        )
        conn.commit()
        runs = [{"id": "run1", "status": "waiting", "workflow_name": "release",
                 "started_at": "2024-01-01T00:00:00", "ended_at": None,
                 "worktree_id": None, "repo_id": None, "target_label": None}]
        result = format_status_line(runs, conn)
        self.assertIn("1 gate waiting", result)
        self.assertIn("approve-deploy", result)

    def test_failed_run_appears_in_output(self):
        conn = make_db()
        runs = [{"id": "r1", "status": "failed", "workflow_name": "ci",
                 "started_at": "2024-01-01T00:00:00", "ended_at": "2024-01-01T01:00:00",
                 "worktree_id": None, "repo_id": None, "target_label": None}]
        result = format_status_line(runs, conn)
        self.assertIn("1 failed", result)

    def test_target_label_shown_in_output(self):
        conn = make_db()
        runs = [{"id": "r1", "status": "running", "workflow_name": "wf",
                 "started_at": "2024-01-01T00:00:00", "ended_at": None,
                 "worktree_id": None, "repo_id": None, "target_label": "myrepo/feat-x"}]
        result = format_status_line(runs, conn)
        self.assertIn("myrepo/feat-x", result)

    def test_multiple_gates_plural(self):
        conn = make_db()
        runs = [
            {"id": "r1", "status": "waiting", "workflow_name": "wf1",
             "started_at": "2024-01-01T00:00:00", "ended_at": None,
             "worktree_id": None, "repo_id": None, "target_label": None},
            {"id": "r2", "status": "waiting", "workflow_name": "wf2",
             "started_at": "2024-01-01T00:00:00", "ended_at": None,
             "worktree_id": None, "repo_id": None, "target_label": None},
        ]
        result = format_status_line(runs, conn)
        self.assertIn("2 gates waiting", result)

    def test_header_contains_conductor_symbol(self):
        conn = make_db()
        runs = [{"id": "r1", "status": "running", "workflow_name": "wf",
                 "started_at": "2024-01-01T00:00:00", "ended_at": None,
                 "worktree_id": None, "repo_id": None, "target_label": None}]
        result = format_status_line(runs, conn)
        self.assertIn("conductor", result)


if __name__ == "__main__":
    unittest.main()
