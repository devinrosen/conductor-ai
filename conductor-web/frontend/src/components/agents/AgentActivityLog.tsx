import { useEffect, useRef } from "react";
import type { AgentEvent } from "../../api/types";

const kindConfig: Record<
  string,
  { label: string; badge: string; text: string; border: string }
> = {
  text: {
    label: "TEXT",
    badge: "bg-gray-600 text-gray-200",
    text: "text-gray-300",
    border: "border-l-gray-500",
  },
  tool: {
    label: "TOOL",
    badge: "bg-yellow-900 text-yellow-300",
    text: "text-yellow-200",
    border: "border-l-yellow-500",
  },
  result: {
    label: "DONE",
    badge: "bg-green-900 text-green-300",
    text: "text-green-300",
    border: "border-l-green-500",
  },
  system: {
    label: "SYS",
    badge: "bg-gray-700 text-gray-400",
    text: "text-gray-500",
    border: "border-l-gray-600",
  },
  error: {
    label: "ERR",
    badge: "bg-red-900 text-red-300",
    text: "text-red-400",
    border: "border-l-red-500",
  },
};

const defaultConfig = {
  label: "???",
  badge: "bg-gray-700 text-gray-400",
  text: "text-gray-400",
  border: "border-l-gray-600",
};

interface AgentActivityLogProps {
  events: AgentEvent[];
  isRunning: boolean;
}

export function AgentActivityLog({ events, isRunning }: AgentActivityLogProps) {
  const scrollRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, [events.length]);

  if (events.length === 0) {
    return (
      <div className="rounded-lg border border-gray-200 bg-gray-50 p-4 text-center text-sm text-gray-500">
        {isRunning
          ? "Waiting for agent activity..."
          : "No agent activity to display"}
      </div>
    );
  }

  return (
    <div
      ref={scrollRef}
      className="rounded-lg border border-gray-200 bg-gray-950 p-2 max-h-[28rem] overflow-y-auto font-mono text-sm"
    >
      {events.map((event, i) => {
        const cfg = kindConfig[event.kind] ?? defaultConfig;
        const prevKind = i > 0 ? events[i - 1].kind : null;
        const showGap = prevKind !== null && prevKind !== event.kind;

        return (
          <div
            key={i}
            className={`flex items-start gap-2 px-2 py-1 border-l-2 ${cfg.border} ${showGap ? "mt-2" : ""}`}
          >
            <span
              className={`shrink-0 inline-block w-12 text-center text-[10px] font-semibold rounded px-1 py-0.5 leading-tight ${cfg.badge}`}
            >
              {cfg.label}
            </span>
            <span className={`${cfg.text} leading-snug break-words min-w-0`}>
              {event.summary}
            </span>
          </div>
        );
      })}
      {isRunning && (
        <div className="text-yellow-400 animate-pulse mt-2 px-2 py-1 text-xs">
          Agent is working...
        </div>
      )}
    </div>
  );
}
