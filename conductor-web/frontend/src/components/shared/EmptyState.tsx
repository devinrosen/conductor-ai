import { isDesktop } from "../../api/transport";

/** Map common empty-state messages to railway-themed equivalents for desktop. */
const railwayMessages: Record<string, string> = {
  "No repos registered yet. Register one to get started.":
    "The station is quiet. Register a repo to get the trains running.",
  "No active worktrees":
    "No platforms active. Create a worktree to lay some track.",
  "No worktrees yet":
    "No platforms active. Create a worktree to lay some track.",
  "No tickets synced yet":
    "No tickets issued. Sync your issues to start the journey.",
  "No repos yet":
    "The station is quiet. Register a repo to get started.",
  "No workflow definitions found.":
    "No timetable set. Add .wf files to schedule your first route.",
  "No workflow runs yet.":
    "The engine house is still. Run a workflow to fire up the locomotive.",
  "No issue sources configured":
    "No ticket windows open. Configure an issue source to start boarding.",
};

export function EmptyState({ message }: { message: string }) {
  const display = isDesktop() ? (railwayMessages[message] ?? message) : message;
  return (
    <div className="flex items-center justify-center py-12 text-gray-400 text-sm">
      {display}
    </div>
  );
}
