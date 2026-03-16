import { createBrowserRouter, RouterProvider } from "react-router";
import { AppShell } from "./components/layout/AppShell";
import { ActivityPage } from "./pages/ActivityPage";
import { ReposPage } from "./pages/ReposPage";
import { WorkflowsPage } from "./pages/WorkflowsPage";
import { RepoDetailPage } from "./pages/RepoDetailPage";
import { WorktreeDetailPage } from "./pages/WorktreeDetailPage";
import { WorkflowRunDetailPage } from "./pages/WorkflowRunDetailPage";
import { TicketsPage } from "./pages/TicketsPage";
import { SettingsPage } from "./pages/SettingsPage";
import { NotFoundPage } from "./pages/NotFoundPage";

const router = createBrowserRouter([
  {
    element: <AppShell />,
    children: [
      { index: true, element: <ActivityPage /> },
      { path: "repos", element: <ReposPage /> },
      { path: "workflows", element: <WorkflowsPage /> },
      { path: "tickets", element: <TicketsPage /> },
      { path: "repos/:repoId", element: <RepoDetailPage /> },
      {
        path: "repos/:repoId/worktrees/:worktreeId",
        element: <WorktreeDetailPage />,
      },
      {
        path: "repos/:repoId/worktrees/:worktreeId/workflows/runs/:runId",
        element: <WorkflowRunDetailPage />,
      },
      { path: "settings", element: <SettingsPage /> },
      { path: "*", element: <NotFoundPage /> },
    ],
  },
]);

export default function App() {
  return <RouterProvider router={router} />;
}
