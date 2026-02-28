import { createContext, useContext } from "react";
import { Outlet } from "react-router";
import { Sidebar } from "./Sidebar";
import { useApi } from "../../hooks/useApi";
import { api } from "../../api/client";
import type { Repo } from "../../api/types";

interface ReposContextValue {
  repos: Repo[];
  loading: boolean;
  refreshRepos: () => void;
}

const ReposContext = createContext<ReposContextValue>({
  repos: [],
  loading: true,
  refreshRepos: () => {},
});

export function useRepos() {
  return useContext(ReposContext);
}

export function AppShell() {
  const { data: repos, loading, refetch } = useApi(() => api.listRepos(), []);

  return (
    <ReposContext.Provider
      value={{ repos: repos ?? [], loading, refreshRepos: refetch }}
    >
      <div className="flex h-screen bg-gray-50">
        <Sidebar />
        <main className="flex-1 overflow-auto p-6">
          <Outlet />
        </main>
      </div>
    </ReposContext.Provider>
  );
}
