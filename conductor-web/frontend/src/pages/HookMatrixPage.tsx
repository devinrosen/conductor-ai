import { useState, useCallback, useRef, useEffect } from "react";
import { Link } from "react-router";
import { useApi } from "../hooks/useApi";
import { api } from "../api/client";
import type { HookSummary } from "../api/types";
import { HookMatrixGrid } from "../components/hooks/HookMatrixGrid";
import { TomlPreviewPanel } from "../components/hooks/TomlPreviewPanel";

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

  function handleCellToggle(hookIndex: number, eventName: string, currentOn: string) {
    if (currentOn === eventName) {
      // Already exact-matched — toggling off is ambiguous without multi-event support.
      // Show nothing for now; the user should use TOML to unset.
      return;
    }
    // Warn if the hook currently has a different exact match (not a wildcard)
    if (currentOn && !currentOn.includes("*") && currentOn !== eventName) {
      const ok = window.confirm(
        `This hook currently fires on "${currentOn}". Changing it to "${eventName}" will replace that assignment. Continue?`,
      );
      if (!ok) return;
    }
    setDraft((prev) => new Map(prev).set(hookIndex, eventName));
    sendPatch(hookIndex, eventName);
  }

  function handleColumnAll(hookIndex: number, currentOn: string) {
    const newOn = currentOn === "*" ? "workflow_run.completed" : "*";
    if (currentOn === "*") {
      const ok = window.confirm(
        `This hook currently fires on all events (*). Replace with "workflow_run.completed"?`,
      );
      if (!ok) return;
    }
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
          className="text-sm text-gray-500 hover:text-gray-700"
        >
          ← Settings
        </Link>
        <span className="text-gray-300">/</span>
        <h2 className="text-xl font-bold text-gray-900">Hook × Event Matrix</h2>
      </div>

      <p className="text-sm text-gray-500">
        Each column is a configured notification hook. Check a cell to set that hook to fire on
        that event. Wildcard badges indicate the hook uses a glob pattern. Changes save immediately.
      </p>

      {patchError && (
        <div className="px-3 py-2 text-sm text-red-700 bg-red-50 rounded-md border border-red-200">
          {patchError}
        </div>
      )}

      {error && (
        <div className="px-3 py-2 text-sm text-red-700 bg-red-50 rounded-md border border-red-200">
          {error}
        </div>
      )}

      {loading ? (
        <div className="text-sm text-gray-400">Loading…</div>
      ) : !hooks || hooks.length === 0 ? (
        <div className="px-4 py-6 text-sm text-gray-600 bg-gray-50 rounded-md border border-gray-200 text-center space-y-2">
          <p>No hooks configured yet.</p>
          <p>
            Edit{" "}
            <code className="text-xs bg-gray-100 px-1 py-0.5 rounded">
              ~/.conductor/config.toml
            </code>{" "}
            to add hooks, then return here to manage their events. See{" "}
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
            onCellToggle={handleCellToggle}
            onColumnAll={handleColumnAll}
          />
          <TomlPreviewPanel hooks={hooks} draft={draft} />
        </>
      )}
    </div>
  );
}
