/**
 * Station name plate section header with per-theme decoration.
 *
 * The layout stays identical across themes — only the decorative
 * prefix/suffix changes, keeping information in the same place.
 *
 * Theme decorations:
 * - Classic: plain (clean baseline)
 * - London Underground: small roundel icon
 * - Swiss: yellow section letter prefix
 * - Shinkansen: lenticular brackets 【】
 * - NYC Subway: colored route bullet
 * - Orient Express: diamond ornament
 * - Platform 9¾: star ornament
 * - Trans-Siberian: km marker
 * - Pullman: brass diamond
 */

const themeDecoration: Record<string, { prefix?: string; suffix?: string }> = {
  "conductor-classic": {},
  "london-underground": { prefix: "\u25CF " }, // filled circle (roundel nod)
  "swiss-federal": {},  // letter prefix handled separately
  "shinkansen": { prefix: "\u3010 ", suffix: " \u3011" }, // 【 】
  "nyc-subway": { prefix: "\u25CF " }, // route bullet
  "platform-9-three-quarters": { prefix: "\u2726 " }, // star
  "orient-express": { prefix: "\u25C6 " }, // diamond
  "trans-siberian": {},
  "pullman-class": { prefix: "\u25C6 " }, // brass diamond
};

// Swiss section letter mapping
const swissLetters: Record<string, string> = {
  "active worktrees": "A",
  "stations": "B",
  "repos": "B",
  "worktrees": "C",
  "tickets": "D",
  "workflows": "E",
  "settings": "F",
  "attention required": "!",
  "recent runs": "E",
};

function getThemeId(): string {
  return document.documentElement.getAttribute("data-theme") ?? "conductor-classic";
}

export function StationHeader({
  children,
  count,
}: {
  children: React.ReactNode;
  count?: number;
}) {
  const themeId = getThemeId();
  const deco = themeDecoration[themeId] ?? {};
  const label = typeof children === "string" ? children.toLowerCase() : "";
  const swissLetter = themeId === "swiss-federal" ? swissLetters[label] : null;

  return (
    <div className="mb-2">
      <div className="flex items-center gap-0">
        {/* Swiss yellow letter prefix */}
        {swissLetter && (
          <span
            className="text-xs font-bold px-1.5 py-0.5 mr-1.5"
            style={{ backgroundColor: "#FFD700", color: "#000000" }}
          >
            {swissLetter}
          </span>
        )}
        <h3 className="text-xs font-semibold uppercase tracking-wider text-gray-400">
          {deco.prefix && <span style={{ color: "var(--color-indigo-500)" }}>{deco.prefix}</span>}
          {children}
          {deco.suffix && <span style={{ color: "var(--color-indigo-500)" }}>{deco.suffix}</span>}
          {count !== undefined && (
            <span className="ml-1 font-mono">({count})</span>
          )}
        </h3>
      </div>
    </div>
  );
}
