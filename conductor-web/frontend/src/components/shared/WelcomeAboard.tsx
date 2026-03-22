import { useState } from "react";
import { RegisterRepoForm } from "../repos/RegisterRepoForm";
import { GitHubDiscoverModal } from "../repos/GitHubDiscoverModal";

interface WelcomeAboardProps {
  onRepoCreated: () => void;
}

const steps = [
  {
    number: 1,
    title: "Add a Repo",
    description: "Register your first station to get the trains running.",
  },
  {
    number: 2,
    title: "Create a Worktree",
    description: "Lay some track — create a worktree to start working.",
  },
  {
    number: 3,
    title: "Sync Tickets",
    description: "Get your passengers aboard by syncing issues from GitHub.",
  },
];

export function WelcomeAboard({ onRepoCreated }: WelcomeAboardProps) {
  const [registerOpen, setRegisterOpen] = useState(false);
  const [discoverOpen, setDiscoverOpen] = useState(false);

  return (
    <div className="flex flex-col items-center justify-center py-16 px-4">
      <div className="max-w-md w-full text-center space-y-8">
        {/* Header */}
        <div className="space-y-2">
          <h2 className="text-2xl font-bold text-gray-900">
            Welcome Aboard, Conductor!
          </h2>
          <p className="text-gray-500 text-sm">
            Let&rsquo;s get your first station set up in three stops.
          </p>
        </div>

        {/* Route map */}
        <div className="flex items-center justify-center gap-0">
          {steps.map((step, i) => (
            <div key={step.number} className="flex items-center">
              {i > 0 && (
                <div className="w-8 h-0.5 bg-gray-300" />
              )}
              <div className="flex flex-col items-center gap-1.5">
                <div className="w-8 h-8 rounded-full border-2 border-indigo-500 flex items-center justify-center text-sm font-bold text-indigo-500 bg-indigo-100">
                  {step.number}
                </div>
                <span className="text-xs font-medium text-gray-700 whitespace-nowrap">
                  {step.title}
                </span>
              </div>
            </div>
          ))}
        </div>

        {/* First stop detail */}
        <div className="rounded-lg border border-gray-200 bg-white p-6 text-left space-y-4">
          <div className="flex items-center gap-2">
            <div className="w-6 h-6 rounded-full bg-indigo-500 text-white text-xs font-bold flex items-center justify-center">
              1
            </div>
            <h3 className="font-semibold text-gray-900">First Stop: Add a Repo</h3>
          </div>
          <p className="text-sm text-gray-500">
            Register a git repository to start orchestrating your work.
            You can enter a URL manually or discover repos from GitHub.
          </p>
          <div className="flex flex-wrap gap-2">
            <button
              onClick={() => setDiscoverOpen(true)}
              className="px-3 py-2 text-sm rounded-md border border-gray-300 text-gray-700 hover:bg-gray-100"
            >
              Discover from GitHub
            </button>
            <RegisterRepoForm
              onCreated={onRepoCreated}
              open={registerOpen}
              onOpenChange={setRegisterOpen}
            />
          </div>
        </div>

        <p className="text-xs text-gray-400">
          You&rsquo;re the conductor. Keep the trains on time.
        </p>
      </div>

      <GitHubDiscoverModal
        open={discoverOpen}
        onClose={() => setDiscoverOpen(false)}
        onImported={onRepoCreated}
      />
    </div>
  );
}
