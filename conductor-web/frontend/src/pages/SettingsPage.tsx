import { useState, useEffect } from "react";
import { useApi } from "../hooks/useApi";
import { api } from "../api/client";
import type { WorkTarget } from "../api/types";
import { LoadingSpinner } from "../components/shared/LoadingSpinner";
import { EmptyState } from "../components/shared/EmptyState";
import { ConfirmDialog } from "../components/shared/ConfirmDialog";

export function SettingsPage() {
  const {
    data: targets,
    loading,
    refetch,
  } = useApi(() => api.listWorkTargets(), []);

  const [showAdd, setShowAdd] = useState(false);
  const [name, setName] = useState("");
  const [command, setCommand] = useState("");
  const [targetType, setTargetType] = useState("editor");
  const [deleteIndex, setDeleteIndex] = useState<number | null>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!showAdd) return;
    function handleKeyDown(e: KeyboardEvent) {
      if (e.key === "Escape") {
        setShowAdd(false);
        setName("");
        setCommand("");
        setTargetType("editor");
      }
    }
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [showAdd]);

  async function handleAdd(e: React.FormEvent) {
    e.preventDefault();
    if (!name.trim() || !command.trim()) return;
    setSaving(true);
    setError(null);
    try {
      await api.createWorkTarget({
        name: name.trim(),
        command: command.trim(),
        type: targetType,
      });
      setName("");
      setCommand("");
      setTargetType("editor");
      setShowAdd(false);
      refetch();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to add");
    } finally {
      setSaving(false);
    }
  }

  async function handleDelete() {
    if (deleteIndex === null) return;
    setSaving(true);
    setError(null);
    try {
      await api.deleteWorkTarget(deleteIndex);
      setDeleteIndex(null);
      refetch();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to delete");
    } finally {
      setSaving(false);
    }
  }

  async function handleMove(index: number, direction: "up" | "down") {
    if (!targets) return;
    const newTargets = [...targets];
    const swapIndex = direction === "up" ? index - 1 : index + 1;
    if (swapIndex < 0 || swapIndex >= newTargets.length) return;
    [newTargets[index], newTargets[swapIndex]] = [
      newTargets[swapIndex],
      newTargets[index],
    ];
    setSaving(true);
    setError(null);
    try {
      await api.replaceWorkTargets(newTargets);
      refetch();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to reorder");
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="space-y-8">
      <h2 className="text-xl font-bold text-gray-900">Settings</h2>

      {/* Work Targets Section */}
      <section>
        <div className="flex items-center justify-between mb-3">
          <div>
            <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400">
              Work Targets
            </h3>
            <p className="text-sm text-gray-500 mt-1">
              Editors and terminals to open worktrees with. Use the "Copy
              Command" button on any worktree to copy the launch command.
            </p>
          </div>
          <button
            onClick={() => setShowAdd(true)}
            className="px-3 py-1.5 text-sm rounded-md bg-indigo-600 text-white hover:bg-indigo-700"
          >
            Add Target
          </button>
        </div>

        {error && (
          <div className="mb-3 px-3 py-2 text-sm text-red-700 bg-red-50 rounded-md border border-red-200">
            {error}
          </div>
        )}

        {loading ? (
          <LoadingSpinner />
        ) : !targets || targets.length === 0 ? (
          <EmptyState message="No work targets configured" />
        ) : (
          <div className="rounded-lg border border-gray-200 bg-white overflow-hidden">
            <table className="w-full text-sm">
              <thead className="bg-gray-50 text-left text-xs text-gray-500 uppercase">
                <tr>
                  <th className="px-4 py-2">Name</th>
                  <th className="px-4 py-2">Command</th>
                  <th className="px-4 py-2">Type</th>
                  <th className="px-4 py-2 text-right">Actions</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-gray-100">
                {targets.map((target: WorkTarget, index: number) => (
                  <tr key={index} className="hover:bg-gray-50">
                    <td className="px-4 py-2 font-medium text-gray-900">
                      {target.name}
                    </td>
                    <td className="px-4 py-2 text-gray-600 font-mono text-xs">
                      {target.command}
                    </td>
                    <td className="px-4 py-2">
                      <span
                        className={`inline-flex items-center px-2 py-0.5 rounded text-xs font-medium ${
                          target.type === "editor"
                            ? "bg-blue-100 text-blue-700"
                            : "bg-green-100 text-green-700"
                        }`}
                      >
                        {target.type}
                      </span>
                    </td>
                    <td className="px-4 py-2 text-right">
                      <div className="flex items-center justify-end gap-1">
                        <button
                          onClick={() => handleMove(index, "up")}
                          disabled={index === 0 || saving}
                          className="px-2 py-1 text-xs rounded border border-gray-300 text-gray-600 hover:bg-gray-100 disabled:opacity-30"
                          title="Move up"
                        >
                          &uarr;
                        </button>
                        <button
                          onClick={() => handleMove(index, "down")}
                          disabled={
                            index === targets.length - 1 || saving
                          }
                          className="px-2 py-1 text-xs rounded border border-gray-300 text-gray-600 hover:bg-gray-100 disabled:opacity-30"
                          title="Move down"
                        >
                          &darr;
                        </button>
                        <button
                          onClick={() => setDeleteIndex(index)}
                          disabled={saving}
                          className="px-2 py-1 text-xs rounded border border-red-300 text-red-600 hover:bg-red-50 disabled:opacity-50"
                        >
                          Delete
                        </button>
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>

      {/* Add Work Target Dialog */}
      {showAdd && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40">
          <div className="bg-white rounded-lg shadow-lg p-6 max-w-sm w-full mx-4">
            <h3 className="text-lg font-semibold text-gray-900">
              Add Work Target
            </h3>
            <form onSubmit={handleAdd} className="mt-4 space-y-3">
              <div>
                <label className="block text-sm font-medium text-gray-700 mb-1">
                  Name
                </label>
                <input
                  type="text"
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                  placeholder="e.g. VS Code"
                  className="w-full px-3 py-1.5 text-sm border border-gray-300 rounded-md focus:ring-indigo-500 focus:border-indigo-500"
                  autoFocus
                />
              </div>
              <div>
                <label className="block text-sm font-medium text-gray-700 mb-1">
                  Command
                </label>
                <input
                  type="text"
                  value={command}
                  onChange={(e) => setCommand(e.target.value)}
                  placeholder="e.g. code"
                  className="w-full px-3 py-1.5 text-sm border border-gray-300 rounded-md focus:ring-indigo-500 focus:border-indigo-500"
                />
              </div>
              <div>
                <label className="block text-sm font-medium text-gray-700 mb-1">
                  Type
                </label>
                <select
                  value={targetType}
                  onChange={(e) => setTargetType(e.target.value)}
                  className="w-full px-3 py-1.5 text-sm border border-gray-300 rounded-md focus:ring-indigo-500 focus:border-indigo-500"
                >
                  <option value="editor">Editor</option>
                  <option value="terminal">Terminal</option>
                </select>
              </div>
              <div className="flex justify-end gap-2 pt-2">
                <button
                  type="button"
                  onClick={() => {
                    setShowAdd(false);
                    setName("");
                    setCommand("");
                    setTargetType("editor");
                  }}
                  className="px-3 py-1.5 text-sm rounded-md border border-gray-300 text-gray-700 hover:bg-gray-50"
                >
                  Cancel
                </button>
                <button
                  type="submit"
                  disabled={!name.trim() || !command.trim() || saving}
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
        open={deleteIndex !== null}
        title="Delete Work Target"
        message={
          deleteIndex !== null && targets
            ? `Delete "${targets[deleteIndex]?.name}"? This cannot be undone.`
            : ""
        }
        onConfirm={handleDelete}
        onCancel={() => setDeleteIndex(null)}
      />
    </div>
  );
}
