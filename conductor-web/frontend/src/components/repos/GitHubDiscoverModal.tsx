import { useEffect, useState } from "react";
import { api } from "../../api/client";
import type { DiscoverableRepo } from "../../api/types";

// ── Org picker step ───────────────────────────────────────────────────────────

function OrgPicker({
  onSelect,
}: {
  onSelect: (owner: string) => void;
}) {
  const [orgs, setOrgs] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api
      .listGithubOrgs()
      .then(setOrgs)
      .catch((err) =>
        setError(err instanceof Error ? err.message : "Failed to fetch orgs"),
      )
      .finally(() => setLoading(false));
  }, []);

  if (loading) {
    return (
      <p className="text-sm text-gray-500 text-center py-8">
        Fetching organizations...
      </p>
    );
  }
  if (error) {
    return (
      <div className="rounded-md bg-red-50 border border-red-200 p-3 text-sm text-red-700">
        {error}
      </div>
    );
  }

  const entries: { label: string; owner: string }[] = [
    { label: "Personal (your repos)", owner: "" },
    ...orgs.map((o) => ({ label: o, owner: o })),
  ];

  return (
    <ul className="divide-y divide-gray-100">
      {entries.map(({ label, owner }) => (
        <li key={owner || "__personal"}>
          <button
            onClick={() => onSelect(owner)}
            className="w-full text-left px-2 py-3 text-sm text-gray-800 hover:bg-indigo-50 hover:text-indigo-700 rounded transition-colors"
          >
            {label}
          </button>
        </li>
      ))}
    </ul>
  );
}

// ── Repo picker step ──────────────────────────────────────────────────────────

