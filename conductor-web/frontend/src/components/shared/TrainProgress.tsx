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

export function TrainProgress({ steps }: { steps: StepInfo[] }) {
  if (steps.length === 0) return null;

  const activeIndex = steps.findIndex(
    (s) => s.status === "running" || s.status === "waiting",
  );

  return (
    <div className="overflow-x-auto py-3">
      <div className="flex items-center min-w-fit gap-0">
        {steps.map((step, i) => {
          const color = statusColor[step.status] ?? statusColor.pending;
          const isActive = i === activeIndex;

          return (
            <div key={i} className="flex items-center">
              {/* Track segment before station (except first) */}
              {i > 0 && (
                <div
                  className="h-0.5 w-8 sm:w-12"
                  style={{
                    backgroundColor:
                      step.status === "completed" || step.status === "skipped"
                        ? statusColor.completed
                        : trackColor,
                  }}
                />
              )}

              {/* Station */}
              <div className="flex flex-col items-center gap-1 relative">
                {/* Train icon above active station */}
                {isActive && (
                  <span className="absolute -top-5 text-sm" aria-label="Current step">
                    🚂
                  </span>
                )}
                <div
                  className="w-3.5 h-3.5 rounded-full border-2 shrink-0"
                  style={{
                    borderColor: color,
                    backgroundColor:
                      step.status === "completed" ? color : "transparent",
                  }}
                />
                <span
                  className="text-[10px] max-w-16 truncate text-center"
                  style={{ color }}
                  title={step.name}
                >
                  {step.name}
                </span>
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}
