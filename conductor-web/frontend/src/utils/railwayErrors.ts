/**
 * Maps generic error messages to railway-themed equivalents.
 *
 * Used as a pass-through: if no mapping matches, returns the original message.
 */

const patterns: [RegExp, string][] = [
  [/failed to fetch|network|connection|couldn't reach/i, "Signal lost \u2014 check your connection and try again."],
  [/permission denied|forbidden|403/i, "End of the line \u2014 you don\u2019t have permission."],
  [/not found|404/i, "Wrong platform \u2014 that resource doesn\u2019t exist."],
  [/timeout|timed out|took too long/i, "Running behind schedule \u2014 the request took too long."],
  [/conflict|merge conflict/i, "Track conflict \u2014 changes have diverged. Resolve before proceeding."],
  [/failed to register repo/i, "Couldn\u2019t add this station. Check the URL and try again."],
  [/failed to start agent/i, "Engine wouldn\u2019t start. Check your configuration."],
  [/failed to start workflow/i, "Couldn\u2019t depart \u2014 workflow failed to start."],
  [/failed to start orchestration/i, "Couldn\u2019t dispatch trains \u2014 orchestration failed to start."],
  [/failed to stop agent/i, "Emergency brake failed. The agent may still be running."],
  [/failed to load/i, "Signal box isn\u2019t responding. Try again."],
  [/failed to save/i, "Couldn\u2019t update the logbook. Try again."],
  [/failed to sync/i, "Ticket office is closed. Sync failed."],
  [/failed to approve/i, "Signal jammed \u2014 couldn\u2019t approve the gate."],
  [/failed to reject/i, "Signal jammed \u2014 couldn\u2019t reject the gate."],
  [/failed to submit/i, "Message didn\u2019t reach the signal box. Try again."],
  [/failed to dismiss/i, "Couldn\u2019t clear the signal. Try again."],
  [/failed to delete|failed to remove/i, "Couldn\u2019t decommission. Try again."],
  [/failed to push/i, "Departure delayed \u2014 push failed."],
  [/failed to create pr/i, "Couldn\u2019t issue a departure notice. PR creation failed."],
];

export function railwayError(message: string): string {
  for (const [pattern, replacement] of patterns) {
    if (pattern.test(message)) return replacement;
  }
  return message;
}
