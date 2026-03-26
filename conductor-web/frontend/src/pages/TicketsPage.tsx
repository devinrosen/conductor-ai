import { useMemo, useState, useRef, useCallback } from "react";
import { useRepos } from "../components/layout/AppShell";
import { useApi } from "../hooks/useApi";
import { api } from "../api/client";
import { LoadingSpinner } from "../components/shared/LoadingSpinner";
import { EmptyState } from "../components/shared/EmptyState";
import { TicketRow } from "../components/tickets/TicketRow";
import { TicketCard } from "../components/tickets/TicketCard";
import { TicketDetailModal } from "../components/tickets/TicketDetailModal";
import type { Ticket, Repo } from "../api/types";
import { parseLabels, buildLabelColorMap, labelTextColor } from "../utils/ticketUtils";
import { useHotkeys } from "../hooks/useHotkeys";
import { useListNav } from "../hooks/useListNav";

function matchesFilter(ticket: Ticket, filter: string, selectedLabels: Set<string>): boolean {
  // Text filter
  if (filter) {
    const lower = filter.toLowerCase();
    const textMatch =
      ticket.title.toLowerCase().includes(lower) ||
      ticket.source_id.toLowerCase().includes(lower) ||
      parseLabels(ticket.labels).some((l) => l.toLowerCase().includes(lower));
    if (!textMatch) return false;
  }
  // Label chip filter: ticket must have ALL selected labels
  if (selectedLabels.size > 0) {
    const ticketLabels = new Set(parseLabels(ticket.labels));
    for (const label of selectedLabels) {
      if (!ticketLabels.has(label)) return false;
    }
  }
  return true;
}

export function TicketsPage() {
  const { repos } = useRepos();
  const [showClosed, setShowClosed] = useState(false);
  const { data: tickets, loading } = useApi(
    () => api.listAllTickets(showClosed),
    [showClosed],
  );
  const { data: ticketTotals } = useApi(() => api.ticketAgentTotals(), []);
  const { data: allLabels } = useApi(() => api.ticketLabels(), []);
  const [filter, setFilter] = useState("");
  const [selectedLabels, setSelectedLabels] = useState<Set<string>>(new Set());
  const [selected, setSelected] = useState<Ticket | null>(null);
  const filterRef = useRef<HTMLInputElement>(null);

  const repoMap = useMemo(() => {
    const map: Record<string, Repo> = {};
    for (const r of repos) map[r.id] = r;
    return map;
  }, [repos]);

  const labelColorMap = useMemo(
    () => buildLabelColorMap(allLabels ?? []),
    [allLabels],
  );

  // Collect all unique label names for the chip filter row
  const allLabelNames = useMemo(() => {
    const names = new Set<string>();
    for (const key of Object.keys(labelColorMap)) {
      names.add(key);
    }
    return Array.from(names).sort();
  }, [labelColorMap]);

  const filtered = useMemo(() => {
    if (!tickets) return [];
    return tickets.filter((t) => matchesFilter(t, filter.trim(), selectedLabels));
  }, [tickets, filter, selectedLabels]);

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
    } else if (selectedLabels.size > 0) {
      setSelectedLabels(new Set());
    } else if (selectedIndex >= 0) {
      reset();
    }
  }, [selected, filter, selectedLabels, selectedIndex, reset]);

  const toggleLabel = useCallback((label: string) => {
    setSelectedLabels((prev) => {
      const next = new Set(prev);
      if (next.has(label)) {
        next.delete(label);
      } else {
        next.add(label);
      }
      return next;
    });
  }, []);

  useHotkeys([
    { key: "/", handler: focusFilter, description: "Focus search" },
    { key: "j", handler: moveDown, description: "Next ticket" },
    { key: "k", handler: moveUp, description: "Previous ticket" },
    { key: "Enter", handler: openSelected, description: "Open selected ticket", enabled: selectedIndex >= 0 && !selected },
    { key: "Escape", handler: handleEscape, description: "Close / clear" },
  ]);

  return (
    <div className="space-y-4">
      <div className="flex flex-col sm:flex-row sm:items-center sm:justify-between gap-3">
        <h2 className="text-xl font-bold text-gray-900">Tickets</h2>
        <div className="flex flex-col sm:flex-row sm:items-center gap-2">
          <button
            onClick={() => setShowClosed((v) => !v)}
            className={`px-3 py-2 text-sm rounded-md border ${
              showClosed
                ? "border-indigo-300 text-indigo-700 bg-indigo-50 hover:bg-indigo-100"
                : "border-gray-300 text-gray-600 hover:bg-gray-50"
            }`}
          >
            {showClosed ? "Hiding open only" : "Show closed"}
          </button>
          <input
            ref={filterRef}
            type="text"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            placeholder="Filter by title, ID, or label..."
            className="w-full sm:w-80 px-3 py-2 text-sm rounded-md border border-gray-300 focus:outline-none focus:ring-2 focus:ring-indigo-500 focus:border-indigo-500"
          />
        </div>
      </div>

      {/* Label chip filter */}
      {allLabelNames.length > 0 && (
        <div className="flex flex-wrap gap-1.5 items-center">
          <span className="text-xs text-gray-400 mr-1">Filter by label:</span>
          {allLabelNames.map((label) => {
            const bg = labelColorMap[label];
            const active = selectedLabels.has(label);
            return (
              <button
                key={label}
                onClick={() => toggleLabel(label)}
                className={`px-2 py-0.5 text-xs rounded border transition-all ${
                  active ? "ring-2 ring-offset-1 ring-indigo-400 opacity-100" : "opacity-70 hover:opacity-100"
                }`}
                style={
                  bg
                    ? { backgroundColor: bg, color: labelTextColor(bg), borderColor: bg }
                    : { backgroundColor: "#f3f4f6", color: "#4b5563", borderColor: "#e5e7eb" }
                }
              >
                {label}
              </button>
            );
          })}
          {selectedLabels.size > 0 && (
            <button
              onClick={() => setSelectedLabels(new Set())}
              className="px-2 py-0.5 text-xs rounded border border-gray-300 text-gray-500 hover:bg-gray-50"
            >
              Clear
            </button>
          )}
        </div>
      )}

      {loading ? (
        <LoadingSpinner />
      ) : filtered.length === 0 ? (
        <EmptyState
          message={
            filter || selectedLabels.size > 0 ? "No tickets match your filter" : "No tickets synced yet"
          }
        />
      ) : (
        <>
          <div className="hidden md:block rounded-lg border border-gray-200 bg-white overflow-hidden overflow-x-auto">
            <table className="w-full text-sm min-w-[560px]">
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
                    labelColorMap={labelColorMap}
                  />
                ))}
              </tbody>
            </table>
          </div>
          <div className="md:hidden space-y-2">
            {filtered.map((t, index) => (
              <TicketCard
                key={t.id}
                ticket={t}
                repoSlug={repoMap[t.repo_id]?.slug ?? "—"}
                agentTotals={ticketTotals?.[t.id]}
                onClick={setSelected}
                selected={index === selectedIndex}
                index={index}
                labelColorMap={labelColorMap}
              />
            ))}
          </div>
        </>
      )}

      {selected && (
        <TicketDetailModal
          ticket={selected}
          onClose={() => setSelected(null)}
          labelColorMap={labelColorMap}
        />
      )}
    </div>
  );
}
