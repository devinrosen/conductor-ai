import { useMemo, useState, useRef, useCallback } from "react";
import { useRepos } from "../components/layout/AppShell";
import { useApi } from "../hooks/useApi";
import { api } from "../api/client";
import { LoadingSpinner } from "../components/shared/LoadingSpinner";
import { EmptyState } from "../components/shared/EmptyState";
import { TicketRow } from "../components/tickets/TicketRow";
import { TicketCard } from "../components/tickets/TicketCard";
import { TicketDetailModal } from "../components/tickets/TicketDetailModal";
import { ColumnHeader, type SortDirection } from "../components/shared/ColumnHeader";
import type { Ticket, Repo } from "../api/types";
import { parseLabels, buildLabelColorMap } from "../utils/ticketUtils";
import { useHotkeys } from "../hooks/useHotkeys";
import { useListNav } from "../hooks/useListNav";

type SortColumn = "repo" | "source_id" | "title" | "state" | "assignee" | "pipeline" | null;

function getPipelineStatus(ticket: Ticket): string {
  try {
    return JSON.parse(ticket.raw_json)?.conductor?.status ?? "";
  } catch {
    return "";
  }
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
  const [selected, setSelected] = useState<Ticket | null>(null);
  const filterRef = useRef<HTMLInputElement>(null);

  // Sort state
  const [sortColumn, setSortColumn] = useState<SortColumn>(null);
  const [sortDir, setSortDir] = useState<SortDirection>(null);

  // Per-column filters
  const [columnFilters, setColumnFilters] = useState<Record<string, Set<string>>>({});

  const repoMap = useMemo(() => {
    const map: Record<string, Repo> = {};
    for (const r of repos) map[r.id] = r;
    return map;
  }, [repos]);

  const labelColorMap = useMemo(
    () => buildLabelColorMap(allLabels ?? []),
    [allLabels],
  );

  const hasVantage = useMemo(
    () => tickets?.some((t) => t.source_type === "vantage") ?? false,
    [tickets],
  );

  const allVantage = useMemo(
    () => (tickets?.length ?? 0) > 0 && tickets!.every((t) => t.source_type === "vantage"),
    [tickets],
  );

  // Compute filter options (unique values per column)
  const filterOptionsMap = useMemo(() => {
    if (!tickets) return {};
    const repoSlugs = new Set<string>();
    const states = new Set<string>();
    const assignees = new Set<string>();
    const labels = new Set<string>();
    const pipelines = new Set<string>();
    for (const t of tickets) {
      const slug = repoMap[t.repo_id]?.slug;
      if (slug) repoSlugs.add(slug);
      states.add(t.state);
      if (t.assignee) assignees.add(t.assignee);
      for (const l of parseLabels(t.labels)) labels.add(l);
      const ps = getPipelineStatus(t);
      if (ps) pipelines.add(ps);
    }
    return {
      repo: Array.from(repoSlugs).sort(),
      state: Array.from(states).sort(),
      assignee: Array.from(assignees).sort(),
      labels: Array.from(labels).sort(),
      pipeline: Array.from(pipelines).sort(),
    };
  }, [tickets, repoMap]);

  const filtered = useMemo(() => {
    if (!tickets) return [];
    let result = tickets;

    // Text search
    const trimmed = filter.trim().toLowerCase();
    if (trimmed) {
      result = result.filter((t) =>
        t.title.toLowerCase().includes(trimmed) ||
        t.source_id.toLowerCase().includes(trimmed) ||
        (t.assignee?.toLowerCase().includes(trimmed) ?? false)
      );
    }

    // Column filters
    for (const [col, values] of Object.entries(columnFilters)) {
      if (values.size === 0) continue;
      result = result.filter((t) => {
        switch (col) {
          case "repo": return values.has(repoMap[t.repo_id]?.slug ?? "");
          case "state": return values.has(t.state);
          case "assignee": return values.has(t.assignee ?? "");
          case "labels": return parseLabels(t.labels).some((l) => values.has(l));
          case "pipeline": return values.has(getPipelineStatus(t));
          default: return true;
        }
      });
    }

    // Sort
    if (sortColumn && sortDir) {
      const dir = sortDir === "asc" ? 1 : -1;
      result = [...result].sort((a, b) => {
        let va = "";
        let vb = "";
        switch (sortColumn) {
          case "repo": va = repoMap[a.repo_id]?.slug ?? ""; vb = repoMap[b.repo_id]?.slug ?? ""; break;
          case "source_id": va = a.source_id; vb = b.source_id; break;
          case "title": va = a.title; vb = b.title; break;
          case "state": va = a.state; vb = b.state; break;
          case "assignee": va = a.assignee ?? ""; vb = b.assignee ?? ""; break;
          case "pipeline": va = getPipelineStatus(a); vb = getPipelineStatus(b); break;
        }
        return va.localeCompare(vb) * dir;
      });
    }

    return result;
  }, [tickets, filter, columnFilters, sortColumn, sortDir, repoMap]);

  const { selectedIndex, moveDown, moveUp, reset } = useListNav(filtered.length);

  const focusFilter = useCallback(() => filterRef.current?.focus(), []);

  const openSelected = useCallback(() => {
    if (selectedIndex >= 0 && filtered[selectedIndex]) {
      setSelected(filtered[selectedIndex]);
    }
  }, [selectedIndex, filtered]);

  const hasActiveFilters = Object.values(columnFilters).some((s) => s.size > 0);

  const handleEscape = useCallback(() => {
    if (selected) {
      setSelected(null);
    } else if (filter) {
      setFilter("");
      filterRef.current?.blur();
    } else if (hasActiveFilters) {
      setColumnFilters({});
    } else if (selectedIndex >= 0) {
      reset();
    }
  }, [selected, filter, hasActiveFilters, selectedIndex, reset]);

  useHotkeys([
    { key: "/", handler: focusFilter, description: "Focus search" },
    { key: "j", handler: moveDown, description: "Next ticket" },
    { key: "k", handler: moveUp, description: "Previous ticket" },
    { key: "Enter", handler: openSelected, description: "Open selected ticket", enabled: selectedIndex >= 0 && !selected },
    { key: "Escape", handler: handleEscape, description: "Close / clear" },
  ]);

  function handleSort(col: string, dir: SortDirection) {
    setSortColumn(dir ? (col as SortColumn) : null);
    setSortDir(dir);
  }

  function handleFilter(col: string, values: Set<string>) {
    setColumnFilters((prev) => ({ ...prev, [col]: values }));
  }

  function sortDirFor(col: string): SortDirection {
    return sortColumn === col ? sortDir : null;
  }

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
            placeholder="Search tickets..."
            className="w-full sm:w-80 px-3 py-2 text-sm rounded-md border border-gray-300 focus:outline-none focus:ring-2 focus:ring-indigo-500 focus:border-indigo-500"
          />
        </div>
      </div>

      {loading ? (
        <LoadingSpinner />
      ) : filtered.length === 0 ? (
        <EmptyState
          message={
            filter || hasActiveFilters ? "No tickets match your filter" : "No tickets issued. Sync your issues to start the journey."
          }
        />
      ) : (
        <>
          <div className="hidden md:block rounded-lg border border-gray-200 bg-white overflow-hidden overflow-x-auto">
            <table className="w-full text-sm min-w-[560px]">
              <thead className="bg-gray-50 text-left text-gray-500">
                <tr>
                  <ColumnHeader label="Repo" columnKey="repo" sortDirection={sortDirFor("repo")} onSort={handleSort} filterOptions={filterOptionsMap.repo} activeFilters={columnFilters.repo} onFilter={handleFilter} />
                  <th className="px-4 py-2 text-xs font-medium uppercase">#</th>
                  <th className="px-4 py-2 text-xs font-medium uppercase">Title</th>
                  {!allVantage && <ColumnHeader label="State" columnKey="state" sortDirection={sortDirFor("state")} onSort={handleSort} filterOptions={filterOptionsMap.state} activeFilters={columnFilters.state} onFilter={handleFilter} />}
                  {!allVantage && <ColumnHeader label="Labels" columnKey="labels" sortDirection={null} onSort={() => {}} filterOptions={filterOptionsMap.labels} activeFilters={columnFilters.labels} onFilter={handleFilter} />}
                  <ColumnHeader label="Assignee" columnKey="assignee" sortDirection={sortDirFor("assignee")} onSort={handleSort} filterOptions={filterOptionsMap.assignee} activeFilters={columnFilters.assignee} onFilter={handleFilter} />
                  {hasVantage && <ColumnHeader label="Pipeline" columnKey="pipeline" sortDirection={sortDirFor("pipeline")} onSort={handleSort} filterOptions={filterOptionsMap.pipeline} activeFilters={columnFilters.pipeline} onFilter={handleFilter} />}
                  <th className="px-4 py-2 text-xs font-medium uppercase">Agent</th>
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
                    showPipeline={hasVantage}
                    hideStateAndLabels={allVantage}
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
