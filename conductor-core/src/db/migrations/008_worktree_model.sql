-- Migration 007: add model column to worktrees table.
-- Nullable TEXT: stores the per-worktree default model override (e.g. "sonnet", "claude-opus-4-6").
ALTER TABLE worktrees ADD COLUMN model TEXT;
