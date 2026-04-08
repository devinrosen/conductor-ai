import { useState } from "react";
import { useApi } from "../hooks/useApi";
import { api } from "../api/client";
import type { HookSummary } from "../api/types";
import { ModelPicker } from "../components/shared/ModelPicker";
import { ThemePicker } from "../components/shared/ThemePicker";

export function SettingsPage() {
  const { data: globalConfig, refetch: refetchGlobalConfig } = useApi(
    () => api.getGlobalModel(),
    [],
  );
  const [savingGlobalModel, setSavingGlobalModel] = useState(false);
  const [globalModelError, setGlobalModelError] = useState<string | null>(null);

  const { data: hooks, loading: hooksLoading } = useApi(
    () => api.listHooks(),
    [],
  );

  // Track which hooks have had a test fired (map of index → timeout handle)
  const [firedHooks, setFiredHooks] = useState<Set<number>>(new Set());

  async function handleGlobalModelChange(model: string | null) {
    setSavingGlobalModel(true);
    setGlobalModelError(null);
    try {
      await api.setGlobalModel(model);
      refetchGlobalConfig();
    } catch (err) {
      setGlobalModelError(
        err instanceof Error ? err.message : "Failed to save",
      );
    } finally {
      setSavingGlobalModel(false);
    }
  }

  async function handleTestHook(hook: HookSummary) {
    try {
      await api.testHook(hook.index);
      setFiredHooks((prev) => new Set(prev).add(hook.index));
      setTimeout(() => {
        setFiredHooks((prev) => {
          const next = new Set(prev);
          next.delete(hook.index);
          return next;
        });
      }, 3000);
    } catch {
      // Errors surface in hook output/logs, not here
    }
  }

  return (
    <div className="space-y-8">
      <h2 className="text-xl font-bold text-gray-900">Settings</h2>

      {/* Theme Section */}
      <section>
        <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400 mb-1">
          Theme
        </h3>
        <p className="text-sm text-gray-500 mb-3">
          Choose a railway heritage theme. Each is inspired by an iconic rail design system.
        </p>
        <ThemePicker />
      </section>

      {/* Global Model Section */}
      <section>
        <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400 mb-1">
          Global Model Default
        </h3>
        <p className="text-sm text-gray-500 mb-3">
          Default Claude model for all agent runs. Overridden by per-repo and
          per-worktree model settings.
        </p>
        {globalModelError && (
          <div className="mb-3 px-3 py-2 text-sm text-red-700 bg-red-50 rounded-md border border-red-200">
            {globalModelError}
          </div>
        )}
        <ModelPicker
          value={globalConfig?.model ?? null}
          onChange={handleGlobalModelChange}
          disabled={savingGlobalModel}
        />
      </section>

      {/* Notification Hooks Section */}
      <section>
        <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400 mb-1">
          Notification Hooks
        </h3>
        <p className="text-sm text-gray-500 mb-3">
          Shell or HTTP hooks fired on workflow and agent lifecycle events. Configure in{" "}
          <code className="text-xs bg-gray-100 px-1 py-0.5 rounded">~/.conductor/config.toml</code>.
        </p>

        {hooksLoading ? (
          <div className="text-sm text-gray-400">Loading hooks…</div>
        ) : !hooks || hooks.length === 0 ? (
          <div className="px-3 py-2 text-sm text-gray-600 bg-gray-50 rounded-md border border-gray-200">
            No hooks configured. Edit{" "}
            <code className="text-xs bg-gray-100 px-1 py-0.5 rounded">~/.conductor/config.toml</code>{" "}
            to add hooks. See{" "}
            <a
              href="https://github.com/devinrosen/conductor-ai/tree/main/docs/examples/hooks"
              target="_blank"
              rel="noreferrer"
              className="text-blue-600 hover:underline"
            >
              example scripts
            </a>{" "}
            to get started.
          </div>
        ) : (
          <div className="overflow-hidden rounded-md border border-gray-200">
            <table className="min-w-full divide-y divide-gray-200 text-sm">
              <thead className="bg-gray-50">
                <tr>
                  <th className="px-3 py-2 text-left text-xs font-medium text-gray-500 uppercase tracking-wider">
                    Pattern
                  </th>
                  <th className="px-3 py-2 text-left text-xs font-medium text-gray-500 uppercase tracking-wider">
                    Type
                  </th>
                  <th className="px-3 py-2 text-left text-xs font-medium text-gray-500 uppercase tracking-wider">
                    Command / URL
                  </th>
                  <th className="px-3 py-2 text-right text-xs font-medium text-gray-500 uppercase tracking-wider">
                    Test
                  </th>
                </tr>
              </thead>
              <tbody className="divide-y divide-gray-100 bg-white">
                {hooks.map((hook) => (
                  <tr key={hook.index}>
                    <td className="px-3 py-2 font-mono text-xs text-gray-800">
                      {hook.on}
                    </td>
                    <td className="px-3 py-2 text-xs text-gray-500">
                      {hook.kind}
                    </td>
                    <td className="px-3 py-2 font-mono text-xs text-gray-600 max-w-xs truncate">
                      {hook.command ?? <span className="italic text-gray-400">—</span>}
                    </td>
                    <td className="px-3 py-2 text-right">
                      {firedHooks.has(hook.index) ? (
                        <span className="text-xs text-green-600 font-medium">
                          Test sent
                        </span>
                      ) : (
                        <button
                          onClick={() => handleTestHook(hook)}
                          className="px-2 py-1 text-xs font-medium text-gray-700 bg-gray-100 rounded hover:bg-gray-200"
                          title="Fire a synthetic WorkflowRunCompleted event through this hook"
                        >
                          Send test event
                        </button>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
            <p className="px-3 py-2 text-xs text-gray-400 bg-gray-50 border-t border-gray-200">
              Test events fire asynchronously. Errors appear in hook output, not here.
            </p>
          </div>
        )}
      </section>
    </div>
  );
}
