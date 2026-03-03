import type { PlanStep } from "../../api/types";

interface AgentPlanChecklistProps {
  steps: PlanStep[];
}

export function AgentPlanChecklist({ steps }: AgentPlanChecklistProps) {
  const doneCount = steps.filter((s) => s.done).length;
  const allDone = doneCount === steps.length;

  return (
    <div className="rounded-lg border border-gray-200 bg-white p-4">
      <div className="flex items-center justify-between mb-3">
        <h4 className="text-sm font-semibold uppercase tracking-wider text-gray-400">
          Plan
        </h4>
        <span className="text-xs text-gray-400">
          {doneCount}/{steps.length} completed
        </span>
      </div>
      <ul className="space-y-1.5">
        {steps.map((step, i) => (
          <li key={i} className="flex items-start gap-2 text-sm">
            <span
              className={`mt-0.5 flex-shrink-0 w-4 h-4 rounded border flex items-center justify-center text-xs ${
                step.done
                  ? "bg-green-100 border-green-400 text-green-600"
                  : "border-gray-300 text-transparent"
              }`}
            >
              {step.done && "\u2713"}
            </span>
            <span
              className={
                step.done ? "text-gray-400 line-through" : "text-gray-900"
              }
            >
              {step.description}
            </span>
          </li>
        ))}
      </ul>
      {allDone && (
        <p className="mt-3 text-xs text-green-600">All steps completed</p>
      )}
    </div>
  );
}
