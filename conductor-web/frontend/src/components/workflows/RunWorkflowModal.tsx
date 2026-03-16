import { useState } from "react";
import { api } from "../../api/client";
import type { WorkflowDefSummary } from "../../api/types";

interface RunWorkflowModalProps {
  def: WorkflowDefSummary;
  worktreeId: string;
  onClose: () => void;
  onStarted: () => void;
}

export function RunWorkflowModal({
  def,
  worktreeId,
  onClose,
  onStarted,
}: RunWorkflowModalProps) {
  const [model, setModel] = useState("");
  const [dryRun, setDryRun] = useState(false);
  const [inputs, setInputs] = useState<Record<string, string>>({});
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleSubmit = async () => {
    setSubmitting(true);
    setError(null);
    try {
      await api.runWorkflow(worktreeId, {
        name: def.name,
        model: model || undefined,
        dry_run: dryRun || undefined,
        inputs: Object.keys(inputs).length > 0 ? inputs : undefined,
      });
      onStarted();
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "Failed to start workflow");
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50 p-4">
      <div className="bg-gray-800 rounded-lg p-6 w-full max-w-md border border-gray-600">
        <h3 className="text-lg font-semibold text-gray-200 mb-4">
          Run: {def.name}
        </h3>

        <div className="space-y-4">
          <div>
            <label className="block text-sm text-gray-400 mb-1">Model override</label>
            <input
              type="text"
              value={model}
              onChange={(e) => setModel(e.target.value)}
              placeholder="Default model"
              className="w-full px-3 py-2 bg-gray-900 border border-gray-600 rounded text-gray-200 text-sm"
            />
          </div>

          <div className="flex items-center gap-2">
            <input
              type="checkbox"
              id="dry-run"
              checked={dryRun}
              onChange={(e) => setDryRun(e.target.checked)}
              className="rounded"
            />
            <label htmlFor="dry-run" className="text-sm text-gray-400">
              Dry run
            </label>
          </div>

          {def.inputs.map((input) => (
            <div key={input.name}>
              <label className="block text-sm text-gray-400 mb-1">
                {input.name}
                {input.required && <span className="text-red-400 ml-1">*</span>}
              </label>
              <input
                type="text"
                value={inputs[input.name] || ""}
                onChange={(e) =>
                  setInputs((prev) => ({ ...prev, [input.name]: e.target.value }))
                }
                className="w-full px-3 py-2 bg-gray-900 border border-gray-600 rounded text-gray-200 text-sm"
              />
            </div>
          ))}

          {error && (
            <p className="text-red-400 text-sm">{error}</p>
          )}
        </div>

        <div className="flex justify-end gap-2 mt-6">
          <button
            onClick={onClose}
            className="px-4 py-2 text-sm text-gray-400 hover:text-gray-200"
          >
            Cancel
          </button>
          <button
            onClick={handleSubmit}
            disabled={submitting}
            className="px-4 py-2 text-sm bg-cyan-600 hover:bg-cyan-500 text-white rounded disabled:opacity-50"
          >
            {submitting ? "Starting..." : "Start Workflow"}
          </button>
        </div>
      </div>
    </div>
  );
}
