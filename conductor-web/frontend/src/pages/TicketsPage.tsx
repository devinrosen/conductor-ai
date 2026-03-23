import { useMemo, useState, useRef, useCallback } from "react";
import { useRepos } from "../components/layout/AppShell";
import { useApi } from "../hooks/useApi";
import { api } from "../api/client";
import { LoadingSpinner } from "../components/shared/LoadingSpinner";
import { EmptyState } from "../components/shared/EmptyState";
import { TicketRow } from "../components/tickets/TicketRow";
import { TicketDetailModal } from "../components/tickets/TicketDetailModal";
import type { Ticket, Repo } from "../api/types";
import { parseLabels, buildLabelColorMap, labelTextColor } from "../utils/ticketUtils";
import { useHotkeys } from "../hooks/useHotkeys";
import { useListNav } from "../hooks/useListNav";

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
  const [sortCol, setSortCol] = useState<string | null>(null);
  const [sortDir, setSortDir] = useState<"asc" | "desc">("asc");
  const [stateFilter, setStateFilter] = useState<Set<string>>(new Set());
  const [assigneeFilter, setAssigneeFilter] = useState<Set<string>>(new Set());
  const [openFilterCol, setOpenFilterCol] = useState<string | null>(null);

  const repoMap = useMemo(() => {
    const map: Record<string, Repo> = {};
    for (const r of repos) map[r.id] = r;
    return map;
  }, [repos]);

  const labelColorMap = useMemo(
    () => buildLabelColorMap(allLabels ?? []),
    [allLabels],
  );

  const allLabelNames = useMemo(() => {
    const names = new Set<string>();
    for (const key of Object.keys(labelColorMap)) names.add(key);
    return Array.from(names).sort();
  }, [labelColorMap]);

  // Unique values for column filter dropdowns
  const uniqueStates = useMemo(() => {
    if (!tickets) return [];
    return [...new Set(tickets.map((t) => t.state))].sort();
  }, [tickets]);

  const uniqueAssignees = useMemo(() => {
    if (!tickets) return [];
    return [...new Set(tickets.map((t) => t.assignee).filter(Boolean) as string[])].sort();
  }, [tickets]);

  // Filter + sort
  const filtered = useMemo(() => {
    if (!tickets) return [];
    let result = tickets;

    // Text search
    const q = filter.toLowerCase().trim();
    if (q) {
      result = result.filter((t) =>
        t.title.toLowerCase().includes(q) ||
        t.source_id.toLowerCase().includes(q) ||
        (t.labels && t.labels.toLowerCase().includes(q)) ||
        (t.assignee && t.assignee.toLowerCase().includes(q))
      );
    }

    // Label chip filter (AND — must have ALL selected)
    if (selectedLabels.size > 0) {
      result = result.filter((t) => {
        const tLabels = new Set(parseLabels(t.labels));
        for (const l of selectedLabels) {
          if (!tLabels.has(l)) return false;
        }
        return true;
      });
    }

    // Column filters
    if (stateFilter.size > 0) {
      result = result.filter((t) => stateFilter.has(t.state));
    }
    if (assigneeFilter.size > 0) {
      result = result.filter((t) => t.assignee && assigneeFilter.has(t.assignee));
    }

    // Sort
    if (sortCol) {
      result = [...result].sort((a, b) => {
        let va = "", vb = "";
        switch (sortCol) {
          case "repo": va = repoMap[a.repo_id]?.slug ?? ""; vb = repoMap[b.repo_id]?.slug ?? ""; break;
          case "#": va = a.source_id; vb = b.source_id; break;
          case "title": va = a.title; vb = b.title; break;
          case "state": va = a.state; vb = b.state; break;
          case "labels": va = a.labels ?? ""; vb = b.labels ?? ""; break;
          case "assignee": va = a.assignee ?? ""; vb = b.assignee ?? ""; break;
        }
        const cmp = va.localeCompare(vb);
        return sortDir === "asc" ? cmp : -cmp;
      });
    }

    return result;
  }, [tickets, filter, selectedLabels, stateFilter, assigneeFilter, sortCol, sortDir, repoMap]);

  const activeFilterCount = stateFilter.size + assigneeFilter.size + selectedLabels.size + (filter ? 1 : 0);

  const toggleSort = useCallback((col: string) => {
    if (sortCol === col) {
      setSortDir((d) => d === "asc" ? "desc" : "asc");
    } else {
      setSortCol(col);
      setSortDir("asc");
    }
  }, [sortCol]);

  const toggleFilterValue = useCallback((setter: React.Dispatch<React.SetStateAction<Set<string>>>, value: string) => {
    setter((prev) => {
      const next = new Set(prev);
      if (next.has(value)) next.delete(value); else next.add(value);
      return next;
    });
  }, []);

  const clearAll = useCallback(() => {
    setFilter("");
    setSelectedLabels(new Set());
    setStateFilter(new Set());
    setAssigneeFilter(new Set());
  }, []);

  const { selectedIndex, moveDown, moveUp, reset } = useListNav(filtered.length);
  const focusFilter = useCallback(() => filterRef.current?.focus(), []);
  const openSelected = useCallback(() => {
    if (selectedIndex >= 0 && filtered[selectedIndex]) setSelected(filtered[selectedIndex]);
  }, [selectedIndex, filtered]);

  const toggleLabel = useCallback((label: string) => {
    setSelectedLabels((prev) => {
      const next = new Set(prev);
      if (next.has(label)) next.delete(label); else next.add(label);
      return next;
    });
  }, []);

  const handleEscape = useCallback(() => {
    if (selected) { setSelected(null); }
    else if (filter) { setFilter(""); filterRef.current?.blur(); }
    else if (selectedLabels.size > 0) { setSelectedLabels(new Set()); }
    else if (selectedIndex >= 0) { reset(); }
  }, [selected, filter, selectedLabels, selectedIndex, reset]);

  useHotkeys([
    { key: "/", handler: focusFilter, description: "Focus search" },
    { key: "j", handler: moveDown, description: "Next ticket" },
    { key: "k", handler: moveUp, description: "Previous ticket" },
    { key: "Enter", handler: openSelected, description: "Open selected ticket", enabled: selectedIndex >= 0 && !selected },
    { key: "Escape", handler: handleEscape, description: "Close / clear" },
  ]);

  const sortArrow = (col: string) => sortCol === col ? (sortDir === "asc" ? "\u25B2" : "\u25BC") : null;

  return (
    <div className="space-y-3">
      <div className="flex flex-col sm:flex-row sm:items-center sm:justify-between gap-2">
        <h2 className="text-lg font-bold text-gray-900">
          Tickets {tickets ? `(${filtered.length}${activeFilterCount > 0 ? ` of ${tickets.length}` : ""})` : ""}
        </h2>
        <div className="flex items-center gap-2">
          <button
            onClick={() => setShowClosed((v) => !v)}
            className={`px-3 py-1.5 text-sm rounded-md border ${
              showClosed
                ? "border-indigo-300 text-indigo-700 bg-indigo-50 hover:bg-indigo-100"
                : "border-gray-300 text-gray-600 hover:bg-gray-50"
            }`}
          >
            {showClosed ? "Hiding open only" : "Show closed"}
          </button>
        </div>
      </div>

      {/* Search + label chips */}
      <div className="flex items-center gap-2">
        <input
          ref={filterRef}
          type="text"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          placeholder="Search tickets..."
          className="flex-1 sm:max-w-sm px-3 py-1.5 text-sm rounded-md border border-gray-200 bg-white placeholder-gray-400 focus:outline-none focus:ring-1 focus:ring-indigo-500"
        />
        {activeFilterCount > 0 && (
          <button onClick={clearAll} className="px-2 py-1.5 text-xs rounded-md text-gray-400 hover:text-gray-600">
            Clear all filters
          </button>
        )}
      </div>

      {allLabelNames.length > 0 && (
        <div className="flex flex-wrap gap-1.5 items-center">
          <span className="text-xs text-gray-400 mr-1">Labels:</span>
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
                    : undefined
                }
              >
                {label}
              </button>
            );
          })}
        </div>
      )}

      {loading ? (
        <LoadingSpinner />
      ) : filtered.length === 0 ? (
        <EmptyState
          message={activeFilterCount > 0 ? "No tickets match your filter." : "No tickets issued. Sync your issues to start the journey."}
        />
      ) : (
        <div className="rounded-lg border border-gray-200 bg-white overflow-hidden max-h-[70vh] overflow-y-auto">
          <table className="w-full text-sm min-w-[560px]">
            <thead className="bg-gray-50 text-left text-xs text-gray-500 uppercase sticky top-0 z-10">
              <tr>
                <th className="px-3 py-1.5">
                  <button onClick={() => toggleSort("repo")} className="hover:text-gray-800 flex items-center gap-1">
                    Repo {sortArrow("repo") && <span>{sortArrow("repo")}</span>}
                  </button>
                </th>
                <th className="px-3 py-1.5">
                  <button onClick={() => toggleSort("#")} className="hover:text-gray-800 flex items-center gap-1">
                    # {sortArrow("#") && <span>{sortArrow("#")}</span>}
                  </button>
                </th>
                <th className="px-3 py-1.5">
                  <button onClick={() => toggleSort("title")} className="hover:text-gray-800 flex items-center gap-1">
                    Title {sortArrow("title") && <span>{sortArrow("title")}</span>}
                  </button>
                </th>
                <th className="px-3 py-1.5">
                  <div className="flex items-center gap-1">
                    <button onClick={() => toggleSort("state")} className="hover:text-gray-800 flex items-center gap-1">
                      State {sortArrow("state") && <span>{sortArrow("state")}</span>}
                    </button>
                    <div className="relative">
                      <button
                        onClick={() => setOpenFilterCol(openFilterCol === "state" ? null : "state")}
                        className={`text-[10px] px-1 rounded ${stateFilter.size > 0 ? "text-indigo-500" : "text-gray-400 hover:text-gray-600"}`}
                      >
                        {stateFilter.size > 0 ? `(${stateFilter.size})` : "\u25BE"}
                      </button>
                      {openFilterCol === "state" && (
                        <div className="absolute left-0 top-6 w-36 p-2 rounded-lg border border-gray-200 bg-white shadow-lg z-30 space-y-1">
                          {uniqueStates.map((v) => (
                            <label key={v} className="flex items-center gap-2 text-xs text-gray-700 cursor-pointer hover:bg-gray-50 px-1 py-0.5 rounded">
                              <input type="checkbox" checked={stateFilter.has(v)} onChange={() => toggleFilterValue(setStateFilter, v)} className="rounded" />
                              {v}
                            </label>
                          ))}
                          {stateFilter.size > 0 && (
                            <button onClick={() => setStateFilter(new Set())} className="text-[10px] text-gray-400 hover:text-gray-600 w-full text-left px-1">Clear</button>
                          )}
                        </div>
                      )}
                    </div>
                  </div>
                </th>
                <th className="px-3 py-1.5">
                  <button onClick={() => toggleSort("labels")} className="hover:text-gray-800 flex items-center gap-1">
                    Labels {sortArrow("labels") && <span>{sortArrow("labels")}</span>}
                  </button>
                </th>
                <th className="px-3 py-1.5">
                  <div className="flex items-center gap-1">
                    <button onClick={() => toggleSort("assignee")} className="hover:text-gray-800 flex items-center gap-1">
                      Assignee {sortArrow("assignee") && <span>{sortArrow("assignee")}</span>}
                    </button>
                    <div className="relative">
                      <button
                        onClick={() => setOpenFilterCol(openFilterCol === "assignee" ? null : "assignee")}
                        className={`text-[10px] px-1 rounded ${assigneeFilter.size > 0 ? "text-indigo-500" : "text-gray-400 hover:text-gray-600"}`}
                      >
                        {assigneeFilter.size > 0 ? `(${assigneeFilter.size})` : "\u25BE"}
                      </button>
                      {openFilterCol === "assignee" && (
                        <div className="absolute right-0 top-6 w-40 max-h-48 overflow-y-auto p-2 rounded-lg border border-gray-200 bg-white shadow-lg z-30 space-y-1">
                          {uniqueAssignees.map((v) => (
                            <label key={v} className="flex items-center gap-2 text-xs text-gray-700 cursor-pointer hover:bg-gray-50 px-1 py-0.5 rounded">
                              <input type="checkbox" checked={assigneeFilter.has(v)} onChange={() => toggleFilterValue(setAssigneeFilter, v)} className="rounded" />
                              {v}
                            </label>
                          ))}
                          {assigneeFilter.size > 0 && (
                            <button onClick={() => setAssigneeFilter(new Set())} className="text-[10px] text-gray-400 hover:text-gray-600 w-full text-left px-1">Clear</button>
                          )}
                        </div>
                      )}
                    </div>
                  </div>
                </th>
                <th className="px-3 py-1.5">Agent</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-gray-100">
              {filtered.map((t, index) => (
                <TicketRow
                  key={t.id}
                  ticket={t}
                  repoSlug={repoMap[t.repo_id]?.slug ?? "\u2014"}
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
