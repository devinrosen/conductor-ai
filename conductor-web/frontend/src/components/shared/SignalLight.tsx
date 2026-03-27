/**
 * Three-aspect railway signal light.
 *
 * Shows a vertical stack of three dots (red/amber/green) with the
 * active one lit and the others dimmed. Maps status strings to
 * which light is active.
 */

const statusToLight: Record<string, "green" | "amber" | "red"> = {
  active: "green",
  running: "green",
  completed: "green",
  open: "green",
  merged: "green",
  idle: "amber",
  pending: "amber",
  waiting: "amber",
  waiting_for_feedback: "amber",
  failed: "red",
  stopped: "red",
  cancelled: "red",
  abandoned: "red",
  closed: "red",
};

// Signal housing is always dark metal — intentionally hardcoded, not theme-variable
const colors = {
  green: { on: "#39B54A", glow: "rgba(57, 181, 74, 0.4)" },
  amber: { on: "#FF9500", glow: "rgba(255, 149, 0, 0.4)" },
  red: { on: "#D73020", glow: "rgba(215, 48, 32, 0.4)" },
  off: "#1a2236",
};

export function SignalLight({ status, size = 14 }: { status: string; size?: number }) {
  const active = statusToLight[status] ?? "amber";
  const dotSize = Math.round(size / 3.5);
  const gap = Math.round(size / 14);

  return (
    <div
      className="inline-flex flex-col items-center rounded-full"
      style={{
        width: size,
        padding: `${gap + 1}px ${gap}px`,
        gap: `${gap}px`,
        backgroundColor: "#0a1020",
        border: "1px solid #232D42",
      }}
      title={status}
    >
      {(["red", "amber", "green"] as const).map((light) => (
        <div
          key={light}
          style={{
            width: dotSize,
            height: dotSize,
            borderRadius: "50%",
            backgroundColor: active === light ? colors[light].on : colors.off,
            boxShadow: active === light ? `0 0 ${dotSize}px ${colors[light].glow}` : "none",
            transition: "all 300ms ease",
          }}
        />
      ))}
    </div>
  );
}
