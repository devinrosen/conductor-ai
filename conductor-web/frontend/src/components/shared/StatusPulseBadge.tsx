const pulseColors: Partial<Record<string, { color: string; pulse: boolean }>> = {
  running: { color: "bg-yellow-400", pulse: true },
  waiting_for_feedback: { color: "bg-purple-400", pulse: true },
  waiting: { color: "bg-gray-300", pulse: false },
};

export function StatusPulseBadge({ status }: { status: string }) {
  const cfg = pulseColors[status];
  if (!cfg) return null;
  return (
    <span className={`inline-block w-2 h-2 rounded-full ${cfg.color}${cfg.pulse ? " animate-pulse" : ""}`} />
  );
}
