import { useState, useEffect } from "react";
import { api } from "../../api/client";
import type { IssueSource } from "../../api/types";
import { LoadingSpinner } from "../shared/LoadingSpinner";
import { EmptyState } from "../shared/EmptyState";
import { ConfirmDialog } from "../shared/ConfirmDialog";

interface Props {
  repoId: string;
  remoteUrl: string;
  sources: IssueSource[];
  loading: boolean;
  onChanged: () => void;
}

function parseConfig(source: IssueSource): Record<string, string> {
  try {
    return JSON.parse(source.config_json);
  } catch {
    return {};
  }
}

function formatConfig(source: IssueSource): string {
  const cfg = parseConfig(source);
  if (source.source_type === "github") {
    return `${cfg.owner}/${cfg.repo}`;
  }
  if (source.source_type === "jira") {
    return `${cfg.url} (${cfg.jql})`;
  }
  return source.config_json;
}

export function IssueSourcesSection({
  repoId,
  remoteUrl,
  sources,
  loading,
  onChanged,
}: Props) {
  const [showAdd, setShowAdd] = useState(false);
  const [sourceType, setSourceType] = useState<"github" | "jira">("github");
  const [jiraUrl, setJiraUrl] = useState("");
  const [jiraJql, setJiraJql] = useState("");
  const [githubOwner, setGithubOwner] = useState("");
  const [githubRepo, setGithubRepo] = useState("");
  const [autoInferred, setAutoInferred] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<IssueSource | null>(null);

  // Auto-infer GitHub owner/repo from remote URL
  useEffect(() => {
    if (sourceType !== "github") return;
    const match =
      remoteUrl.match(/github\.com[:/]([^/]+)\/([^/.]+)/) ?? null;
    if (match) {
      setGithubOwner(match[1]);
      setGithubRepo(match[2]);
      setAutoInferred(true);
    } else {
      setAutoInferred(false);
    }
  }, [remoteUrl, sourceType]);

  useEffect(() => {
    if (!showAdd) return;
    function handleKeyDown(e: KeyboardEvent) {
      if (e.key === "Escape") {
        resetForm();
      }
    }
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [showAdd]);

  function resetForm() {
    setShowAdd(false);
    setSourceType("github");
    setJiraUrl("");
    setJiraJql("");
    setGithubOwner("");
    setGithubRepo("");
    setAutoInferred(false);
    setError(null);
  }

  async function handleAdd(e: React.FormEvent) {
    e.preventDefault();
    setSaving(true);
    setError(null);
    try {
      if (sourceType === "github") {
        // If user hasn't modified the auto-inferred values, let the backend infer
        const configJson = autoInferred
          ? undefined
          : JSON.stringify({ owner: githubOwner.trim(), repo: githubRepo.trim() });
        if (!autoInferred && (!githubOwner.trim() || !githubRepo.trim())) {
          setError("GitHub owner and repo are required");
          setSaving(false);
          return;
        }
        await api.createIssueSource(repoId, {
          source_type: "github",
          config_json: configJson,
        });
      } else {
        if (!jiraUrl.trim() || !jiraJql.trim()) {
          setError("Jira URL and JQL are required");
          setSaving(false);
          return;
        }
        await api.createIssueSource(repoId, {
          source_type: "jira",
          config_json: JSON.stringify({
            url: jiraUrl.trim(),
            jql: jiraJql.trim(),
          }),
        });
      }
      resetForm();
      onChanged();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to add source");
    } finally {
      setSaving(false);
    }
  }

  async function handleDelete() {
    if (!deleteTarget) return;
    setSaving(true);
    setError(null);
    try {
      await api.deleteIssueSource(repoId, deleteTarget.id);
      setDeleteTarget(null);
      onChanged();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to delete");
    } finally {
      setSaving(false);
    }
  }

  const hasGithub = sources.some((s) => s.source_type === "github");
  const hasJira = sources.some((s) => s.source_type === "jira");
  const canAdd = !hasGithub || !hasJira;

  return (
    <section>
      <div className="flex items-center justify-between mb-3">
        <div>
          <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400">
            Issue Sources
          </h3>
          <p className="text-sm text-gray-500 mt-1">
            Configure where tickets are synced from for this repo.
          </p>
        </div>
        {canAdd && (
          <button
            onClick={() => {
              // Default to whichever type isn't already added
              if (hasGithub && !hasJira) setSourceType("jira");
              else setSourceType("github");
              setShowAdd(true);
            }}
            className="px-3 py-1.5 text-sm rounded-md bg-indigo-600 text-white hover:bg-indigo-700"
          >
            Add Source
          </button>
        )}
      </div>

      {error && (
        <div className="mb-3 px-3 py-2 text-sm text-red-700 bg-red-50 rounded-md border border-red-200">
          {error}
        </div>
      )}

      {loading ? (
        <LoadingSpinner />
      ) : sources.length === 0 ? (
        <EmptyState message="No issue sources configured" />
      ) : (
        <div className="rounded-lg border border-gray-200 bg-white overflow-hidden">
          <table className="w-full text-sm">
            <thead className="bg-gray-50 text-left text-xs text-gray-500 uppercase">
              <tr>
                <th className="px-4 py-2">Type</th>
                <th className="px-4 py-2">Config</th>
                <th className="px-4 py-2 text-right">Actions</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-gray-100">
              {sources.map((source) => (
                <tr key={source.id} className="hover:bg-gray-50">
                  <td className="px-4 py-2">
                    <span
                      className={`inline-flex items-center px-2 py-0.5 rounded text-xs font-medium ${
                        source.source_type === "github"
                          ? "bg-gray-800 text-white"
                          : "bg-blue-100 text-blue-700"
                      }`}
                    >
                      {source.source_type}
                    </span>
                  </td>
                  <td className="px-4 py-2 text-gray-600 font-mono text-xs">
                    {formatConfig(source)}
                  </td>
                  <td className="px-4 py-2 text-right">
                    <button
                      onClick={() => setDeleteTarget(source)}
                      disabled={saving}
                      className="px-2 py-1 text-xs rounded border border-red-300 text-red-600 hover:bg-red-50 disabled:opacity-50"
                    >
                      Remove
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {/* Add Issue Source Dialog */}
      {showAdd && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40">
          <div className="bg-white rounded-lg shadow-lg p-6 max-w-sm w-full mx-4">
            <h3 className="text-lg font-semibold text-gray-900">
              Add Issue Source
            </h3>
            <form onSubmit={handleAdd} className="mt-4 space-y-3">
              <div>
                <label className="block text-sm font-medium text-gray-700 mb-1">
                  Source Type
                </label>
                <select
                  value={sourceType}
                  onChange={(e) =>
                    setSourceType(e.target.value as "github" | "jira")
                  }
                  className="w-full px-3 py-1.5 text-sm border border-gray-300 rounded-md focus:ring-indigo-500 focus:border-indigo-500"
                >
                  {!hasGithub && <option value="github">GitHub</option>}
                  {!hasJira && <option value="jira">Jira</option>}
                </select>
              </div>

              {sourceType === "github" && (
                <>
                  <div>
                    <label className="block text-sm font-medium text-gray-700 mb-1">
                      Owner
                    </label>
                    <input
                      type="text"
                      value={githubOwner}
                      onChange={(e) => {
                        setGithubOwner(e.target.value);
                        setAutoInferred(false);
                      }}
                      placeholder="e.g. octocat"
                      className="w-full px-3 py-1.5 text-sm border border-gray-300 rounded-md focus:ring-indigo-500 focus:border-indigo-500"
                    />
                  </div>
                  <div>
                    <label className="block text-sm font-medium text-gray-700 mb-1">
                      Repository
                    </label>
                    <input
                      type="text"
                      value={githubRepo}
                      onChange={(e) => {
                        setGithubRepo(e.target.value);
                        setAutoInferred(false);
                      }}
                      placeholder="e.g. my-project"
                      className="w-full px-3 py-1.5 text-sm border border-gray-300 rounded-md focus:ring-indigo-500 focus:border-indigo-500"
                    />
                  </div>
                  {autoInferred && (
                    <p className="text-xs text-green-600">
                      Auto-inferred from remote URL
                    </p>
                  )}
                </>
              )}

              {sourceType === "jira" && (
                <>
                  <div>
                    <label className="block text-sm font-medium text-gray-700 mb-1">
                      Jira URL
                    </label>
                    <input
                      type="text"
                      value={jiraUrl}
                      onChange={(e) => setJiraUrl(e.target.value)}
                      placeholder="e.g. https://mycompany.atlassian.net"
                      className="w-full px-3 py-1.5 text-sm border border-gray-300 rounded-md focus:ring-indigo-500 focus:border-indigo-500"
                      autoFocus
                    />
                  </div>
                  <div>
                    <label className="block text-sm font-medium text-gray-700 mb-1">
                      JQL Query
                    </label>
                    <input
                      type="text"
                      value={jiraJql}
                      onChange={(e) => setJiraJql(e.target.value)}
                      placeholder='e.g. project = PROJ AND status != Done'
                      className="w-full px-3 py-1.5 text-sm border border-gray-300 rounded-md focus:ring-indigo-500 focus:border-indigo-500"
                    />
                  </div>
                </>
              )}

              <div className="flex justify-end gap-2 pt-2">
                <button
                  type="button"
                  onClick={resetForm}
                  className="px-3 py-1.5 text-sm rounded-md border border-gray-300 text-gray-700 hover:bg-gray-50"
                >
                  Cancel
                </button>
                <button
                  type="submit"
                  disabled={saving}
                  className="px-3 py-1.5 text-sm rounded-md bg-indigo-600 text-white hover:bg-indigo-700 disabled:opacity-50"
                >
                  {saving ? "Adding..." : "Add"}
                </button>
              </div>
            </form>
          </div>
        </div>
      )}

      {/* Delete Confirmation */}
      <ConfirmDialog
        open={deleteTarget !== null}
        title="Remove Issue Source"
        message={
          deleteTarget
            ? `Remove ${deleteTarget.source_type} source? You will need to re-add it to sync tickets from this source.`
            : ""
        }
        onConfirm={handleDelete}
        onCancel={() => setDeleteTarget(null)}
      />
    </section>
  );
}
