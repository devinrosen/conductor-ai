import { useEffect, useState } from "react";
import { themes, type Theme } from "../../themes/themes";
import { useTheme } from "../../themes/useTheme";
import { api } from "../../api/client";
import type { ThemeUnlockStats } from "../../api/types";

/**
 * "Mystery Destination" theme picker — three-tier unlock system.
 *
 * Tier 1 (Undiscovered): 0% progress — ???, cryptic hint, uniform gray swatches
 * Tier 2 (In Progress):  >0% — name + description revealed, progress bar, redacted swatches
 * Tier 3 (Unlocked):     condition met — full card, clickable
 */

interface ThemeProgress {
  current: number;
  target: number;
  label: string;
  percent: number;
}

/** Parse a condition and return { current, target, label, percent } or null if no progress. */
function getProgress(condition: string, stats: ThemeUnlockStats | null, label: string): ThemeProgress | null {
  if (!stats || condition === "always" || condition === "hidden" || condition === "oss_contributor") return null;

  let current = 0;
  let target = 1;

  if (condition.startsWith("repos_registered >= ")) {
    target = parseInt(condition.split(">= ")[1], 10);
    current = stats.repos_registered;
  } else if (condition.startsWith("workflow_streak >= ")) {
    target = parseInt(condition.split(">= ")[1], 10);
    current = stats.workflow_streak;
  } else if (condition.startsWith("prs_merged >= ")) {
    target = parseInt(condition.split(">= ")[1], 10);
    current = stats.prs_merged;
  } else if (condition.startsWith("usage_years >= ")) {
    target = parseFloat(condition.split(">= ")[1]);
    current = Math.round((stats.usage_days / 365) * 10) / 10; // 1 decimal
    const percent = Math.min(100, Math.round((stats.usage_days / (target * 365)) * 100));
    return { current, target, label, percent };
  } else if (condition.startsWith("parallel_agents >= ")) {
    target = parseInt(condition.split(">= ")[1], 10);
    current = stats.max_parallel_agents;
  } else if (condition.startsWith("workflow_steps >= ")) {
    target = parseInt(condition.split(">= ")[1], 10);
    current = stats.max_workflow_steps;
  } else {
    return null;
  }

  const percent = Math.min(100, Math.round((current / target) * 100));
  return { current, target, label, percent };
}

function isUnlocked(condition: string, stats: ThemeUnlockStats | null): boolean {
  const progress = getProgress(condition, stats, "");
  if (condition === "always") return true;
  if (!progress) return false;
  return progress.percent >= 100;
}

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

function RedactedSwatch() {
  return (
    <div className="flex gap-0.5 rounded overflow-hidden h-6 group">
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
  );
}

/** Tier 1: fully concealed */
function UndiscoveredCard({ theme }: { theme: Theme }) {
  return (
    <div className="text-left rounded-lg border p-3 opacity-60 cursor-default border-gray-200 group">
      <div className="flex items-center gap-2 mb-2">
        <span className="text-sm italic text-gray-400">
          <span className="mr-1.5">&#x1F512;</span>???
        </span>
      </div>
      <RedactedSwatch />
      <p className="text-xs italic mt-2 text-gray-500">
        &ldquo;{theme.unlockHint}&rdquo;
      </p>
    </div>
  );
}

/** Tier 2: name revealed, progress bar, redacted swatches */
function InProgressCard({ theme, progress }: { theme: Theme; progress: ThemeProgress }) {
  return (
    <div className="text-left rounded-lg border p-3 cursor-default border-gray-200 group">
      <div className="flex items-center justify-between gap-2 mb-2">
        <span className="text-sm font-semibold text-gray-800">
          <span className="mr-1.5">&#x1F512;</span>{theme.name}
        </span>
        <span className="text-xs font-mono text-gray-400">{progress.current}/{progress.target}</span>
      </div>
      <RedactedSwatch />
      <p className="text-xs text-gray-500 mt-2 line-clamp-1">
        {theme.description}
      </p>
      {/* Track-line progress bar */}
      <div className="mt-3 h-1 rounded-full bg-gray-700/30 relative overflow-hidden">
        <div
          className="h-full rounded-full transition-all duration-500 ease-out"
          style={{
            width: `${progress.percent}%`,
            backgroundColor: "var(--color-indigo-500, #2B5EA7)",
          }}
        />
      </div>
      <p className="text-[10px] font-mono text-gray-500 mt-1">
        {progress.current}/{progress.target} {progress.label.toLowerCase()}
      </p>
    </div>
  );
}

/** Tier 3: fully unlocked */
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
          : "border-gray-200 hover:border-gray-300"
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
  const [stats, setStats] = useState<ThemeUnlockStats | null>(null);

  useEffect(() => {
    api.getThemeUnlockStats().then(setStats).catch(() => {});
  }, []);

  return (
    <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-3">
      {themes.map((t) => {
        const unlocked = isUnlocked(t.unlockCondition, stats);
        const progress = getProgress(t.unlockCondition, stats, t.unlockProgressLabel);
        const isActive = t.id === current.id;
        const hasProgress = progress && progress.current > 0 && !unlocked;

        if (unlocked) {
          return <UnlockedCard key={t.id} theme={t} isActive={isActive} onClick={() => setTheme(t)} />;
        }
        if (hasProgress) {
          return <InProgressCard key={t.id} theme={t} progress={progress} />;
        }
        return <UndiscoveredCard key={t.id} theme={t} />;
      })}
    </div>
  );
}
