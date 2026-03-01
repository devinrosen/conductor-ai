import { useEffect, useRef, useCallback } from "react";

/** All SSE event types emitted by the backend. */
export type ConductorEventType =
  | "repo_created"
  | "repo_deleted"
  | "worktree_created"
  | "worktree_deleted"
  | "tickets_synced"
  | "agent_started"
  | "agent_stopped"
  | "agent_event"
  | "work_targets_changed"
  | "issue_sources_changed"
  | "lagged";

export interface ConductorEventData {
  event: ConductorEventType;
  data?: Record<string, string>;
}

type EventHandler = (data: ConductorEventData) => void;

/**
 * Subscribe to the backend SSE stream at /api/events.
 *
 * Accepts a map of event types to handler functions. The hook manages a single
 * shared EventSource connection per mount, reconnecting automatically on error.
 */
export function useConductorEvents(
  handlers: Partial<Record<ConductorEventType, EventHandler>>,
) {
  const handlersRef = useRef(handlers);
  handlersRef.current = handlers;

  const makeHandler = useCallback(
    (eventType: ConductorEventType) => (e: MessageEvent) => {
      const handler = handlersRef.current[eventType];
      if (!handler) return;
      try {
        const parsed = JSON.parse(e.data);
        handler({ event: eventType, data: parsed.data ?? parsed });
      } catch {
        handler({ event: eventType });
      }
    },
    [],
  );

  useEffect(() => {
    const source = new EventSource("/api/events");

    const eventTypes: ConductorEventType[] = [
      "repo_created",
      "repo_deleted",
      "worktree_created",
      "worktree_deleted",
      "tickets_synced",
      "agent_started",
      "agent_stopped",
      "agent_event",
      "work_targets_changed",
      "issue_sources_changed",
      "lagged",
    ];

    const boundHandlers: [string, (e: MessageEvent) => void][] = [];

    for (const type of eventTypes) {
      const handler = makeHandler(type);
      source.addEventListener(type, handler as EventListener);
      boundHandlers.push([type, handler]);
    }

    return () => {
      for (const [type, handler] of boundHandlers) {
        source.removeEventListener(type, handler as EventListener);
      }
      source.close();
    };
  }, [makeHandler]);
}
