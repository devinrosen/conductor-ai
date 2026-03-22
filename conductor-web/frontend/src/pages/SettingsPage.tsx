import { useState } from "react";
import { useApi } from "../hooks/useApi";
import { api } from "../api/client";
import { ModelPicker } from "../components/shared/ModelPicker";
import { ThemePicker } from "../components/shared/ThemePicker";

export function SettingsPage() {
  const { data: globalConfig, refetch: refetchGlobalConfig } = useApi(
    () => api.getGlobalModel(),
    [],
  );
  const [savingGlobalModel, setSavingGlobalModel] = useState(false);
  const [globalModelError, setGlobalModelError] = useState<string | null>(null);

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
    </div>
  );
}
