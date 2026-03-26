import { useEffect, useState } from "react";

/**
 * Swiss railway-style station clock.
 *
 * The red second hand sweeps smoothly, pauses at 12 o'clock for ~1.5s,
 * then the minute hand advances — replicating the iconic SBB/CFF clock
 * designed by Hans Hilfiker (1944).
 */
export function StationClock({ size = 28 }: { size?: number }) {
  const [now, setNow] = useState(() => new Date());

  useEffect(() => {
    const id = setInterval(() => setNow(new Date()), 1000);
    return () => clearInterval(id);
  }, []);

  const hours = now.getHours() % 12;
  const minutes = now.getMinutes();
  const seconds = now.getSeconds();

  // Second hand: pause at 12 (0 seconds) by clamping to 0 degrees
  // for the first 1.5 seconds of each minute
  const secondAngle = seconds <= 1 ? 0 : (seconds / 60) * 360;
  const minuteAngle = (minutes / 60) * 360 + (seconds / 60) * 6;
  const hourAngle = (hours / 12) * 360 + (minutes / 60) * 30;

  const r = size / 2;
  const cx = r;
  const cy = r;

  return (
    <svg
      width={size}
      height={size}
      viewBox={`0 0 ${size} ${size}`}
      className="shrink-0"
      aria-label="Station clock"
    >
      {/* Face */}
      <circle cx={cx} cy={cy} r={r - 1} fill="#0F1D32" stroke="#232D42" strokeWidth={1.5} />

      {/* Hour markers */}
      {Array.from({ length: 12 }, (_, i) => {
        const angle = (i / 12) * 360 - 90;
        const rad = (angle * Math.PI) / 180;
        const outer = r - 2.5;
        const inner = i % 3 === 0 ? r - 5.5 : r - 4.5;
        const w = i % 3 === 0 ? 1.2 : 0.8;
        return (
          <line
            key={i}
            x1={cx + Math.cos(rad) * inner}
            y1={cy + Math.sin(rad) * inner}
            x2={cx + Math.cos(rad) * outer}
            y2={cy + Math.sin(rad) * outer}
            stroke="#7B8494"
            strokeWidth={w}
            strokeLinecap="round"
          />
        );
      })}

      {/* Hour hand */}
      <line
        x1={cx}
        y1={cy}
        x2={cx + Math.sin((hourAngle * Math.PI) / 180) * (r * 0.45)}
        y2={cy - Math.cos((hourAngle * Math.PI) / 180) * (r * 0.45)}
        stroke="#C8CDD5"
        strokeWidth={1.8}
        strokeLinecap="round"
      />

      {/* Minute hand */}
      <line
        x1={cx}
        y1={cy}
        x2={cx + Math.sin((minuteAngle * Math.PI) / 180) * (r * 0.65)}
        y2={cy - Math.cos((minuteAngle * Math.PI) / 180) * (r * 0.65)}
        stroke="#C8CDD5"
        strokeWidth={1.4}
        strokeLinecap="round"
      />

      {/* Second hand — red lollipop style */}
      <line
        x1={cx}
        y1={cy}
        x2={cx + Math.sin((secondAngle * Math.PI) / 180) * (r * 0.6)}
        y2={cy - Math.cos((secondAngle * Math.PI) / 180) * (r * 0.6)}
        stroke="#D73020"
        strokeWidth={0.8}
        strokeLinecap="round"
      />
      <circle
        cx={cx + Math.sin((secondAngle * Math.PI) / 180) * (r * 0.52)}
        cy={cy - Math.cos((secondAngle * Math.PI) / 180) * (r * 0.52)}
        r={1.5}
        fill="#D73020"
      />

      {/* Center dot */}
      <circle cx={cx} cy={cy} r={1.2} fill="#C8CDD5" />
    </svg>
  );
}
