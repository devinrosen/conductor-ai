const pulseColors: Partial<Record<string, { color: string; pulse: boolean; label: string }>> = {
  running: { color: "bg-yellow-400", pulse: true, label: "Running" },
  waiting_for_feedback: { color: "bg-purple-400", pulse: true, label: "Waiting for feedback" },
  waiting: { color: "bg-gray-300", pulse: false, label: "Waiting" },
};

export const PULSE_STATUSES = new Set(Object.keys(pulseColors));

export function StatusPulseBadge({ status }: { status: string }) {
  const cfg = pulseColors[status];
  if (!cfg) return null;
  return (
    <span
      aria-label={`Status: ${cfg.label}`}
      className={`inline-block w-2 h-2 rounded-full ${cfg.color}${cfg.pulse ? " motion-safe:animate-pulse" : ""}`}
    />
  );
}
