import { useState, useCallback, useRef, useEffect } from "react";
import { Link } from "react-router";
import { useApi } from "../hooks/useApi";
import { api } from "../api/client";
import type { HookSummary } from "../api/types";
import { HookMatrixGrid } from "../components/hooks/HookMatrixGrid";
import { TomlPreviewPanel } from "../components/hooks/TomlPreviewPanel";
import type { CellMode } from "../components/hooks/CellToggle";

export function HookMatrixPage() {
  const {
    data: fetchedHooks,
    loading: hooksLoading,
    error: hooksError,
  } = useApi(() => api.listHooks(), []);

  // Local mirror of hooks — updated optimistically on each PATCH response.
  const [hooks, setHooks] = useState<HookSummary[] | null>(null);
  useEffect(() => {
    if (fetchedHooks !== null) setHooks(fetchedHooks);
  }, [fetchedHooks]);

  const {
    data: events,
    loading: eventsLoading,
    error: eventsError,
  } = useApi(() => api.listHookEvents(), []);

  // Pending on-pattern overrides: hookIndex → new on value
  const [draft, setDraft] = useState<Map<number, string>>(new Map());
  // Set of hook indices that have an in-flight PATCH request
  const [inFlight, setInFlight] = useState<Set<number>>(new Set());
  const [patchError, setPatchError] = useState<string | null>(null);

  // Track in-flight count for beforeunload guard
  const inFlightRef = useRef(inFlight);
  useEffect(() => {
    inFlightRef.current = inFlight;
  }, [inFlight]);

  useEffect(() => {
    function handleBeforeUnload(e: BeforeUnloadEvent) {
      if (inFlightRef.current.size > 0) {
        e.preventDefault();
      }
    }
    window.addEventListener("beforeunload", handleBeforeUnload);
    return () => window.removeEventListener("beforeunload", handleBeforeUnload);
  }, []);

  const sendPatch = useCallback(async (hookIndex: number, newOn: string) => {
    setInFlight((prev) => new Set(prev).add(hookIndex));
    setPatchError(null);
    try {
      const updated = await api.patchHookOn(hookIndex, newOn);
      setDraft((prev) => {
        const next = new Map(prev);
        next.delete(hookIndex);
        return next;
      });
      // Update in-place using the server's response — no extra GET needed.
      setHooks((prev) =>
        prev ? prev.map((h) => (h.index === hookIndex ? updated : h)) : prev,
      );
    } catch (err) {
      setPatchError(err instanceof Error ? err.message : "Failed to update hook");
    } finally {
      setInFlight((prev) => {
        const next = new Set(prev);
        next.delete(hookIndex);
        return next;
      });
    }
  }, []);

  /**
   * Handle a cell mode change in the matrix.
   *
   * Computes the new comma-separated `on` pattern by removing old entries
   * for this event (`event`, `event:root`) and adding the new one based on mode.
   */
  function handleCellChange(
    hookIndex: number,
    eventName: string,
    mode: CellMode,
    currentOn: string,
  ) {
    const parts = currentOn
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean);

    // Remove any existing entries for this event (plain or :root)
    const filtered = parts.filter(
      (p) => p !== eventName && p !== `${eventName}:root`,
    );

    // Add the new entry based on mode
    if (mode === "any") {
      filtered.push(eventName);
    } else if (mode === "root") {
      filtered.push(`${eventName}:root`);
    }
    // mode === "off" — just leave it removed

    const newOn = filtered.join(",");
    setDraft((prev) => new Map(prev).set(hookIndex, newOn));
    sendPatch(hookIndex, newOn);
  }

  function handleColumnAll(hookIndex: number, currentOn: string) {
    const newOn = currentOn === "*" ? "" : "*";
    setDraft((prev) => new Map(prev).set(hookIndex, newOn));
    sendPatch(hookIndex, newOn);
  }

  const loading = hooksLoading || eventsLoading;
  const error = hooksError || eventsError;

  return (
    <div className="space-y-6">
      <div className="flex items-center gap-2">
        <Link
          to="/settings"
          className="text-sm text-gray-500 hover:text-gray-700 dark:text-gray-400 dark:hover:text-gray-200"
        >
          ← Settings
        </Link>
        <span className="text-gray-300 dark:text-gray-600">/</span>
        <h2 className="text-xl font-bold text-gray-900 dark:text-gray-100">Hook × Event Matrix</h2>
      </div>

      <p className="text-sm text-gray-500 dark:text-gray-400">
        Each column is a notification hook. For workflow events, choose{" "}
        <span className="font-semibold text-blue-600">Any</span> (all workflows) or{" "}
        <span className="font-semibold text-amber-500">Root</span> (root workflows only).
        Changes save immediately.
      </p>

      {patchError && (
        <div className="px-3 py-2 text-sm text-red-700 bg-red-50 dark:bg-red-900/30 dark:text-red-300 rounded-md border border-red-200 dark:border-red-800">
          {patchError}
        </div>
      )}

      {error && (
        <div className="px-3 py-2 text-sm text-red-700 bg-red-50 dark:bg-red-900/30 dark:text-red-300 rounded-md border border-red-200 dark:border-red-800">
          {error}
        </div>
      )}

      {loading ? (
        <div className="text-sm text-gray-400">Loading…</div>
      ) : !hooks || hooks.length === 0 ? (
        <div className="px-4 py-6 text-sm text-gray-600 dark:text-gray-400 bg-gray-50 dark:bg-gray-800 rounded-md border border-gray-200 dark:border-gray-700 text-center space-y-2">
          <p>No hooks configured yet.</p>
          <p>
            Add hook scripts to{" "}
            <code className="text-xs bg-gray-100 dark:bg-gray-700 px-1 py-0.5 rounded">
              ~/.conductor/hooks/
            </code>{" "}
            or edit{" "}
            <code className="text-xs bg-gray-100 dark:bg-gray-700 px-1 py-0.5 rounded">
              ~/.conductor/config.toml
            </code>{" "}
            to add hooks. See{" "}
            <a
              href="https://github.com/devinrosen/conductor-ai/tree/main/docs/examples/hooks"
              target="_blank"
              rel="noreferrer"
              className="text-blue-600 hover:underline"
            >
              example scripts
            </a>{" "}
            to get started.
          </p>
        </div>
      ) : (
        <>
          <HookMatrixGrid
            hooks={hooks}
            events={events ?? []}
            draft={draft}
            inFlight={inFlight}
            onCellChange={handleCellChange}
            onColumnAll={handleColumnAll}
          />
          <TomlPreviewPanel hooks={hooks} draft={draft} />
        </>
      )}
    </div>
  );
}
