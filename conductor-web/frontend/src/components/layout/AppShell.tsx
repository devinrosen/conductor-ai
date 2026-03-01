import { createContext, useContext, useMemo } from "react";
import { Outlet } from "react-router";
import { Sidebar } from "./Sidebar";
import { useApi } from "../../hooks/useApi";
import { api } from "../../api/client";
import type { Repo } from "../../api/types";
import {
  useConductorEvents,
  type ConductorEventType,
  type ConductorEventData,
} from "../../hooks/useConductorEvents";

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

  const handlers = useMemo(() => {
    const refetchRepos = (_data: ConductorEventData) => refetch();
    const handleMap: Partial<
      Record<ConductorEventType, (data: ConductorEventData) => void>
    > = {
      repo_created: refetchRepos,
      repo_deleted: refetchRepos,
      lagged: refetchRepos,
    };
    return handleMap;
  }, [refetch]);

  useConductorEvents(handlers);

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
