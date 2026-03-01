import { useMemo, useState, useRef, useCallback } from "react";
import { useRepos } from "../components/layout/AppShell";
import { useApi } from "../hooks/useApi";
import { api } from "../api/client";
import { LoadingSpinner } from "../components/shared/LoadingSpinner";
import { EmptyState } from "../components/shared/EmptyState";
import { TicketRow } from "../components/tickets/TicketRow";
import { TicketDetailModal } from "../components/tickets/TicketDetailModal";
import type { Ticket, Repo } from "../api/types";
import { parseLabels } from "../utils/ticketUtils";
import { useHotkeys } from "../hooks/useHotkeys";
import { useListNav } from "../hooks/useListNav";

function matchesFilter(ticket: Ticket, filter: string): boolean {
  const lower = filter.toLowerCase();
  if (ticket.title.toLowerCase().includes(lower)) return true;
  if (ticket.source_id.toLowerCase().includes(lower)) return true;
  const labels = parseLabels(ticket.labels);
  if (labels.some((l) => l.toLowerCase().includes(lower))) return true;
  return false;
}

export function TicketsPage() {
  const { repos } = useRepos();
  const { data: tickets, loading } = useApi(() => api.listAllTickets(), []);
  const { data: ticketTotals } = useApi(() => api.ticketAgentTotals(), []);
  const [filter, setFilter] = useState("");
  const [selected, setSelected] = useState<Ticket | null>(null);
  const filterRef = useRef<HTMLInputElement>(null);

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

  const { selectedIndex, moveDown, moveUp, reset } = useListNav(filtered.length);

  const focusFilter = useCallback(() => filterRef.current?.focus(), []);

  const openSelected = useCallback(() => {
    if (selectedIndex >= 0 && filtered[selectedIndex]) {
      setSelected(filtered[selectedIndex]);
    }
  }, [selectedIndex, filtered]);

  const handleEscape = useCallback(() => {
    if (selected) {
      setSelected(null);
    } else if (filter) {
      setFilter("");
      filterRef.current?.blur();
    } else if (selectedIndex >= 0) {
      reset();
    }
  }, [selected, filter, selectedIndex, reset]);

  useHotkeys([
    { key: "/", handler: focusFilter, description: "Focus search" },
    { key: "j", handler: moveDown, description: "Next ticket" },
    { key: "k", handler: moveUp, description: "Previous ticket" },
    { key: "Enter", handler: openSelected, description: "Open selected ticket", enabled: selectedIndex >= 0 && !selected },
    { key: "Escape", handler: handleEscape, description: "Close / clear" },
  ]);

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h2 className="text-xl font-bold text-gray-900">Tickets</h2>
        <input
          ref={filterRef}
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
                <th className="px-4 py-2">Agent</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-gray-100">
              {filtered.map((t, index) => (
                <TicketRow
                  key={t.id}
                  ticket={t}
                  repoSlug={repoMap[t.repo_id]?.slug ?? "—"}
                  agentTotals={ticketTotals?.[t.id]}
                  onClick={setSelected}
                  selected={index === selectedIndex}
                  index={index}
                />
              ))}
            </tbody>
          </table>
        </div>
      )}

      {selected && (
        <TicketDetailModal
          ticket={selected}
          onClose={() => setSelected(null)}
        />
      )}
    </div>
  );
}
