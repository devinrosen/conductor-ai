/**
 * Train-on-track progress indicator for workflow steps.
 *
 * Shows steps as stations along a horizontal track with a train icon
 * at the currently active step. Completed stations are green, pending
 * are gray, failed are red.
 */

interface StepInfo {
  name: string;
  status: "pending" | "running" | "completed" | "failed" | "skipped" | "waiting";
}

const statusColor: Record<string, string> = {
  completed: "#39B54A",
  running: "#2B5EA7",
  waiting: "#FF9500",
  failed: "#D73020",
  skipped: "#4A5568",
  pending: "#4A5568",
};

const trackColor = "#232D42";

/** Strip common prefixes like "workflow:" from step names for display */
function displayName(name: string): string {
  return name.replace(/^workflow:/, "");
}

export function TrainProgress({ steps }: { steps: StepInfo[] }) {
  if (steps.length === 0) return null;

  const activeIndex = steps.findIndex(
    (s) => s.status === "running" || s.status === "waiting",
  );

  return (
    <div className="overflow-x-auto py-3">
      <div className="flex items-start min-w-fit">
        {steps.map((step, i) => {
          const color = statusColor[step.status] ?? statusColor.pending;
          const isActive = i === activeIndex;
          const trackDone = step.status === "completed" || step.status === "skipped";

          return (
            <div key={i} className="flex flex-col items-center" style={{ minWidth: 0 }}>
              {/* Track + dot row */}
              <div className="flex items-center w-full">
                {/* Left track segment */}
                <div
                  className="h-0.5 flex-1"
                  style={{
                    backgroundColor: i === 0 ? "transparent" : trackDone ? statusColor.completed : trackColor,
                  }}
                />
                {/* Station dot */}
                <div className="relative shrink-0">
                  {isActive && (
                    <span className="absolute -top-5 left-1/2 -translate-x-1/2 text-sm" aria-label="Current step">
                      🚂
                    </span>
                  )}
                  <div
                    className="w-3.5 h-3.5 rounded-full border-2 shrink-0"
                    style={{
                      borderColor: color,
                      backgroundColor: step.status === "completed" ? color : "transparent",
                    }}
                  />
                </div>
                {/* Right track segment */}
                <div
                  className="h-0.5 flex-1"
                  style={{
                    backgroundColor: i === steps.length - 1 ? "transparent" : (
                      (steps[i + 1]?.status === "completed" || steps[i + 1]?.status === "skipped")
                        ? statusColor.completed
                        : trackColor
                    ),
                  }}
                />
              </div>
              {/* Label */}
              <span
                className="text-[10px] max-w-24 sm:max-w-32 text-center leading-tight break-words mt-1 px-1"
                style={{ color }}
                title={step.name}
              >
                {displayName(step.name)}
              </span>
            </div>
          );
        })}
      </div>
    </div>
  );
}
