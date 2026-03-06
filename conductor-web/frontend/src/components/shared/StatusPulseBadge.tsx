const pulseColors: Partial<Record<string, string>> = {
  running: "bg-yellow-400",
  waiting_for_feedback: "bg-purple-400",
};

export function StatusPulseBadge({ status }: { status: string }) {
  const color = pulseColors[status];
  if (!color) return null;
  return (
    <span className={`inline-block w-2 h-2 rounded-full ${color} animate-pulse`} />
  );
}
