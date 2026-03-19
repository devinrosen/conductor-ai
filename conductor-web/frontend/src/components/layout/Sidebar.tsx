import { NavLink } from "react-router";
import { useRepos } from "./AppShell";
import { NotificationBell } from "../notifications/NotificationBell";

const linkClass = ({ isActive }: { isActive: boolean }) =>
  `block px-3 py-2 rounded-md text-sm ${
    isActive
      ? "bg-indigo-100 text-indigo-700 font-medium"
      : "text-gray-700 hover:bg-gray-100"
  }`;

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
        ${open ? "translate-x-0" : "-translate-x-full"}
        md:static md:translate-x-0 md:inset-auto
      `}
    >
      <div className="px-4 py-4 border-b border-gray-200 flex items-center justify-between">
        <h1 className="text-lg font-bold text-gray-900">Conductor</h1>
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

      <nav className="flex-1 overflow-auto p-3 space-y-1">
        <NavLink to="/" end className={linkClass}>
          Activity
        </NavLink>
        <NavLink to="/repos" className={linkClass}>
          Repos
        </NavLink>
        <NavLink to="/workflows" className={linkClass}>
          Workflows
        </NavLink>
        <NavLink to="/tickets" className={linkClass}>
          Tickets
        </NavLink>
        <div className="pt-4 pb-1 px-3">
          <span className="text-xs font-semibold uppercase tracking-wider text-gray-400">
            Repos
          </span>
        </div>

        {loading && (
          <div className="px-3 py-2 text-sm text-gray-400">Loading...</div>
        )}

        {repos.map((repo) => (
          <NavLink
            key={repo.id}
            to={`/repos/${repo.id}`}
            className={linkClass}
          >
            {repo.slug}
          </NavLink>
        ))}

        {!loading && repos.length === 0 && (
          <div className="px-3 py-2 text-sm text-gray-400">No repos yet</div>
        )}
      </nav>

      <div className="border-t border-gray-200 p-3">
        <NavLink to="/settings" className={linkClass}>
          Settings
        </NavLink>
        <div className="mt-2 px-3 text-xs text-gray-400">
          Press <kbd className="px-1 py-0.5 bg-gray-100 rounded text-gray-500 font-mono text-[10px]">?</kbd> for shortcuts
        </div>
      </div>
    </aside>
  );
}
