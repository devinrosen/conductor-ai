import { useEffect } from "react";
import type { Ticket, TicketDetail } from "../../api/types";
import { api } from "../../api/client";
import { useApi } from "../../hooks/useApi";
import { StatusBadge } from "../shared/StatusBadge";
import { parseLabels } from "../../utils/ticketUtils";
import { formatCostCompact, formatDuration } from "../../utils/agentStats";

interface TicketDetailModalProps {
  ticket: Ticket;
  onClose: () => void;
}

export function TicketDetailModal({ ticket, onClose }: TicketDetailModalProps) {
  const {
    data: detail,
    loading,
  } = useApi<TicketDetail>(
    () => api.getTicketDetail(ticket.id),
    [ticket.id],
  );

  useEffect(() => {
    function handleKeyDown(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
    }
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [onClose]);

  const labels = parseLabels(ticket.labels);
  const totals = detail?.agent_totals;
  const worktrees = detail?.worktrees ?? [];

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/40"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="bg-white rounded-lg shadow-lg w-full max-w-lg mx-4 max-h-[80vh] flex flex-col">
        {/* Header */}
        <div className="flex items-center justify-between px-6 pt-5 pb-3 border-b border-gray-100">
          <h3 className="text-lg font-semibold text-gray-900 truncate pr-4">
            #{ticket.source_id} {ticket.title}
          </h3>
          <button
            onClick={onClose}
            className="text-gray-400 hover:text-gray-600 text-xl leading-none"
          >
            &times;
          </button>
        </div>

        {/* Body */}
        <div className="px-6 py-4 overflow-y-auto space-y-5">
          {/* Ticket metadata */}
          <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-2 text-sm">
            <dt className="font-medium text-gray-500">State</dt>
            <dd><StatusBadge status={ticket.state} /></dd>

            <dt className="font-medium text-gray-500">Source</dt>
            <dd className="text-gray-900">{ticket.source_type} #{ticket.source_id}</dd>

            <dt className="font-medium text-gray-500">Assignee</dt>
            <dd className="text-gray-900">{ticket.assignee ?? "Unassigned"}</dd>

            <dt className="font-medium text-gray-500">Labels</dt>
            <dd>
              {labels.length > 0 ? (
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
              ) : (
                <span className="text-gray-400">None</span>
              )}
            </dd>

            <dt className="font-medium text-gray-500">URL</dt>
            <dd>
              {ticket.url ? (
                <a
                  href={ticket.url}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="text-indigo-600 hover:underline truncate block"
                >
                  {ticket.url}
                </a>
              ) : (
                <span className="text-gray-400">-</span>
              )}
            </dd>
          </dl>

          {/* Description */}
          {ticket.body && (
            <div>
              <h4 className="text-xs font-semibold uppercase tracking-wider text-gray-400 mb-1">
                Description
              </h4>
              <p className="text-sm text-gray-700 whitespace-pre-wrap break-words line-clamp-6">
                {ticket.body}
              </p>
            </div>
          )}

          {/* Agent Totals */}
          <div>
            <h4 className="text-xs font-semibold uppercase tracking-wider text-gray-400 mb-2">
              Agent Totals
            </h4>
            {loading ? (
              <p className="text-sm text-gray-400">Loading...</p>
            ) : totals ? (
              <div className="grid grid-cols-4 gap-3">
                <div className="rounded-md bg-gray-50 p-2 text-center">
                  <div className="text-base font-semibold text-gray-900">{totals.total_runs}</div>
                  <div className="text-xs text-gray-500">Runs</div>
                </div>
                <div className="rounded-md bg-gray-50 p-2 text-center">
                  <div className="text-base font-semibold text-fuchsia-700">{formatCostCompact(totals.total_cost)}</div>
                  <div className="text-xs text-gray-500">Cost</div>
                </div>
                <div className="rounded-md bg-gray-50 p-2 text-center">
                  <div className="text-base font-semibold text-gray-900">{totals.total_turns}</div>
                  <div className="text-xs text-gray-500">Turns</div>
                </div>
                <div className="rounded-md bg-gray-50 p-2 text-center">
                  <div className="text-base font-semibold text-gray-900">{formatDuration(totals.total_duration_ms)}</div>
                  <div className="text-xs text-gray-500">Duration</div>
                </div>
              </div>
            ) : (
              <p className="text-sm text-gray-400">No agent runs recorded</p>
            )}
          </div>

          {/* Linked Worktrees */}
          <div>
            <h4 className="text-xs font-semibold uppercase tracking-wider text-gray-400 mb-2">
              Linked Worktrees
            </h4>
            {loading ? (
              <p className="text-sm text-gray-400">Loading...</p>
            ) : worktrees.length > 0 ? (
              <ul className="space-y-1.5">
                {worktrees.map((wt) => (
                  <li
                    key={wt.id}
                    className="flex items-center gap-2 text-sm"
                  >
                    <span
                      className={`inline-block w-2 h-2 rounded-full ${
                        wt.status === "active"
                          ? "bg-green-500"
                          : wt.status === "merged"
                            ? "bg-blue-500"
                            : "bg-gray-400"
                      }`}
                    />
                    <span className="font-mono text-gray-900">{wt.slug}</span>
                    <span className="text-gray-400">{wt.branch}</span>
                    <StatusBadge status={wt.status} />
                  </li>
                ))}
              </ul>
            ) : (
              <p className="text-sm text-gray-400">No linked worktrees</p>
            )}
          </div>
        </div>

        {/* Footer */}
        <div className="px-6 py-3 border-t border-gray-100 flex justify-end">
          <button
            onClick={onClose}
            className="px-3 py-1.5 text-sm rounded-md border border-gray-300 text-gray-700 hover:bg-gray-50"
          >
            Close
          </button>
        </div>
      </div>
    </div>
  );
}
