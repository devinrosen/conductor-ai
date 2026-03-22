import { themes, type Theme } from "../../themes/themes";
import { useTheme } from "../../themes/useTheme";

/**
 * Locked theme tiers (visual only — no actual locking enforced).
 *
 * Tier 1: Undiscovered — ???, gray swatches, cryptic hint
 * Tier 2: In Progress — name visible, redacted swatches with shimmer, progress bar
 * Tier 3: Unlocked — full card with real swatches
 *
 * For demo purposes, Conductor Classic is always Tier 3 (unlocked).
 * All other themes render as Tier 1 (undiscovered) with their hidden=true
 * flag determining extra mystery. Clicking any card still applies the theme.
 */

type Tier = "undiscovered" | "in-progress" | "unlocked";

function getTier(t: Theme): Tier {
  if (t.unlockCondition === "always") return "unlocked";
  // For demo: show some as in-progress, hidden ones as undiscovered
  if (t.hidden) return "undiscovered";
  return "in-progress";
}

// Fake progress values for demo display
const demoProgress: Record<string, { current: number; target: number }> = {
  "london-underground": { current: 3, target: 5 },
  "swiss-federal": { current: 6, target: 10 },
  "shinkansen": { current: 38, target: 50 },
  "orient-express": { current: 0, target: 1 },
  "nyc-subway": { current: 4, target: 10 },
  "trans-siberian": { current: 12, target: 20 },
  "pullman-class": { current: 0, target: 1 },
};

function RealSwatch({ palette }: { palette: Theme["palette"] }) {
  return (
    <div className="flex gap-0.5 rounded overflow-hidden h-6">
      <div className="w-4 rounded" style={{ backgroundColor: palette.gray50 }} />
      <div className="w-4 rounded" style={{ backgroundColor: palette.white }} />
      <div className="w-4 rounded" style={{ backgroundColor: palette.accent }} />
      <div className="w-4 rounded" style={{ backgroundColor: palette.accentGlow }} />
      <div className="w-2.5 rounded" style={{ backgroundColor: palette.statusGo }} />
      <div className="w-2.5 rounded" style={{ backgroundColor: palette.statusCaution }} />
      <div className="w-2.5 rounded" style={{ backgroundColor: palette.statusStop }} />
    </div>
  );
}

function RedactedSwatch({ shimmer }: { shimmer?: boolean }) {
  return (
    <div className="flex gap-0.5 rounded overflow-hidden h-6">
      {Array.from({ length: 7 }).map((_, i) => (
        <div
          key={i}
          className={`rounded relative overflow-hidden ${i < 4 ? "w-4" : "w-2.5"}`}
          style={{ backgroundColor: "#2A2A2A" }}
        >
          {shimmer && <div className="swatch-shimmer" />}
        </div>
      ))}
    </div>
  );
}

function GraySwatch() {
  return (
    <div className="flex gap-0.5 rounded overflow-hidden h-6">
      {Array.from({ length: 7 }).map((_, i) => (
        <div
          key={i}
          className={`rounded ${i < 4 ? "w-4" : "w-2.5"}`}
          style={{ backgroundColor: "#2A2A2A" }}
        />
      ))}
    </div>
  );
}

function ProgressBar({ current, target }: { current: number; target: number }) {
  const pct = Math.min((current / target) * 100, 100);
  return (
    <div className="mt-2">
      <div className="h-1 rounded-full overflow-hidden" style={{ backgroundColor: "var(--color-gray-200)" }}>
        <div
          className="h-full rounded-full relative"
          style={{
            width: `${pct}%`,
            backgroundColor: "var(--color-indigo-500)",
            transition: "width 500ms ease-out",
          }}
        >
          <div
            className="absolute -right-1 -top-0.5 w-2 h-2 rounded-full"
            style={{ backgroundColor: "var(--color-indigo-500)" }}
          />
        </div>
      </div>
      <p className="text-[10px] font-mono mt-1" style={{ color: "var(--color-gray-400)" }}>
        {current} / {target} {" "}
      </p>
    </div>
  );
}

