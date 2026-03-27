import { useState } from "react";
import { Link } from "react-router";
import { useRepos } from "../components/layout/AppShell";

/**
 * Getting Started — onboarding + reference guide.
 *
 * Auto-shown on first launch (no repos). Always accessible from Settings / Help.
 * Three sections:
 *   1. What is Conductor — value prop and mental model
 *   2. Setup — three-stop route map (Add Repo → Create Worktree → Sync Tickets)
 *   3. Best Practices — ticket-to-PR workflow guide
 */

type Section = "welcome" | "setup" | "workflow";

function WelcomeSection() {
  return (
    <div className="space-y-6">
      <div>
        <h3 className="text-lg font-bold text-gray-900 mb-2">What is Conductor?</h3>
        <p className="text-sm text-gray-600 leading-relaxed">
          Conductor is a multi-repo orchestration tool that manages your git repos, worktrees,
          tickets, and AI agent runs from a single dashboard. Think of it as a train conductor&apos;s
          control room — you see every track, every platform, every schedule at a glance.
        </p>
      </div>

      <div className="grid gap-4 sm:grid-cols-3">
        <div className="rounded-lg border border-gray-200 p-4">
          <div className="text-2xl mb-2">🚉</div>
          <h4 className="text-sm font-semibold text-gray-800 mb-1">Stations (Repos)</h4>
          <p className="text-xs text-gray-500">
            Each registered repo is a station. Conductor tracks its branches,
            worktrees, tickets, and agent activity.
          </p>
        </div>
        <div className="rounded-lg border border-gray-200 p-4">
          <div className="text-2xl mb-2">🛤️</div>
          <h4 className="text-sm font-semibold text-gray-800 mb-1">Platforms (Worktrees)</h4>
          <p className="text-xs text-gray-500">
            Worktrees are isolated git working directories — like separate platforms at a station.
            Each one holds a branch for a specific feature or fix.
          </p>
        </div>
        <div className="rounded-lg border border-gray-200 p-4">
          <div className="text-2xl mb-2">🎫</div>
          <h4 className="text-sm font-semibold text-gray-800 mb-1">Tickets (Issues)</h4>
          <p className="text-xs text-gray-500">
            Sync issues from GitHub or Jira. Link tickets to worktrees so every
            branch has context about what it&apos;s solving.
          </p>
        </div>
      </div>

      <div className="grid gap-4 sm:grid-cols-2">
        <div className="rounded-lg border border-gray-200 p-4">
          <div className="text-2xl mb-2">🤖</div>
          <h4 className="text-sm font-semibold text-gray-800 mb-1">AI Agents</h4>
          <p className="text-xs text-gray-500">
            Launch Claude agents inside any worktree to write code, review PRs,
            or investigate issues. Agents run in tmux sessions with full repo context.
          </p>
        </div>
        <div className="rounded-lg border border-gray-200 p-4">
          <div className="text-2xl mb-2">📋</div>
          <h4 className="text-sm font-semibold text-gray-800 mb-1">Workflows</h4>
          <p className="text-xs text-gray-500">
            Multi-step workflow engine that chains agents, gates, and scripts.
            Automate ticket-to-PR, code review, and release processes.
          </p>
        </div>
      </div>
    </div>
  );
}

