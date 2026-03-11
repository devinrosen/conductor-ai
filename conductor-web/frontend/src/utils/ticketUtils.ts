import type { TicketLabel } from "../api/types";

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
