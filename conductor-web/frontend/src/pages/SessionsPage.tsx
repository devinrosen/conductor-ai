import { useState } from "react";
import { useApi } from "../hooks/useApi";
import { api } from "../api/client";
import { StatusBadge } from "../components/shared/StatusBadge";
import { TimeAgo } from "../components/shared/TimeAgo";
import { LoadingSpinner } from "../components/shared/LoadingSpinner";
import { EmptyState } from "../components/shared/EmptyState";
import { EndSessionForm } from "../components/sessions/EndSessionForm";

export function SessionsPage() {
  const { data: sessions, loading, refetch } = useApi(
    () => api.listSessions(),
    [],
  );
  const [starting, setStarting] = useState(false);

  async function handleStartSession() {
    setStarting(true);
    try {
      await api.startSession();
      refetch();
    } finally {
      setStarting(false);
    }
  }

  if (loading) return <LoadingSpinner />;

  const sorted = [...(sessions ?? [])].sort((a, b) => {
    // Active sessions first
    if (!a.ended_at && b.ended_at) return -1;
    if (a.ended_at && !b.ended_at) return 1;
    return b.started_at.localeCompare(a.started_at);
  });

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <h2 className="text-xl font-bold text-gray-900">Sessions</h2>
        <button
          onClick={handleStartSession}
          disabled={starting}
          className="px-3 py-1.5 text-sm rounded-md bg-indigo-600 text-white hover:bg-indigo-700 disabled:opacity-50"
        >
          {starting ? "Starting..." : "Start Session"}
        </button>
      </div>

      {sorted.length === 0 ? (
        <EmptyState message="No sessions yet. Start one to begin tracking your work." />
      ) : (
        <div className="rounded-lg border border-gray-200 bg-white overflow-hidden">
          <table className="w-full text-sm">
            <thead className="bg-gray-50 text-left text-xs text-gray-500 uppercase">
              <tr>
                <th className="px-4 py-2">Started</th>
                <th className="px-4 py-2">Ended</th>
                <th className="px-4 py-2">Status</th>
                <th className="px-4 py-2">Notes</th>
                <th className="px-4 py-2"></th>
              </tr>
            </thead>
            <tbody className="divide-y divide-gray-100">
              {sorted.map((s) => (
                <tr key={s.id}>
                  <td className="px-4 py-2 text-gray-600">
                    <TimeAgo date={s.started_at} />
                  </td>
                  <td className="px-4 py-2 text-gray-600">
                    {s.ended_at ? <TimeAgo date={s.ended_at} /> : "-"}
                  </td>
                  <td className="px-4 py-2">
                    <StatusBadge status={s.ended_at ? "closed" : "active"} />
                  </td>
                  <td className="px-4 py-2 text-gray-500 truncate max-w-xs">
                    {s.notes ?? "-"}
                  </td>
                  <td className="px-4 py-2">
                    {!s.ended_at && (
                      <EndSessionForm sessionId={s.id} onEnded={refetch} />
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