function UndiscoveredCard({
  theme,
  isActive,
  onClick,
}: {
  theme: Theme;
  isActive: boolean;
  onClick: () => void;
}) {
  return (
    <button
      onClick={onClick}
      className={`text-left rounded-lg border p-3 transition-colors opacity-80 hover:opacity-90 ${
        isActive
          ? "border-indigo-500 bg-indigo-100"
          : "border-gray-200 hover:border-gray-300 bg-white"
      }`}
    >
      <div className="flex items-center justify-between gap-2 mb-2">
        <span className="text-sm italic" style={{ color: "var(--color-gray-400)" }}>
          <span className="mr-1.5">&#x1F512;</span>???
        </span>
        {isActive && (
          <span className="text-xs text-indigo-600 font-medium shrink-0">Active</span>
        )}
      </div>
      <GraySwatch />
      <p className="text-xs italic mt-2" style={{ color: "var(--color-gray-500)" }}>
        &ldquo;{theme.unlockHint}&rdquo;
      </p>
    </button>
  );
}

function InProgressCard({
  theme,
  isActive,
  onClick,
}: {
  theme: Theme;
  isActive: boolean;
  onClick: () => void;
}) {
  const progress = demoProgress[theme.id] ?? { current: 0, target: 1 };
  return (
    <button
      onClick={onClick}
      className={`text-left rounded-lg border p-3 transition-colors ${
        isActive
          ? "border-indigo-500 bg-indigo-100"
          : "border-gray-200 hover:border-gray-300 bg-white"
      }`}
    >
      <div className="flex items-center justify-between gap-2 mb-2">
        <span className="text-sm font-semibold text-gray-900">
          <span className="mr-1.5" style={{ color: "var(--color-gray-400)" }}>&#x1F512;</span>
          {theme.name}
        </span>
        <span className="text-[10px] font-mono shrink-0" style={{ color: "var(--color-gray-400)" }}>
          {isActive ? (
            <span className="text-xs text-indigo-600 font-medium font-sans">Active</span>
          ) : (
            `${progress.current}/${progress.target}`
          )}
        </span>
      </div>
      <RedactedSwatch shimmer />
      <p className="text-xs mt-2" style={{ color: "var(--color-gray-500)" }}>
        {theme.description}
      </p>
      <ProgressBar current={progress.current} target={progress.target} />
      <p className="text-[10px] mt-0.5" style={{ color: "var(--color-gray-400)" }}>
        {theme.unlockProgressLabel}
      </p>
    </button>
  );
}

function UnlockedCard({
  theme,
  isActive,
  onClick,
}: {
  theme: Theme;
  isActive: boolean;
  onClick: () => void;
}) {
  return (
    <button
      onClick={onClick}
      className={`text-left rounded-lg border p-3 transition-colors ${
        isActive
          ? "border-indigo-500 bg-indigo-100"
          : "border-gray-200 hover:border-gray-300 bg-white"
      }`}
    >
      <div className="flex items-center justify-between gap-2 mb-2">
        <span className="text-sm font-medium text-gray-900 truncate">
          {theme.name}
        </span>
        {isActive && (
          <span className="text-xs text-indigo-600 font-medium shrink-0">Active</span>
        )}
      </div>
      <RealSwatch palette={theme.palette} />
      <p className="text-xs text-gray-500 mt-2 line-clamp-1">
        {theme.description}
      </p>
    </button>
  );
}

export function ThemePicker() {
  const { theme: current, setTheme } = useTheme();

  return (
    <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-3">
      {themes.map((t) => {
        const tier = getTier(t);
        const isActive = t.id === current.id;
        const onClick = () => setTheme(t);

        switch (tier) {
          case "undiscovered":
            return <UndiscoveredCard key={t.id} theme={t} isActive={isActive} onClick={onClick} />;
          case "in-progress":
            return <InProgressCard key={t.id} theme={t} isActive={isActive} onClick={onClick} />;
          case "unlocked":
            return <UnlockedCard key={t.id} theme={t} isActive={isActive} onClick={onClick} />;
        }
      })}
    </div>
  );
}
