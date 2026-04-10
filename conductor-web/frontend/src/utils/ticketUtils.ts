import type { Ticket, TicketLabel } from "../api/types";
import type { SortDirection } from "../components/shared/ColumnHeader";

/** Parse a JSON-encoded labels string into an array. */
export function parseLabels(raw: string): string[] {
  try {
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

/**
 * Build a map of label name → hex color from a flat list of TicketLabel rows.
 * Duplicate label names (across repos) are last-write-wins.
 */
export function buildLabelColorMap(labels: TicketLabel[]): Record<string, string> {
  const map: Record<string, string> = {};
  for (const l of labels) {
    if (l.color) {
      map[l.label] = l.color.startsWith("#") ? l.color : `#${l.color}`;
    }
  }
  return map;
}

/**
 * Extract the Vantage pipeline status from a ticket's raw_json.
 * Returns an empty string for non-Vantage tickets or malformed JSON.
 */
export function getPipelineStatus(ticket: Ticket): string {
  try {
    return JSON.parse(ticket.raw_json)?.conductor?.status ?? "";
  } catch {
    return "";
  }
}

/**
 * Apply per-column filters to a ticket list.
 *
 * @param getRepoSlug - maps a repo_id to its slug; pass `() => ""` when the
 *   "repo" column is not present.
 */
export function filterTicketsByColumns(
  tickets: Ticket[],
  columnFilters: Record<string, Set<string>>,
  getRepoSlug: (repoId: string) => string,
): Ticket[] {
  let result = tickets;
  for (const [col, values] of Object.entries(columnFilters)) {
    if (values.size === 0) continue;
    result = result.filter((t) => {
      switch (col) {
        case "repo": return values.has(getRepoSlug(t.repo_id));
        case "state": return values.has(t.state);
        case "assignee": return values.has(t.assignee ?? "");
        case "labels": return parseLabels(t.labels).some((l) => values.has(l));
        case "pipeline": return values.has(getPipelineStatus(t));
        default: return true;
      }
    });
  }
  return result;
}

/**
 * Sort a ticket list by a column. Returns the original array reference when
 * no sort is active.
 *
 * @param getRepoSlug - maps a repo_id to its slug; pass `() => ""` when the
 *   "repo" column is not present.
 */
export function sortTickets(
  tickets: Ticket[],
  sortColumn: string | null,
  sortDir: SortDirection,
  getRepoSlug: (repoId: string) => string,
): Ticket[] {
  if (!sortColumn || !sortDir) return tickets;
  const dir = sortDir === "asc" ? 1 : -1;
  return [...tickets].sort((a, b) => {
    let va = "";
    let vb = "";
    switch (sortColumn) {
      case "repo": va = getRepoSlug(a.repo_id); vb = getRepoSlug(b.repo_id); break;
      case "source_id": va = a.source_id; vb = b.source_id; break;
      case "title": va = a.title; vb = b.title; break;
      case "state": va = a.state; vb = b.state; break;
      case "assignee": va = a.assignee ?? ""; vb = b.assignee ?? ""; break;
      case "pipeline": va = getPipelineStatus(a); vb = getPipelineStatus(b); break;
    }
    return va.localeCompare(vb) * dir;
  });
}

/**
 * Return "#ffffff" or "#000000" for maximum contrast against the given hex
 * background color, using the W3C perceived-luminance formula.
 */
export function labelTextColor(hex: string): "#ffffff" | "#000000" {
  const h = hex.replace("#", "");
  // Support 3-digit shorthand
  const full = h.length === 3
    ? h[0] + h[0] + h[1] + h[1] + h[2] + h[2]
    : h;
  const r = parseInt(full.slice(0, 2), 16);
  const g = parseInt(full.slice(2, 4), 16);
  const b = parseInt(full.slice(4, 6), 16);
  if (isNaN(r) || isNaN(g) || isNaN(b)) return "#000000";
  // Perceived luminance (0–255 scale)
  const luminance = 0.299 * r + 0.587 * g + 0.114 * b;
  return luminance > 128 ? "#000000" : "#ffffff";
}
