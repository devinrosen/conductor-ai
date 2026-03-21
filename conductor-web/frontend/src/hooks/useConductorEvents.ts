import { useEffect, useRef } from "react";
import { getApiOrigin } from "../api/transport";

/** All SSE event types emitted by the backend. */
export type ConductorEventType =
  | "repo_registered"
  | "repo_unregistered"
  | "worktree_created"
  | "worktree_deleted"
  | "tickets_synced"
  | "agent_started"
  | "agent_stopped"
  | "agent_event"
  | "feedback_requested"
  | "feedback_submitted"
  | "issue_sources_changed"
  | "notification_created"
  | "lagged";

export interface ConductorEventData {
  event: ConductorEventType;
  data?: Record<string, string>;
}

type EventHandler = (data: ConductorEventData) => void;

const ALL_EVENT_TYPES: ConductorEventType[] = [
  "repo_registered",
  "repo_unregistered",
  "worktree_created",
  "worktree_deleted",
  "tickets_synced",
  "agent_started",
  "agent_stopped",
  "agent_event",
  "feedback_requested",
  "feedback_submitted",
  "issue_sources_changed",
  "notification_created",
  "lagged",
];

type Subscriber = {
  handlersRef: React.RefObject<Partial<Record<ConductorEventType, EventHandler>>>;
};

/** Shared singleton state — one EventSource for all hook instances. */
let sharedSource: EventSource | null = null;
let subscribers: Set<Subscriber> = new Set();
let boundListeners: [string, EventListener][] = [];

function dispatch(eventType: ConductorEventType, e: MessageEvent) {
  let parsed: ConductorEventData = { event: eventType };
  try {
    const json = JSON.parse(e.data);
    parsed = { event: eventType, data: json.data ?? json };
  } catch (err) {
    console.warn(`[useConductorEvents] failed to parse SSE data for "${eventType}":`, err);
  }
  for (const sub of subscribers) {
    const handler = sub.handlersRef.current?.[eventType];
    if (handler) handler(parsed);
  }
}

function connectSource(origin: string) {
  const source = new EventSource(`${origin}/api/events`);
  sharedSource = source;

  for (const type of ALL_EVENT_TYPES) {
    const listener = ((e: MessageEvent) => dispatch(type, e)) as EventListener;
    source.addEventListener(type, listener);
    boundListeners.push([type, listener]);
  }

  source.onerror = () => {
    if (source.readyState === EventSource.CLOSED) {
      // Connection permanently closed — tear down and reconnect if there are still subscribers
      closeSharedSource();
      if (subscribers.size > 0) {
        setTimeout(() => openSharedSource(), 3000);
      }
    }
  };
}

function openSharedSource() {
  if (sharedSource && sharedSource.readyState !== EventSource.CLOSED) return;

  // Clean up any dead connection before creating a new one
  if (sharedSource) {
    closeSharedSource();
  }

  // Resolve the API origin (async for desktop mode), then connect.
  getApiOrigin().then((origin) => connectSource(origin));
}

function closeSharedSource() {
  if (!sharedSource) return;
  for (const [type, listener] of boundListeners) {
    sharedSource.removeEventListener(type, listener);
  }
  sharedSource.close();
  sharedSource = null;
  boundListeners = [];
}

/**
 * Subscribe to the backend SSE stream at /api/events.
 *
 * Accepts a map of event types to handler functions. All hook instances share
 * a single EventSource connection (ref-counted). The first caller opens the
 * connection; the last unmount closes it.
 */
export function useConductorEvents(
  handlers: Partial<Record<ConductorEventType, EventHandler>>,
) {
  const handlersRef = useRef(handlers);
  handlersRef.current = handlers;

  useEffect(() => {
    const subscriber: Subscriber = { handlersRef };
    subscribers.add(subscriber);
    openSharedSource();

    return () => {
      subscribers.delete(subscriber);
      if (subscribers.size === 0) {
        closeSharedSource();
      }
    };
  }, []);
}
