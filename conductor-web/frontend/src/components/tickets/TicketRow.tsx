import type { Ticket } from "../../api/types";
import { StatusBadge } from "../shared/StatusBadge";

function parseLabels(raw: string): string[] {
  try {
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

export function TicketRow({ ticket }: { ticket: Ticket }) {
  const labels = parseLabels(ticket.labels);
  return (
    <tr>
      <td className="px-4 py-2">
        <a
          href={ticket.url}
          target="_blank"
          rel="noopener noreferrer"
          className="text-indigo-600 hover:underline"
        >
          {ticket.source_id}
        </a>
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
    </tr>
  );
}
