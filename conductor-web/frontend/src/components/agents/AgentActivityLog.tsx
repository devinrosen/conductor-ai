import { useEffect, useMemo, useRef } from "react";
import type { AgentEvent, AgentRun } from "../../api/types";

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
  prompt: {
    label: "YOU",
    badge: "bg-blue-900 text-blue-300",
    text: "text-blue-300",
    border: "border-l-blue-500",
  },
};

/**
 * Extract a clean step label from an orchestrator child prompt.
 * Returns "Step N/M: description" or null if not a child prompt.
 */
function extractStepLabel(prompt: string): string | null {
  const match = prompt.match(
    /^You are executing step (\d+) of (\d+) in a multi-step plan\./,
  );
  if (!match) return null;
  const [, stepNum, total] = match;

  const assignmentIdx = prompt.indexOf("## Your Assignment");
  if (assignmentIdx !== -1) {
    const afterHeader = prompt.slice(assignmentIdx);
    const nlIdx = afterHeader.indexOf("\n");
    if (nlIdx !== -1) {
      const desc = afterHeader
        .slice(nlIdx + 1)
        .trim()
        .split("\n")
        .filter((l) => !l.startsWith("Focus only on this step"))
        .join(" ")
        .trim();
      if (desc) {
        const truncated = desc.length > 80 ? desc.slice(0, 80) + "..." : desc;
        return `Step ${stepNum}/${total}: ${truncated}`;
      }
    }
  }

  return `Step ${stepNum}/${total}`;
}

const defaultConfig = {
  label: "???",
  badge: "bg-gray-700 text-gray-400",
  text: "text-gray-400",
  border: "border-l-gray-600",
};

interface AgentActivityLogProps {
  events: AgentEvent[];
  runs: AgentRun[];
  isRunning: boolean;
}

export function AgentActivityLog({ events, runs, isRunning }: AgentActivityLogProps) {
  const scrollRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, [events.length]);

  // Build run_id -> { runNumber, model, startedAt } lookup
  const runInfo = useMemo(() => {
    const sorted = [...runs].sort(
      (a, b) => a.started_at.localeCompare(b.started_at),
    );
    const map = new Map<string, { runNumber: number; model: string | null; startedAt: string }>();
    sorted.forEach((r, i) => {
      map.set(r.id, { runNumber: i + 1, model: r.model, startedAt: r.started_at });
    });
    return map;
  }, [runs]);

  const hasMultipleRuns = runs.length > 1;

  if (events.length === 0) {
    return (
      <div className="rounded-lg border border-gray-200 bg-gray-50 p-4 text-center text-sm text-gray-500">
        {isRunning
          ? "Waiting for agent activity..."
          : "No agent activity to display"}
      </div>
    );
  }

  // Build elements with run boundary separators
  const elements: React.ReactNode[] = [];
  let prevRunId: string | null = null;

  events.forEach((event, i) => {
    // Insert run boundary separator when run_id changes
    if (hasMultipleRuns && event.run_id !== prevRunId) {
      const info = runInfo.get(event.run_id);
      if (info) {
        const ts = info.startedAt.slice(0, 16).replace("T", " ");
        const model = info.model ?? "default";
        elements.push(
          <div
            key={`sep-${event.run_id}`}
            className={`flex items-center gap-2 px-2 py-1.5 ${prevRunId !== null ? "mt-3 border-t border-gray-800 pt-2" : ""}`}
          >
            <span className="text-[10px] text-gray-500 font-medium tracking-wider">
              ── Run {info.runNumber} &nbsp; {ts} &nbsp; {model} ──
            </span>
          </div>,
        );
      }
    }
    prevRunId = event.run_id;

    const cfg = kindConfig[event.kind] ?? defaultConfig;
    const prevKind = i > 0 ? events[i - 1].kind : null;
    const showGap =
      prevKind !== null && prevKind !== event.kind && events[i - 1].run_id === event.run_id;

    const stepLabel = event.kind === "prompt" ? extractStepLabel(event.summary) : null;
    const isFeedback = event.summary.startsWith("[NEEDS_FEEDBACK]");
    const displayLabel = stepLabel ? "STEP" : isFeedback ? "WAIT" : cfg.label;
    const displayText = stepLabel ?? event.summary;
    const effectiveCfg = stepLabel
      ? { badge: "bg-fuchsia-900 text-fuchsia-300", text: "text-fuchsia-300", border: "border-l-fuchsia-500" }
      : isFeedback
        ? { badge: "bg-purple-900 text-purple-300", text: "text-purple-300", border: "border-l-purple-500" }
        : cfg;

    elements.push(
      <div
        key={i}
        className={`flex items-start gap-2 px-2 py-1 border-l-2 ${effectiveCfg.border} ${showGap ? "mt-2" : ""}`}
      >
        <span
          className={`shrink-0 inline-block w-12 text-center text-[10px] font-semibold rounded px-1 py-0.5 leading-tight ${effectiveCfg.badge}`}
        >
          {displayLabel}
        </span>
        <span className={`${effectiveCfg.text} leading-snug break-words min-w-0 flex-1`}>
          {displayText}
        </span>
        {event.duration_ms != null && event.duration_ms >= 100 && (
          <span className="shrink-0 text-[10px] text-gray-500 tabular-nums">
            {event.duration_ms >= 1000
              ? `${(event.duration_ms / 1000).toFixed(1)}s`
              : `${event.duration_ms}ms`}
          </span>
        )}
      </div>,
    );
  });

  return (
    <div
      ref={scrollRef}
      className="rounded-lg border border-gray-200 bg-gray-950 p-2 max-h-[28rem] overflow-y-auto font-mono text-sm"
    >
      {elements}
      {isRunning && (
        <div className="text-yellow-400 animate-pulse mt-2 px-2 py-1 text-xs">
          Agent is working...
        </div>
      )}
    </div>
  );
}
