/**
 * Railway-themed SVG illustrations for empty states.
 * Line-art style, single accent color, dark-mode native.
 */

const stroke = "var(--color-gray-400, #4A5568)";
const accent = "var(--color-indigo-800, #CD853F)"; // brass

export function EmptyPlatform({ size = 64 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 64 64" fill="none" xmlns="http://www.w3.org/2000/svg">
      {/* Platform */}
      <rect x="8" y="42" width="48" height="4" rx="1" stroke={stroke} strokeWidth="1.5" />
      {/* Shelter/canopy */}
      <path d="M16 42 V28 H48 V42" stroke={stroke} strokeWidth="1.5" />
      <path d="M12 28 H52" stroke={accent} strokeWidth="1.5" />
      {/* Tracks disappearing into distance */}
      <line x1="4" y1="52" x2="60" y2="52" stroke={stroke} strokeWidth="1" />
      <line x1="4" y1="56" x2="60" y2="56" stroke={stroke} strokeWidth="1" />
      {/* Cross ties */}
      {[12, 22, 32, 42, 52].map((x) => (
        <line key={x} x1={x} y1="50" x2={x} y2="58" stroke={stroke} strokeWidth="0.8" opacity="0.5" />
      ))}
      {/* Empty sign */}
      <rect x="24" y="32" width="16" height="6" rx="1" stroke={accent} strokeWidth="1" />
    </svg>
  );
}

export function ParallelTracks({ size = 64 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 64 64" fill="none" xmlns="http://www.w3.org/2000/svg">
      {/* Two parallel tracks converging to vanishing point */}
      <line x1="4" y1="52" x2="32" y2="20" stroke={stroke} strokeWidth="1.5" />
      <line x1="60" y1="52" x2="32" y2="20" stroke={stroke} strokeWidth="1.5" />
      <line x1="8" y1="56" x2="32" y2="24" stroke={stroke} strokeWidth="1.5" />
      <line x1="56" y1="56" x2="32" y2="24" stroke={stroke} strokeWidth="1.5" />
      {/* Cross ties */}
      {[0, 1, 2, 3, 4].map((i) => {
        const y = 52 - i * 7;
        const spread = 28 - i * 5;
        return (
          <line key={i} x1={32 - spread} y1={y} x2={32 + spread} y2={y}
            stroke={stroke} strokeWidth="0.8" opacity={0.3 + i * 0.1} />
        );
      })}
      {/* Vanishing point dot */}
      <circle cx="32" cy="20" r="2" fill={accent} />
    </svg>
  );
}

export function ClosedTicketWindow({ size = 64 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 64 64" fill="none" xmlns="http://www.w3.org/2000/svg">
      {/* Window frame */}
      <rect x="12" y="12" width="40" height="36" rx="2" stroke={stroke} strokeWidth="1.5" />
      {/* Window opening */}
      <rect x="16" y="16" width="32" height="20" rx="1" stroke={stroke} strokeWidth="1" />
      {/* Closed shutter */}
      <line x1="16" y1="26" x2="48" y2="26" stroke={accent} strokeWidth="1.5" />
      <line x1="16" y1="20" x2="48" y2="32" stroke={stroke} strokeWidth="0.8" opacity="0.3" />
      <line x1="16" y1="32" x2="48" y2="20" stroke={stroke} strokeWidth="0.8" opacity="0.3" />
      {/* Counter shelf */}
      <rect x="14" y="38" width="36" height="2" rx="0.5" stroke={stroke} strokeWidth="1" />
      {/* "CLOSED" text placeholder */}
      <rect x="22" y="42" width="20" height="4" rx="1" stroke={accent} strokeWidth="0.8" opacity="0.6" />
    </svg>
  );
}

export function QuietRoundhouse({ size = 64 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 64 64" fill="none" xmlns="http://www.w3.org/2000/svg">
      {/* Roundhouse arch */}
      <path d="M12 48 V28 C12 18 52 18 52 28 V48" stroke={stroke} strokeWidth="1.5" />
      {/* Door */}
      <rect x="24" y="30" width="16" height="18" rx="1" stroke={stroke} strokeWidth="1.5" />
      <line x1="32" y1="30" x2="32" y2="48" stroke={stroke} strokeWidth="0.8" />
      {/* Door handles */}
      <circle cx="29" cy="40" r="1" fill={accent} />
      <circle cx="35" cy="40" r="1" fill={accent} />
      {/* Tracks leading in */}
      <line x1="28" y1="48" x2="28" y2="58" stroke={stroke} strokeWidth="1" />
      <line x1="36" y1="48" x2="36" y2="58" stroke={stroke} strokeWidth="1" />
      {/* Smoke stack */}
      <rect x="30" y="14" width="4" height="6" rx="1" stroke={stroke} strokeWidth="1" />
    </svg>
  );
}

export function BlankDepartureBoard({ size = 64 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 64 64" fill="none" xmlns="http://www.w3.org/2000/svg">
      {/* Board frame */}
      <rect x="8" y="12" width="48" height="32" rx="2" stroke={stroke} strokeWidth="1.5" />
      {/* Rows (empty flaps) */}
      {[20, 28, 36].map((y) => (
        <g key={y}>
          <rect x="12" y={y} width="40" height="5" rx="1" stroke={stroke} strokeWidth="0.8" opacity="0.4" />
          <line x1="14" y1={y + 2.5} x2="50" y2={y + 2.5} stroke={stroke} strokeWidth="0.5" opacity="0.2" />
        </g>
      ))}
      {/* Clock on top */}
      <circle cx="32" cy="12" r="4" stroke={accent} strokeWidth="1" fill="none" />
      <line x1="32" y1="10" x2="32" y2="12" stroke={accent} strokeWidth="0.8" />
      <line x1="32" y1="12" x2="34" y2="12" stroke={accent} strokeWidth="0.8" />
      {/* Support posts */}
      <line x1="12" y1="44" x2="12" y2="52" stroke={stroke} strokeWidth="1.5" />
      <line x1="52" y1="44" x2="52" y2="52" stroke={stroke} strokeWidth="1.5" />
    </svg>
  );
}

export function TrackSwitchIcon({ size = 20 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 20 20" fill="none" xmlns="http://www.w3.org/2000/svg">
      {/* Main track */}
      <line x1="2" y1="10" x2="10" y2="10" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
      <line x1="10" y1="10" x2="18" y2="10" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
      {/* Diverging track */}
      <line x1="10" y1="10" x2="18" y2="4" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
      {/* Switch point */}
      <circle cx="10" cy="10" r="2" fill="currentColor" />
    </svg>
  );
}
