import { themes, type Theme } from "../../themes/themes";
import { useTheme } from "../../themes/useTheme";

/**
 * "Mystery Destination" theme picker.
 *
 * Locked themes show full concealment: no name, no palette, no description.
 * Just a silhouette card with a lock icon and a cryptic hint. Maximum
 * mystery, maximum reward on unlock.
 *
 * Conductor Classic is always unlocked. All others show as locked mystery
 * cards. Clicking any card still applies the theme (no actual locking).
 */

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

function MysteryCard({
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
      className={`text-left rounded-lg border p-3 transition-colors opacity-75 hover:opacity-90 group ${
        isActive
          ? "border-indigo-500 bg-indigo-100 opacity-100"
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
      <div className="flex gap-0.5 rounded overflow-hidden h-6">
        {Array.from({ length: 7 }).map((_, i) => (
          <div
            key={i}
            className={`rounded relative overflow-hidden ${i < 4 ? "w-4" : "w-2.5"}`}
            style={{ backgroundColor: "#2A2A2A" }}
          >
            <div className="swatch-shimmer opacity-0 group-hover:opacity-100 transition-opacity" />
          </div>
        ))}
      </div>
      <p className="text-xs italic mt-2" style={{ color: "var(--color-gray-500)" }}>
        &ldquo;{theme.unlockHint}&rdquo;
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
        const isUnlocked = t.unlockCondition === "always";
        const isActive = t.id === current.id;
        const onClick = () => setTheme(t);

        return isUnlocked ? (
          <UnlockedCard key={t.id} theme={t} isActive={isActive} onClick={onClick} />
        ) : (
          <MysteryCard key={t.id} theme={t} isActive={isActive} onClick={onClick} />
        );
      })}
    </div>
  );
}
