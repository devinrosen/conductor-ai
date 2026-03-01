import { createContext, useContext, useMemo, useState, useCallback } from "react";
import { Outlet, useNavigate } from "react-router";
import { Sidebar } from "./Sidebar";
import { useApi } from "../../hooks/useApi";
import { api } from "../../api/client";
import type { Repo } from "../../api/types";
import {
  useConductorEvents,
  type ConductorEventType,
  type ConductorEventData,
} from "../../hooks/useConductorEvents";
import { useHotkeys } from "../../hooks/useHotkeys";
import { KeyboardShortcutHelp } from "../shared/KeyboardShortcutHelp";

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
  const [helpOpen, setHelpOpen] = useState(false);
  const navigate = useNavigate();

  const openHelp = useCallback(() => setHelpOpen(true), []);
  const closeHelp = useCallback(() => setHelpOpen(false), []);
  const goToDashboard = useCallback(() => navigate("/"), [navigate]);
  const goToTickets = useCallback(() => navigate("/tickets"), [navigate]);
  const goToSettings = useCallback(() => navigate("/settings"), [navigate]);

  useHotkeys([
    { key: "?", handler: openHelp, description: "Show keyboard shortcuts" },
    { key: "g d", handler: goToDashboard, description: "Go to Dashboard" },
    { key: "g t", handler: goToTickets, description: "Go to Tickets" },
    { key: "g s", handler: goToSettings, description: "Go to Settings" },
  ]);

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
      <KeyboardShortcutHelp open={helpOpen} onClose={closeHelp} />
    </ReposContext.Provider>
  );
}
