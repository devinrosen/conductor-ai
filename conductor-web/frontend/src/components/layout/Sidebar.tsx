import { NavLink } from "react-router";
import { useRepos } from "./AppShell";
import { NotificationBell } from "../notifications/NotificationBell";
import { StationClock } from "../shared/StationClock";
import { ThemeLogo } from "../shared/ThemeLogo";
import { useThemeId } from "../../themes/useTheme";

// Per-theme repo indicator colors (cycling palette)
const indicatorColors = [
  "#2B5EA7", "#39B54A", "#FF9500", "#D73020", "#CD853F",
  "#00B5AD", "#9B0056", "#6CBE45", "#B36305", "#0098D4",
];

function RepoIndicator({ index }: { index: number }) {
  const theme = useThemeId();
  const color = indicatorColors[index % indicatorColors.length];
  const s = 7;

  // Shape varies by theme
  switch (theme) {
    case "swiss-federal":
    case "nyc-subway":
      // Square
      return <span className="shrink-0" style={{ width: s, height: s, backgroundColor: color, display: "inline-block" }} />;
    case "orient-express":
    case "pullman-class":
      // Diamond
      return <span className="shrink-0" style={{ width: s, height: s, backgroundColor: color, display: "inline-block", transform: "rotate(45deg)" }} />;
    case "shinkansen":
      // Horizontal bar
      return <span className="shrink-0 rounded-sm" style={{ width: 10, height: 3, backgroundColor: color, display: "inline-block" }} />;
    default:
      // Circle (Classic, London, 9¾, Trans-Siberian)
      return <span className="shrink-0 rounded-full" style={{ width: s, height: s, backgroundColor: color, display: "inline-block" }} />;
  }
}

const linkClass = ({ isActive }: { isActive: boolean }) =>
  `flex items-center justify-between px-2.5 py-1.5 rounded-md text-sm ${
    isActive
      ? "bg-indigo-100 text-indigo-700 font-medium"
      : "text-gray-700 hover:bg-gray-100"
  }`;

function ShortcutHint({ keys }: { keys: string }) {
  return (
    <span className="text-[10px] text-gray-400 font-mono hidden md:inline">
      {keys}
    </span>
  );
}

interface SidebarProps {
  open: boolean;
  onClose: () => void;
}

export function Sidebar({ open, onClose }: SidebarProps) {
  const { repos, loading } = useRepos();

  return (
    <aside
      className={`
        fixed inset-y-0 left-0 z-40 w-60 shrink-0 border-r border-gray-200 bg-white flex flex-col
        transform transition-transform duration-200 ease-in-out
        ${open ? "translate-x-0 pointer-events-auto" : "-translate-x-full pointer-events-none"}
        md:static md:translate-x-0 md:inset-auto md:pointer-events-auto
      `}
    >
      <div className="px-3 py-3 border-b border-gray-200 flex items-center justify-between">
        <div className="flex items-center gap-2">
          <StationClock size={20} />
          <ThemeLogo size={24} />
          <h1 className="text-base font-bold text-gray-900">Conductor</h1>
        </div>
        <div className="hidden md:block">
          <NotificationBell />
        </div>
        {/* Close button only shown on mobile */}
        <button
          onClick={onClose}
          className="md:hidden flex items-center justify-center rounded text-gray-500 hover:bg-gray-100"
          style={{ minHeight: 44, minWidth: 44 }}
          aria-label="Close menu"
        >
          ✕
        </button>
      </div>

      <nav className="flex-1 overflow-auto p-2 space-y-0.5">
        <NavLink to="/" end className={linkClass}>
          Home <ShortcutHint keys="g d" />
        </NavLink>
        <NavLink to="/repos" className={linkClass}>
          Repos
        </NavLink>
        <NavLink to="/workflows" className={linkClass}>
          Workflows
        </NavLink>
        <NavLink to="/tickets" className={linkClass}>
          Tickets <ShortcutHint keys="g t" />
        </NavLink>
        <div className="pt-3 pb-1 px-2.5">
          <span className="text-xs font-semibold uppercase tracking-wider text-gray-400">
            Repos
          </span>
        </div>

        {loading && (
          <div className="px-3 py-2 text-sm text-gray-400">Loading...</div>
        )}

        {repos.map((repo, i) => (
          <NavLink
            key={repo.id}
            to={`/repos/${repo.id}`}
            className={linkClass}
          >
            <span className="flex items-center gap-1.5">
              <RepoIndicator index={i} />
              {repo.slug}
            </span>
          </NavLink>
        ))}

        {!loading && repos.length === 0 && (
          <div className="px-3 py-2 text-sm text-gray-400">
            {"The station is quiet"}
          </div>
        )}
      </nav>

      <div className="border-t border-gray-200 p-2">
        <NavLink to="/settings" className={linkClass}>
          Settings <ShortcutHint keys="g s" />
        </NavLink>
        <NavLink to="/getting-started" className={linkClass}>
          Getting Started <span className="ml-auto">🚂</span>
        </NavLink>
        <div className="mt-2 px-2.5 text-xs text-gray-400 space-y-0.5">
          <div><kbd className="px-1 py-0.5 bg-gray-100 rounded text-gray-500 font-mono text-[10px]">&#8984;K</kbd> command palette</div>
          <div><kbd className="px-1 py-0.5 bg-gray-100 rounded text-gray-500 font-mono text-[10px]">?</kbd> shortcuts</div>
        </div>
      </div>
    </aside>
  );
}
