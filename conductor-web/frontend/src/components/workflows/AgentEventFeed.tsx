import { useState, useEffect, useCallback } from "react";
import { api } from "../../api/client";
import type { AgentEvent } from "../../api/types";
import { formatDuration } from "../../utils/agentStats";

const KIND_STYLES: Record<string, string> = {
  tool: "text-blue-600 bg-blue-50",
  result: "text-green-600 bg-green-50",
  text: "text-gray-600 bg-gray-50",
  error: "text-red-600 bg-red-50",
  prompt: "text-purple-600 bg-purple-50",
  system: "text-amber-600 bg-amber-50",
};

interface AgentEventFeedProps {
  worktreeId: string;
  agentRunId: string;
  active: boolean;
}

export function AgentEventFeed({ worktreeId, agentRunId, active }: AgentEventFeedProps) {
  const [events, setEvents] = useState<AgentEvent[]>([]);
  const [loading, setLoading] = useState(true);

  const fetchEvents = useCallback(async () => {
    try {
      const data = await api.getRunEvents(worktreeId, agentRunId);
      setEvents(data);
    } catch {
      // silently fail
    } finally {
      setLoading(false);
    }
  }, [worktreeId, agentRunId]);

  useEffect(() => {
    fetchEvents();
  }, [fetchEvents]);

  useEffect(() => {
    if (!active) return;
    const interval = setInterval(fetchEvents, 3000);
    return () => clearInterval(interval);
  }, [active, fetchEvents]);

  if (loading && events.length === 0) {
    return <p className="text-xs text-gray-400 py-2">Loading events...</p>;
  }

  if (events.length === 0) {
    return (
      <p className="text-xs text-gray-400 py-2">
        {active ? "Agent running \u2014 waiting for events..." : "No agent events"}
      </p>
    );
  }

  return (
    <div className="space-y-1 max-h-80 overflow-y-auto">
      {events.map((evt) => {
        const style = KIND_STYLES[evt.kind] ?? KIND_STYLES.text;
        return (
          <div key={evt.id} className="flex items-start gap-2 text-xs py-1">
            <span className={`shrink-0 px-1.5 py-0.5 rounded font-mono text-[10px] ${style}`}>
              {evt.kind}
            </span>
            <span className="text-gray-700 break-words min-w-0 flex-1">
              {evt.summary}
            </span>
            {evt.duration_ms != null && evt.duration_ms > 0 && (
              <span className="shrink-0 text-gray-400 font-mono tabular-nums">
                {formatDuration(evt.duration_ms)}
              </span>
            )}
          </div>
        );
      })}
    </div>
  );
}