function RepoPicker({
  owner,
  onBack,
  onImported,
}: {
  owner: string;
  onBack: () => void;
  onImported: () => void;
}) {
  const [repos, setRepos] = useState<DiscoverableRepo[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [importing, setImporting] = useState(false);
  const [importErrors, setImportErrors] = useState<Record<string, string>>({});

  useEffect(() => {
    setLoading(true);
    setError(null);
    setSelected(new Set());
    setImportErrors({});
    api
      .discoverGithubRepos(owner || undefined)
      .then(setRepos)
      .catch((err) =>
        setError(err instanceof Error ? err.message : "Failed to fetch repos"),
      )
      .finally(() => setLoading(false));
  }, [owner]);

  function toggle(fullName: string) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(fullName)) next.delete(fullName);
      else next.add(fullName);
      return next;
    });
  }

  function selectAll() {
    setSelected(
      new Set(repos.filter((r) => !r.already_registered).map((r) => r.full_name)),
    );
  }

  async function handleImport() {
    const toImport = repos.filter(
      (r) => selected.has(r.full_name) && !r.already_registered,
    );
    if (toImport.length === 0) return;

    setImporting(true);
    setImportErrors({});
    const errors: Record<string, string> = {};
    let anySuccess = false;

    for (const repo of toImport) {
      try {
        await api.registerRepo({ remote_url: repo.clone_url });
        anySuccess = true;
      } catch (err) {
        errors[repo.full_name] =
          err instanceof Error ? err.message : "Import failed";
      }
    }

    setImportErrors(errors);
    setImporting(false);

    if (anySuccess) {
      onImported();
      setSelected((prev) => {
        const next = new Set(prev);
        for (const repo of toImport) {
          if (!errors[repo.full_name]) next.delete(repo.full_name);
        }
        return next;
      });
      api
        .discoverGithubRepos(owner || undefined)
        .then(setRepos)
        .catch(() => {});
    }
  }

  const unregisteredCount = repos.filter((r) => !r.already_registered).length;
  const selectedCount = selected.size;
  const ownerLabel = owner || "Personal";

  return (
    <>
      <div className="flex-1 overflow-y-auto px-5 py-4">
        {loading && (
          <p className="text-sm text-gray-500 text-center py-8">
            Loading repos from <strong>{ownerLabel}</strong>...
          </p>
        )}
        {error && (
          <div className="rounded-md bg-red-50 border border-red-200 p-3 text-sm text-red-700">
            {error}
          </div>
        )}
        {!loading && !error && repos.length === 0 && (
          <p className="text-sm text-gray-500 text-center py-8">
            No repos found in <strong>{ownerLabel}</strong>.
          </p>
        )}
        {!loading && repos.length > 0 && (
          <>
            <div className="flex items-center justify-between mb-3">
              <p className="text-xs text-gray-500">
                {repos.length} repo{repos.length !== 1 ? "s" : ""} in{" "}
                <strong>{ownerLabel}</strong>
                {unregisteredCount < repos.length &&
                  ` · ${repos.length - unregisteredCount} already registered`}
              </p>
              <div className="flex gap-2">
                <button
                  onClick={selectAll}
                  className="text-xs text-indigo-600 hover:underline"
                >
                  Select all
                </button>
                {selectedCount > 0 && (
                  <button
                    onClick={() => setSelected(new Set())}
                    className="text-xs text-gray-500 hover:underline"
                  >
                    Clear
                  </button>
                )}
              </div>
            </div>
            <ul className="divide-y divide-gray-100">
              {repos.map((repo) => {
                const isRegistered = repo.already_registered;
                const isSelected = selected.has(repo.full_name);
                const importErr = importErrors[repo.full_name];
                return (
                  <li
                    key={repo.full_name}
                    className={`flex items-start gap-3 py-2.5 ${isRegistered ? "opacity-50" : ""}`}
                  >
                    <input
                      type="checkbox"
                      checked={isSelected}
                      disabled={isRegistered}
                      onChange={() => toggle(repo.full_name)}
                      className="mt-0.5 h-4 w-4 rounded border-gray-300 text-indigo-600 focus:ring-indigo-500 disabled:cursor-not-allowed"
                    />
                    <div className="min-w-0 flex-1">
                      <div className="flex items-center gap-2">
                        <span className="text-sm font-medium text-gray-900 truncate">
                          {repo.full_name}
                        </span>
                        {repo.private && (
                          <span className="shrink-0 rounded px-1 py-0.5 text-xs bg-gray-100 text-gray-500">
                            private
                          </span>
                        )}
                        {isRegistered && (
                          <span className="shrink-0 rounded px-1 py-0.5 text-xs bg-green-100 text-green-700">
                            registered
                          </span>
                        )}
                      </div>
                      {repo.description && (
                        <p className="mt-0.5 text-xs text-gray-500 truncate">
                          {repo.description}
                        </p>
                      )}
                      {importErr && (
                        <p className="mt-0.5 text-xs text-red-600">{importErr}</p>
                      )}
                    </div>
                  </li>
                );
              })}
            </ul>
          </>
        )}
      </div>
      <div className="flex items-center justify-between px-5 py-3 border-t border-gray-200 bg-gray-50 rounded-b-xl">
        <button
          onClick={onBack}
          className="text-sm text-gray-500 hover:text-gray-700 flex items-center gap-1"
        >
          ← Back
        </button>
        <div className="flex items-center gap-3">
          <span className="text-xs text-gray-500">
            {selectedCount > 0 ? `${selectedCount} selected` : "Select repos to import"}
          </span>
          <button
            onClick={handleImport}
            disabled={selectedCount === 0 || importing}
            className="px-3 py-1.5 text-sm rounded-md bg-indigo-600 text-white hover:bg-indigo-700 disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {importing
              ? "Importing..."
              : `Import ${selectedCount > 0 ? selectedCount : ""} Repo${selectedCount !== 1 ? "s" : ""}`}
          </button>
        </div>
      </div>
    </>
  );
}

// ── Modal shell ───────────────────────────────────────────────────────────────

export function GitHubDiscoverModal({
  open,
  onClose,
  onImported,
}: {
  open: boolean;
  onClose: () => void;
  onImported: () => void;
}) {
  // null = org picker; string = repo picker for that owner ("" = personal)
  const [selectedOwner, setSelectedOwner] = useState<string | null>(null);

  // Reset to org picker whenever the modal opens
  useEffect(() => {
    if (open) setSelectedOwner(null);
  }, [open]);

  if (!open) return null;

  const title =
    selectedOwner === null
      ? "Discover GitHub Repos"
      : `${selectedOwner || "Personal"} — repos`;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40">
      <div className="w-full max-w-2xl rounded-xl border border-gray-200 bg-white shadow-xl flex flex-col max-h-[80vh]">
        {/* Header */}
        <div className="flex items-center justify-between px-5 py-4 border-b border-gray-200">
          <h2 className="text-base font-semibold text-gray-900">{title}</h2>
          <button
            onClick={onClose}
            className="text-gray-400 hover:text-gray-600 text-lg leading-none"
            aria-label="Close"
          >
            ✕
          </button>
        </div>

        {selectedOwner === null ? (
          <div className="flex-1 overflow-y-auto px-5 py-4">
            <OrgPicker onSelect={setSelectedOwner} />
          </div>
        ) : (
          <RepoPicker
            owner={selectedOwner}
            onBack={() => setSelectedOwner(null)}
            onImported={onImported}
          />
        )}
      </div>
    </div>
  );
}
