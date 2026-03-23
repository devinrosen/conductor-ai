import type { Ticket, TicketAgentTotals } from "../../api/types";
import { StatusBadge } from "../shared/StatusBadge";
import { formatTicketTotalsFull } from "../../utils/agentStats";
import { parseLabels, labelTextColor } from "../../utils/ticketUtils";

interface TicketRowProps {
  ticket: Ticket;
  agentTotals?: TicketAgentTotals;
  repoSlug?: string;
  onClick: (ticket: Ticket) => void;
  selected?: boolean;
  index?: number;
  labelColorMap?: Record<string, string>;
}

export function TicketRow({ ticket, agentTotals, repoSlug, onClick, selected, index, labelColorMap }: TicketRowProps) {
  const labels = parseLabels(ticket.labels);
  return (
    <tr
      className={`cursor-pointer hover:bg-gray-50 ${selected ? "bg-indigo-50 ring-1 ring-inset ring-indigo-200" : ""}`}
      onClick={() => onClick(ticket)}
      data-list-index={index}
    >
      {repoSlug !== undefined && (
        <td className="px-3 py-1.5">
          <span className="inline-block px-1.5 py-0.5 text-[11px] font-mono rounded bg-gray-100 text-gray-600 truncate max-w-[100px]">
            {repoSlug}
          </span>
        </td>
      )}
      <td className="pl-1 pr-3 py-1.5">
        <div className="flex items-center gap-1.5">
          {/* Ticket notch */}
          <div className="w-1 h-3 rounded-r-full shrink-0" style={{ backgroundColor: "var(--color-gray-300)" }} />
          <span className="text-indigo-600">{ticket.source_id}</span>
        </div>
      </td>
      <td className="px-3 py-1.5 text-gray-900">{ticket.title}</td>
      <td className="px-3 py-1.5">
        <StatusBadge status={ticket.state} />
      </td>
      <td className="px-3 py-1.5">
        <div className="flex flex-wrap gap-1">
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
      </td>
      <td className="px-3 py-1.5 text-xs text-gray-500">
        {ticket.assignee ?? "-"}
      </td>
      <td className="px-3 py-1.5 text-xs text-purple-600 whitespace-nowrap">
        {agentTotals ? formatTicketTotalsFull(agentTotals) : ""}
      </td>
    </tr>
  );
}
