import { describe, it, expect } from "vitest";
import { isInteractiveRun, isStoppableRun } from "./agentStats";

describe("isStoppableRun", () => {
  it("allows headless running and waiting runs", () => {
    expect(isStoppableRun({ status: "running", runtime: "claude" })).toBe(true);
    expect(isStoppableRun({ status: "waiting_for_feedback" })).toBe(true);
  });

  it("never allows interactive runs", () => {
    for (const status of ["running", "waiting_for_feedback", "idle", "completed"]) {
      expect(isStoppableRun({ status, runtime: "interactive" })).toBe(false);
    }
  });

  it("excludes idle and terminal headless runs", () => {
    for (const status of ["idle", "completed", "failed", "cancelled"]) {
      expect(isStoppableRun({ status, runtime: "claude" })).toBe(false);
    }
  });
});

describe("isInteractiveRun", () => {
  it("detects the interactive runtime", () => {
    expect(isInteractiveRun({ runtime: "interactive" })).toBe(true);
    expect(isInteractiveRun({ runtime: "claude" })).toBe(false);
    expect(isInteractiveRun({})).toBe(false);
  });
});
