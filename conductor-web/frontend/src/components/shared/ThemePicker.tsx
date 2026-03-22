import { themes, type Theme } from "../../themes/themes";
import { useTheme } from "../../themes/useTheme";

function ThemeSwatch({ palette }: { palette: Theme["palette"] }) {
  return (
    <div className="flex gap-0.5 rounded overflow-hidden h-6">
      <div className="w-4" style={{ backgroundColor: palette.gray50 }} />
      <div className="w-4" style={{ backgroundColor: palette.white }} />
      <div className="w-4" style={{ backgroundColor: palette.accent }} />
      <div className="w-4" style={{ backgroundColor: palette.accentGlow }} />
      <div className="w-2" style={{ backgroundColor: palette.statusGo }} />
      <div className="w-2" style={{ backgroundColor: palette.statusCaution }} />
      <div className="w-2" style={{ backgroundColor: palette.statusStop }} />
    </div>
  );
}

export function ThemePicker() {
  const { theme: current, setTheme } = useTheme();

  return (
    <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-3">
      {themes.map((t) => {
        const isActive = t.id === current.id;
        return (
          <button
            key={t.id}
            onClick={() => setTheme(t)}
            className={`text-left rounded-lg border p-3 transition-colors ${
              isActive
                ? "border-indigo-500 bg-indigo-100"
                : "border-gray-200 hover:border-gray-300 bg-white"
            }`}
          >
            <div className="flex items-center justify-between gap-2 mb-2">
              <span className="text-sm font-medium text-gray-900 truncate">
                {t.name}
              </span>
              {isActive && (
                <span className="text-xs text-indigo-600 font-medium shrink-0">
                  Active
                </span>
              )}
            </div>
            <ThemeSwatch palette={t.palette} />
            <p className="text-xs text-gray-500 mt-2 line-clamp-1">
              {t.description}
            </p>
          </button>
        );
      })}
    </div>
  );
}
