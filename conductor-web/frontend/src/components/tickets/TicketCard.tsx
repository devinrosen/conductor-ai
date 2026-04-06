import type { Ticket, TicketAgentTotals } from "../../api/types";
import { StatusBadge } from "../shared/StatusBadge";
import { formatTicketTotalsFull } from "../../utils/agentStats";
import { parseLabels, labelTextColor } from "../../utils/ticketUtils";
import type { TreePosition } from "../../utils/ticketTree";

interface TicketCardProps {
  ticket: Ticket;
  agentTotals?: TicketAgentTotals;
  repoSlug?: string;
  onClick: (ticket: Ticket) => void;
  selected?: boolean;
  index?: number;
  labelColorMap?: Record<string, string>;
  treePosition?: TreePosition;
  blocked?: boolean;
}

export function TicketCard({ ticket, agentTotals, repoSlug, onClick, selected, index, labelColorMap, treePosition, blocked }: TicketCardProps) {
  const labels = parseLabels(ticket.labels);
  const indentPx = treePosition ? treePosition.depth * 20 : 0;
  return (
    <div
      className={`rounded-lg border p-3 cursor-pointer hover:bg-gray-50 ${
        selected
          ? "bg-indigo-50 ring-1 ring-inset ring-indigo-200 border-indigo-200"
          : "border-gray-200 bg-white"
      }`}
      style={indentPx > 0 ? { marginLeft: indentPx } : undefined}
      onClick={() => onClick(ticket)}
      data-list-index={index}
    >
      <div className="flex items-center gap-2 text-sm">
        {repoSlug !== undefined && (
          <span className="text-gray-500">{repoSlug}</span>
        )}
        <span className="text-indigo-600 font-medium">{ticket.source_id}</span>
        {blocked && (
          <span className="text-red-500 text-xs" title="Blocked by open ticket">&#x1F512;</span>
        )}
        <StatusBadge status={ticket.state} />
      </div>
      <p className="mt-1 text-sm text-gray-900">{ticket.title}</p>
      {labels.length > 0 && (
        <div className="mt-2 flex flex-wrap gap-1">
          {labels.map((l) => {
            const bg = labelColorMap?.[l];
            return bg ? (
              <span
                key={l}
                className="px-1.5 py-0.5 text-xs rounded"
                style={{ backgroundColor: bg, color: labelTextColor(bg) }}
              >
                {l}
              </span>
            ) : (
              <span
                key={l}
                className="px-1.5 py-0.5 text-xs rounded bg-gray-100 text-gray-600"
              >
                {l}
              </span>
            );
          })}
        </div>
      )}
      <div className="mt-2 flex items-center gap-3 text-xs text-gray-500">
        {ticket.assignee && <span>{ticket.assignee}</span>}
        {agentTotals && (
          <span className="text-purple-600">{formatTicketTotalsFull(agentTotals)}</span>
        )}
      </div>
    </div>
  );
}
