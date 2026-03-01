import type { Ticket, TicketAgentTotals } from "../../api/types";
import { StatusBadge } from "../shared/StatusBadge";
import { formatTicketTotalsFull } from "../../utils/agentStats";
import { parseLabels } from "../../utils/ticketUtils";

interface TicketRowProps {
  ticket: Ticket;
  agentTotals?: TicketAgentTotals;
  repoSlug?: string;
  onClick: (ticket: Ticket) => void;
  selected?: boolean;
  index?: number;
}

export function TicketRow({ ticket, agentTotals, repoSlug, onClick, selected, index }: TicketRowProps) {
  const labels = parseLabels(ticket.labels);
  return (
    <tr
      className={`cursor-pointer hover:bg-gray-50 ${selected ? "bg-indigo-50 ring-1 ring-inset ring-indigo-200" : ""}`}
      onClick={() => onClick(ticket)}
      data-list-index={index}
    >
      {repoSlug !== undefined && (
        <td className="px-4 py-2 text-gray-500">{repoSlug}</td>
      )}
      <td className="px-4 py-2">
        <span className="text-indigo-600">{ticket.source_id}</span>
      </td>
      <td className="px-4 py-2 text-gray-900">{ticket.title}</td>
      <td className="px-4 py-2">
        <StatusBadge status={ticket.state} />
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
        {ticket.assignee ?? "-"}
      </td>
      <td className="px-4 py-2 text-xs text-purple-600 whitespace-nowrap">
        {agentTotals ? formatTicketTotalsFull(agentTotals) : ""}
      </td>
    </tr>
  );
}
