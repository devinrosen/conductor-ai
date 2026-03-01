import { useMemo, useState } from "react";
import { useRepos } from "../components/layout/AppShell";
import { useApi } from "../hooks/useApi";
import { api } from "../api/client";
import { StatusBadge } from "../components/shared/StatusBadge";
import { LoadingSpinner } from "../components/shared/LoadingSpinner";
import { EmptyState } from "../components/shared/EmptyState";
import type { Ticket, Repo } from "../api/types";

function parseLabels(raw: string): string[] {
  try {
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

function matchesFilter(ticket: Ticket, filter: string): boolean {
  const lower = filter.toLowerCase();
  if (ticket.title.toLowerCase().includes(lower)) return true;
  if (ticket.source_id.toLowerCase().includes(lower)) return true;
  const labels = parseLabels(ticket.labels);
  if (labels.some((l) => l.toLowerCase().includes(lower))) return true;
  return false;
}

function TicketDetailModal({
  ticket,
  repoSlug,
  onClose,
}: {
  ticket: Ticket;
  repoSlug: string;
  onClose: () => void;
}) {
  const labels = parseLabels(ticket.labels);

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/40"
      onClick={onClose}
    >
      <div
        className="bg-white rounded-lg shadow-lg p-6 max-w-lg w-full mx-4 max-h-[80vh] overflow-auto"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-start justify-between gap-3">
          <h3 className="text-lg font-semibold text-gray-900">
            {ticket.source_id}: {ticket.title}
          </h3>
          <button
            onClick={onClose}
            className="text-gray-400 hover:text-gray-600 text-xl leading-none"
          >
            &times;
          </button>
        </div>

        <dl className="mt-4 space-y-3 text-sm">
          <div className="flex gap-2">
            <dt className="font-medium text-gray-500 w-24 shrink-0">State</dt>
            <dd>
              <StatusBadge status={ticket.state} />
            </dd>
          </div>
          <div className="flex gap-2">
            <dt className="font-medium text-gray-500 w-24 shrink-0">Repo</dt>
            <dd className="text-gray-900">{repoSlug}</dd>
          </div>
          <div className="flex gap-2">
            <dt className="font-medium text-gray-500 w-24 shrink-0">Source</dt>
            <dd className="text-gray-900">{ticket.source_type}</dd>
          </div>
          {ticket.assignee && (
            <div className="flex gap-2">
              <dt className="font-medium text-gray-500 w-24 shrink-0">
                Assignee
              </dt>
              <dd className="text-gray-900">{ticket.assignee}</dd>
            </div>
          )}
          {ticket.priority && (
            <div className="flex gap-2">
              <dt className="font-medium text-gray-500 w-24 shrink-0">
                Priority
              </dt>
              <dd className="text-gray-900">{ticket.priority}</dd>
            </div>
          )}
          {labels.length > 0 && (
            <div className="flex gap-2">
              <dt className="font-medium text-gray-500 w-24 shrink-0">
                Labels
              </dt>
              <dd className="flex flex-wrap gap-1">
                {labels.map((l) => (
                  <span
                    key={l}
                    className="px-1.5 py-0.5 text-xs rounded bg-gray-100 text-gray-600"
                  >
                    {l}
                  </span>
                ))}
              </dd>
            </div>
          )}
          {ticket.url && (
            <div className="flex gap-2">
              <dt className="font-medium text-gray-500 w-24 shrink-0">URL</dt>
              <dd>
                <a
                  href={ticket.url}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="text-indigo-600 hover:underline break-all"
                >
                  Open in browser
                </a>
              </dd>
            </div>
          )}
          <div className="flex gap-2">
            <dt className="font-medium text-gray-500 w-24 shrink-0">
              Synced
            </dt>
            <dd className="text-gray-500">{ticket.synced_at}</dd>
          </div>
        </dl>

        {ticket.body && (
          <div className="mt-4 pt-4 border-t border-gray-200">
            <h4 className="text-sm font-medium text-gray-500 mb-2">
              Description
            </h4>
            <p className="text-sm text-gray-700 whitespace-pre-wrap">
              {ticket.body}
            </p>
          </div>
        )}
      </div>
    </div>
  );
}

export function TicketsPage() {
  const { repos } = useRepos();
  const { data: tickets, loading } = useApi(() => api.listAllTickets(), []);
  const [filter, setFilter] = useState("");
  const [selected, setSelected] = useState<Ticket | null>(null);

  const repoMap = useMemo(() => {
    const map: Record<string, Repo> = {};
    for (const r of repos) map[r.id] = r;
    return map;
  }, [repos]);

  const filtered = useMemo(() => {
    if (!tickets) return [];
    if (!filter.trim()) return tickets;
    return tickets.filter((t) => matchesFilter(t, filter.trim()));
  }, [tickets, filter]);

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h2 className="text-xl font-bold text-gray-900">Tickets</h2>
        <input
          type="text"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          placeholder="Filter by title, ID, or label..."
          className="w-80 px-3 py-1.5 text-sm rounded-md border border-gray-300 focus:outline-none focus:ring-2 focus:ring-indigo-500 focus:border-indigo-500"
        />
      </div>

      {loading ? (
        <LoadingSpinner />
      ) : filtered.length === 0 ? (
        <EmptyState
          message={
            filter ? "No tickets match your filter" : "No tickets synced yet"
          }
        />
      ) : (
        <div className="rounded-lg border border-gray-200 bg-white overflow-hidden">
          <table className="w-full text-sm">
            <thead className="bg-gray-50 text-left text-xs text-gray-500 uppercase">
              <tr>
                <th className="px-4 py-2">Repo</th>
                <th className="px-4 py-2">#</th>
                <th className="px-4 py-2">Title</th>
                <th className="px-4 py-2">State</th>
                <th className="px-4 py-2">Labels</th>
                <th className="px-4 py-2">Assignee</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-gray-100">
              {filtered.map((t) => {
                const labels = parseLabels(t.labels);
                const repo = repoMap[t.repo_id];
                return (
                  <tr
                    key={t.id}
                    className="hover:bg-gray-50 cursor-pointer"
                    onClick={() => setSelected(t)}
                  >
                    <td className="px-4 py-2 text-gray-500">
                      {repo?.slug ?? "—"}
                    </td>
                    <td className="px-4 py-2">
                      <a
                        href={t.url}
                        target="_blank"
                        rel="noopener noreferrer"
                        className="text-indigo-600 hover:underline"
                        onClick={(e) => e.stopPropagation()}
                      >
                        {t.source_id}
                      </a>
                    </td>
                    <td className="px-4 py-2 text-gray-900">{t.title}</td>
                    <td className="px-4 py-2">
                      <StatusBadge status={t.state} />
                    </td>
                    <td className="px-4 py-2">
                      <div className="flex flex-wrap gap-1">
                        {labels.map((l) => (
                          <span
                            key={l}
                            className="px-1.5 py-0.5 text-xs rounded bg-gray-100 text-gray-600"
                          >
                            {l}
                          </span>
                        ))}
                      </div>
                    </td>
                    <td className="px-4 py-2 text-xs text-gray-500">
                      {t.assignee ?? "—"}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}

      {selected && (
        <TicketDetailModal
          ticket={selected}
          repoSlug={repoMap[selected.repo_id]?.slug ?? "Unknown"}
          onClose={() => setSelected(null)}
        />
      )}
    </div>
  );
}
