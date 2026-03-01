import { createBrowserRouter, RouterProvider } from "react-router";
import { AppShell } from "./components/layout/AppShell";
import { DashboardPage } from "./pages/DashboardPage";
import { RepoDetailPage } from "./pages/RepoDetailPage";
import { WorktreeDetailPage } from "./pages/WorktreeDetailPage";
import { TicketsPage } from "./pages/TicketsPage";
import { SettingsPage } from "./pages/SettingsPage";
import { NotFoundPage } from "./pages/NotFoundPage";

const router = createBrowserRouter([
  {
    element: <AppShell />,
    children: [
      { index: true, element: <DashboardPage /> },
      { path: "tickets", element: <TicketsPage /> },
      { path: "repos/:repoId", element: <RepoDetailPage /> },
      {
        path: "repos/:repoId/worktrees/:worktreeId",
        element: <WorktreeDetailPage />,
      },
      { path: "settings", element: <SettingsPage /> },
      { path: "*", element: <NotFoundPage /> },
    ],
  },
]);

export default function App() {
  return <RouterProvider router={router} />;
}
