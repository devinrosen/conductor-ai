import type { ReactNode } from "react";
import {
  EmptyPlatform,
  ParallelTracks,
  ClosedTicketWindow,
  QuietRoundhouse,
  BlankDepartureBoard,
} from "./RailwayIllustrations";

/** Map keywords in the message to an illustration. */
function pickIllustration(message: string): ReactNode | null {
  const m = message.toLowerCase();
  if (m.includes("station") || m.includes("repo")) return <EmptyPlatform />;
  if (m.includes("platform") || m.includes("worktree") || m.includes("track")) return <ParallelTracks />;
  if (m.includes("ticket") || m.includes("window") || m.includes("issued")) return <ClosedTicketWindow />;
  if (m.includes("engine") || m.includes("agent") || m.includes("locomotive")) return <QuietRoundhouse />;
  if (m.includes("timetable") || m.includes("workflow") || m.includes("run")) return <BlankDepartureBoard />;
  if (m.includes("filter") || m.includes("match")) return null;
  return null;
}

export function EmptyState({ message }: { message: string }) {
  const illustration = pickIllustration(message);
  return (
    <div className="flex flex-col items-center justify-center py-10 text-gray-400 text-sm gap-3">
      {illustration}
      <p>{message}</p>
    </div>
  );
}
