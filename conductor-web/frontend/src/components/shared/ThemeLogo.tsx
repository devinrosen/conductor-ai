/**
 * Theme-specific logo mark for the sidebar header.
 *
 * Each theme gets a distinct visual identity while preserving
 * the core Conductor "C" + signal concept.
 */

function getThemeId(): string {
  return document.documentElement.getAttribute("data-theme") ?? "conductor-classic";
}

export function ThemeLogo({ size = 28 }: { size?: number }) {
  const theme = getThemeId();

  switch (theme) {
    case "london-underground":
      return <LondonRoundel size={size} />;
    case "swiss-federal":
      return <SwissSquare size={size} />;
    case "shinkansen":
      return <JRMark size={size} />;
    case "nyc-subway":
      return <SubwayBullet size={size} />;
    case "platform-9-three-quarters":
      return <MagicCrest size={size} />;
    default:
      // Classic, Orient Express, Trans-Siberian, Pullman all use the default logo file
      return <img src="/logo-header.svg" alt="Conductor" style={{ width: size, height: size }} />;
  }
}

function LondonRoundel({ size }: { size: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 28 28" fill="none">
      <circle cx="14" cy="14" r="11" stroke="#E32017" strokeWidth="3" fill="none" />
      <rect x="2" y="11" width="24" height="6" rx="0" fill="#003688" />
      <text x="14" y="16" textAnchor="middle" fontSize="5" fontWeight="600" fill="#FFF" fontFamily="sans-serif">C</text>
    </svg>
  );
}

function SwissSquare({ size }: { size: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 28 28" fill="none">
      <rect x="2" y="2" width="24" height="24" rx="0" fill="#EC0000" />
      <path d="M 17 7 A 8 8 0 1 0 17 21" stroke="#FFF" strokeWidth="3" strokeLinecap="butt" fill="none" />
      <rect x="18" y="13" width="3" height="3" fill="#FFF" opacity="0.6" />
    </svg>
  );
}

function JRMark({ size }: { size: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 28 28" fill="none">
      <rect x="2" y="6" width="24" height="16" rx="4" fill="#00B5AD" />
      <path d="M 15 10 A 5 5 0 1 0 15 18" stroke="#FFF" strokeWidth="2.5" strokeLinecap="round" fill="none" />
      <circle cx="17" cy="14" r="1.5" fill="#FF6B9D" />
      {/* Speed lines */}
      <line x1="20" y1="11" x2="24" y2="11" stroke="#FFF" strokeWidth="0.8" opacity="0.4" />
      <line x1="20" y1="14" x2="24" y2="14" stroke="#FFF" strokeWidth="0.8" opacity="0.5" />
      <line x1="20" y1="17" x2="24" y2="17" stroke="#FFF" strokeWidth="0.8" opacity="0.4" />
    </svg>
  );
}

function SubwayBullet({ size }: { size: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 28 28" fill="none">
      <circle cx="14" cy="14" r="12" fill="#FCCC0A" />
      <text x="14" y="18.5" textAnchor="middle" fontSize="14" fontWeight="800" fill="#000" fontFamily="sans-serif">C</text>
    </svg>
  );
}

function MagicCrest({ size }: { size: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 28 28" fill="none">
      {/* Shield shape */}
      <path d="M 14 2 L 24 8 V 18 C 24 22 14 26 14 26 C 14 26 4 22 4 18 V 8 Z"
        stroke="#C9A84C" strokeWidth="1.5" fill="none" />
      <text x="14" y="17" textAnchor="middle" fontSize="10" fontWeight="700" fill="#C9A84C" fontFamily="serif">C</text>
      {/* Tiny star */}
      <circle cx="14" cy="8" r="1.5" fill="#F5D98E" />
    </svg>
  );
}
