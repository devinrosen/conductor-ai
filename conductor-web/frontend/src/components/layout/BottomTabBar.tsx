import { NavLink } from "react-router";

export function BottomTabBar() {
  return (
    <div
      className="fixed bottom-0 left-0 right-0 md:hidden bg-white border-t border-gray-200 z-20 flex"
      style={{ paddingBottom: "env(safe-area-inset-bottom)" }}
    >
      <NavLink
        to="/"
        end
        className={({ isActive }) =>
          `flex flex-col items-center flex-1 py-2 text-xs border-t-2 ${
            isActive
              ? "text-indigo-600 border-indigo-600"
              : "text-gray-500 border-transparent"
          }`
        }
        style={{ minHeight: 56 }}
      >
        <svg className="w-5 h-5 mb-0.5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
          <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={1.5} d="M13 10V3L4 14h7v7l9-11h-7z" />
        </svg>
        Activity
      </NavLink>
      <NavLink
        to="/repos"
        className={({ isActive }) =>
          `flex flex-col items-center flex-1 py-2 text-xs border-t-2 ${
            isActive
              ? "text-indigo-600 border-indigo-600"
              : "text-gray-500 border-transparent"
          }`
        }
        style={{ minHeight: 56 }}
      >
        <svg className="w-5 h-5 mb-0.5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
          <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={1.5} d="M3 7v10a2 2 0 002 2h14a2 2 0 002-2V9a2 2 0 00-2-2h-6l-2-2H5a2 2 0 00-2 2z" />
        </svg>
        Repos
      </NavLink>
      <NavLink
        to="/workflows"
        className={({ isActive }) =>
          `flex flex-col items-center flex-1 py-2 text-xs border-t-2 ${
            isActive
              ? "text-indigo-600 border-indigo-600"
              : "text-gray-500 border-transparent"
          }`
        }
        style={{ minHeight: 56 }}
      >
        <svg className="w-5 h-5 mb-0.5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
          <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={1.5} d="M4 4v5h.582m15.356 2A8.001 8.001 0 004.582 9m0 0H9m11 11v-5h-.581m0 0a8.003 8.003 0 01-15.357-2m15.357 2H15" />
        </svg>
        Workflows
      </NavLink>
    </div>
  );
}