function SetupSection() {
  const { repos } = useRepos();
  const hasRepos = repos.length > 0;

  const stops = [
    {
      number: 1,
      title: "Add a Repo",
      subtitle: "Register your first station",
      description: "Tell Conductor about a git repository. It will detect the remote, set up the workspace directory, and start tracking branches.",
      command: "conductor repo register /path/to/your/repo",
      done: hasRepos,
      link: "/repos",
      linkText: "Go to Repos",
    },
    {
      number: 2,
      title: "Create a Worktree",
      subtitle: "Lay some track",
      description: "Create an isolated worktree for a feature or fix. Conductor handles the git branch, directory setup, and dependency installation automatically.",
      command: "conductor worktree create <repo-slug> <name>",
      done: false, // Would need worktree data to check
      link: hasRepos ? `/repos/${repos[0]?.id}` : "/repos",
      linkText: "Create Worktree",
    },
    {
      number: 3,
      title: "Sync Tickets",
      subtitle: "Get your passengers aboard",
      description: "Connect GitHub Issues or Jira and sync your tickets. Link a ticket to a worktree to give agents and workflows the full context of what they're working on.",
      command: "conductor ticket sync <repo-slug>",
      done: false,
      link: "/tickets",
      linkText: "View Tickets",
    },
  ];

  return (
    <div className="space-y-6">
      <div>
        <h3 className="text-lg font-bold text-gray-900 mb-2">Setup — Three Stops to Get Running</h3>
        <p className="text-sm text-gray-500">
          Complete these three steps to set up your Conductor workspace.
        </p>
      </div>

      {/* Route map */}
      <div className="flex items-center justify-center gap-0 py-4">
        {stops.map((stop, i) => (
          <div key={stop.number} className="flex items-center">
            {i > 0 && (
              <div className={`h-0.5 w-12 sm:w-20 ${stop.done || stops[i - 1].done ? "bg-green-500" : "bg-gray-300"}`} />
            )}
            <div className="flex flex-col items-center gap-1">
              <div className={`w-8 h-8 rounded-full flex items-center justify-center text-sm font-bold border-2 ${
                stop.done
                  ? "bg-green-500 border-green-500 text-white"
                  : "border-gray-300 text-gray-400"
              }`}>
                {stop.done ? "✓" : stop.number}
              </div>
              <span className="text-[10px] text-gray-500 text-center max-w-20">{stop.title}</span>
            </div>
          </div>
        ))}
      </div>

      {/* Stop cards */}
      <div className="space-y-3">
        {stops.map((stop) => (
          <div key={stop.number} className={`rounded-lg border p-4 ${stop.done ? "border-green-200 bg-green-50/30" : "border-gray-200"}`}>
            <div className="flex items-start justify-between gap-4">
              <div className="flex-1">
                <div className="flex items-center gap-2 mb-1">
                  <span className={`text-xs font-mono px-1.5 py-0.5 rounded ${stop.done ? "bg-green-100 text-green-700" : "bg-gray-100 text-gray-500"}`}>
                    Stop {stop.number}
                  </span>
                  <h4 className="text-sm font-semibold text-gray-800">{stop.title}</h4>
                  {stop.done && <span className="text-xs text-green-600">Done</span>}
                </div>
                <p className="text-xs text-gray-500 mb-2">{stop.description}</p>
                <code className="text-[11px] font-mono bg-gray-100 text-gray-600 px-2 py-1 rounded block">
                  {stop.command}
                </code>
              </div>
              <Link
                to={stop.link}
                className="shrink-0 px-3 py-1.5 text-xs rounded-md border border-indigo-300 text-indigo-600 hover:bg-indigo-50"
              >
                {stop.linkText}
              </Link>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

function WorkflowSection() {
  const steps = [
    {
      icon: "🎫",
      title: "Start with a ticket",
      description: "Every change should trace back to a ticket. Sync your issues from GitHub or Jira, then pick the one you want to work on.",
    },
    {
      icon: "🛤️",
      title: "Create a worktree",
      description: "Create a worktree linked to the ticket. This gives you an isolated branch with a clean working directory. Conductor auto-installs dependencies.",
    },
    {
      icon: "🤖",
      title: "Launch an agent",
      description: "Start a Claude agent in the worktree with the ticket as context. The agent can write code, run tests, and iterate based on your feedback.",
    },
    {
      icon: "📝",
      title: "Review & iterate",
      description: "Review the agent's changes. Use the feedback loop to refine — the agent sees your comments and adjusts. Use workflows to automate multi-step reviews.",
    },
    {
      icon: "🚀",
      title: "Push & create PR",
      description: "Push the branch and create a PR directly from Conductor. The worktree's ticket is automatically linked in the PR description.",
    },
    {
      icon: "✅",
      title: "Merge & clean up",
      description: "Once the PR is merged, mark the worktree as complete. Conductor cleans up the git worktree and branch automatically.",
    },
  ];

  return (
    <div className="space-y-6">
      <div>
        <h3 className="text-lg font-bold text-gray-900 mb-2">Ticket to PR — Best Practices</h3>
        <p className="text-sm text-gray-500">
          The recommended workflow for turning a ticket into a merged pull request using Conductor.
        </p>
      </div>

      <div className="space-y-1">
        {steps.map((step, i) => (
          <div key={i} className="flex gap-4 py-3">
            <div className="flex flex-col items-center shrink-0">
              <div className="text-xl">{step.icon}</div>
              {i < steps.length - 1 && (
                <div className="w-px flex-1 bg-gray-200 mt-1" />
              )}
            </div>
            <div className="pb-2">
              <h4 className="text-sm font-semibold text-gray-800">{step.title}</h4>
              <p className="text-xs text-gray-500 mt-0.5 leading-relaxed">{step.description}</p>
            </div>
          </div>
        ))}
      </div>

      <div className="rounded-lg border border-indigo-200 bg-indigo-50/30 p-4">
        <h4 className="text-sm font-semibold text-indigo-800 mb-2">Pro Tips</h4>
        <ul className="text-xs text-indigo-700 space-y-1.5">
          <li>• Use <code className="bg-indigo-100 px-1 rounded">Cmd+K</code> to quickly navigate between repos, worktrees, and workflows</li>
          <li>• Name worktrees with <code className="bg-indigo-100 px-1 rounded">feat-</code> or <code className="bg-indigo-100 px-1 rounded">fix-</code> prefixes — Conductor auto-normalizes to <code className="bg-indigo-100 px-1 rounded">feat/</code> branches</li>
          <li>• Set up workflow definitions in <code className="bg-indigo-100 px-1 rounded">.conductor/workflows/</code> to automate the full ticket-to-PR pipeline</li>
          <li>• Use the <code className="bg-indigo-100 px-1 rounded">iterate-pr</code> workflow for automated PR review cycles with parallel reviewer agents</li>
          <li>• Link tickets before launching agents — the ticket context dramatically improves agent output</li>
        </ul>
      </div>
    </div>
  );
}

export function GettingStartedPage() {
  const [section, setSection] = useState<Section>("welcome");

  const tabs: { key: Section; label: string }[] = [
    { key: "welcome", label: "What is Conductor?" },
    { key: "setup", label: "Setup" },
    { key: "workflow", label: "Ticket to PR" },
  ];

  return (
    <div className="space-y-6 max-w-3xl mx-auto">
      <div>
        <h2 className="text-xl font-bold text-gray-900">Getting Started</h2>
        <p className="text-sm text-gray-500 mt-1">Welcome aboard! Everything you need to start using Conductor.</p>
      </div>

      {/* Tab navigation */}
      <div className="flex gap-1 border-b border-gray-200">
        {tabs.map((tab) => (
          <button
            key={tab.key}
            onClick={() => setSection(tab.key)}
            className={`px-4 py-2 text-sm font-medium border-b-2 transition-colors ${
              section === tab.key
                ? "border-indigo-500 text-indigo-600"
                : "border-transparent text-gray-500 hover:text-gray-700 hover:border-gray-300"
            }`}
          >
            {tab.label}
          </button>
        ))}
      </div>

      {/* Content */}
      {section === "welcome" && <WelcomeSection />}
      {section === "setup" && <SetupSection />}
      {section === "workflow" && <WorkflowSection />}
    </div>
  );
}
