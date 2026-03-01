import { NavLink } from "react-router";
import { useRepos } from "./AppShell";

const linkClass = ({ isActive }: { isActive: boolean }) =>
  `block px-3 py-2 rounded-md text-sm ${
    isActive
      ? "bg-indigo-100 text-indigo-700 font-medium"
      : "text-gray-700 hover:bg-gray-100"
  }`;

export function Sidebar() {
  const { repos, loading } = useRepos();

  return (
    <aside className="w-60 shrink-0 border-r border-gray-200 bg-white flex flex-col">
      <div className="px-4 py-4 border-b border-gray-200">
        <h1 className="text-lg font-bold text-gray-900">Conductor</h1>
      </div>

      <nav className="flex-1 overflow-auto p-3 space-y-1">
        <NavLink to="/" end className={linkClass}>
          Dashboard
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
      </div>
    </aside>
  );
}
