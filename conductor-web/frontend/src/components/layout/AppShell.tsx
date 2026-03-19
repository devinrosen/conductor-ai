import { createContext, useContext, useMemo, useState, useCallback, useEffect } from "react";
import { Outlet, useNavigate, useLocation } from "react-router";
import { Sidebar } from "./Sidebar";
import { BottomTabBar } from "./BottomTabBar";
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
import { NotificationBell } from "../notifications/NotificationBell";
import { ToastContainer } from "../notifications/ToastContainer";
import { useToast } from "../../hooks/useToast";

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
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const { toasts, addToast, dismissToast } = useToast();
  const navigate = useNavigate();
  const location = useLocation();

  // Auto-close sidebar when route changes (mobile drawer behaviour)
  useEffect(() => {
    setSidebarOpen(false);
  }, [location.pathname]);

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
      repo_registered: refetchRepos,
      repo_unregistered: refetchRepos,
      lagged: refetchRepos,
      notification_created: (data: ConductorEventData) => {
        if (data.data) {
          const d = data.data as Record<string, string>;
          addToast({
            title: d.title || d.kind?.replace(/_/g, " ") || "Notification",
            body: d.body || "",
            severity: (d.severity as "info" | "warning" | "action_required") ?? "info",
          });
        }
      },
    };
    return handleMap;
  }, [refetch, addToast]);

  useConductorEvents(handlers);

  return (
    <ReposContext.Provider
      value={{ repos: repos ?? [], loading, refreshRepos: refetch }}
    >
      <div className="flex h-screen bg-gray-50">
        {/* Mobile backdrop */}
        {sidebarOpen && (
          <div
            className="fixed inset-0 bg-black/40 z-30 md:hidden"
            onClick={() => setSidebarOpen(false)}
          />
        )}
        <Sidebar open={sidebarOpen} onClose={() => setSidebarOpen(false)} />
        <main className="flex-1 overflow-auto">
          {/* Mobile top bar */}
          <div className="md:hidden flex items-center gap-3 px-4 border-b border-gray-200 bg-white sticky top-0 z-20" style={{ minHeight: 56 }}>
            <button
              onClick={() => setSidebarOpen(true)}
              className="flex items-center justify-center rounded text-gray-600 hover:bg-gray-100"
              style={{ minHeight: 44, minWidth: 44 }}
              aria-label="Open menu"
            >
              ☰
            </button>
            <span className="font-semibold text-gray-900">Conductor</span>
            <div className="ml-auto">
              <NotificationBell />
            </div>
          </div>
          <div className="p-4 md:p-6 pb-20 md:pb-6">
            <Outlet />
          </div>
        </main>
        <BottomTabBar />
      </div>
      <KeyboardShortcutHelp open={helpOpen} onClose={closeHelp} />
      <ToastContainer toasts={toasts} onDismiss={dismissToast} />
    </ReposContext.Provider>
  );
}
