import { useState } from "react";
import { useApi } from "../hooks/useApi";
import { api } from "../api/client";
import { ModelPicker } from "../components/shared/ModelPicker";
import { ThemePicker } from "../components/shared/ThemePicker";
import { usePushNotifications } from "../hooks/usePushNotifications";

export function SettingsPage() {
  const { data: globalConfig, refetch: refetchGlobalConfig } = useApi(
    () => api.getGlobalModel(),
    [],
  );
  const [savingGlobalModel, setSavingGlobalModel] = useState(false);
  const [globalModelError, setGlobalModelError] = useState<string | null>(null);

  // Push notifications hook
  const pushNotifications = usePushNotifications();

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

      {/* Push Notifications Section */}
      <section>
        <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400 mb-1">
          Push Notifications
        </h3>
        <p className="text-sm text-gray-500 mb-3">
          Get notified about workflow completions, failures, and gate approvals even when the app is in the background.
        </p>

        {pushNotifications.error && (
          <div className="mb-3 px-3 py-2 text-sm text-red-700 bg-red-50 rounded-md border border-red-200">
            {pushNotifications.error}
          </div>
        )}

        {!pushNotifications.isSupported ? (
          <div className="px-3 py-2 text-sm text-gray-600 bg-gray-50 rounded-md border border-gray-200">
            Push notifications are not supported on this device or browser.
          </div>
        ) : (
          <div className="space-y-3">
            <div className="flex items-center justify-between">
              <div>
                <div className="text-sm font-medium text-gray-900">
                  Push Notifications
                </div>
                <div className="text-xs text-gray-500">
                  Status: {pushNotifications.permission === 'granted'
                    ? pushNotifications.isSubscribed
                      ? 'Enabled'
                      : 'Permission granted, not subscribed'
                    : pushNotifications.permission === 'denied'
                      ? 'Blocked'
                      : 'Not configured'
                  }
                </div>
              </div>

              <div className="flex space-x-2">
                {pushNotifications.permission === 'default' && (
                  <button
                    onClick={() => pushNotifications.actions.requestPermission()}
                    disabled={pushNotifications.isLoading}
                    className="px-3 py-2 text-sm font-medium text-white bg-blue-600 rounded-md hover:bg-blue-700 disabled:opacity-50 disabled:cursor-not-allowed"
                  >
                    {pushNotifications.isLoading ? 'Requesting...' : 'Request Permission'}
                  </button>
                )}

                {pushNotifications.permission === 'granted' && !pushNotifications.isSubscribed && (
                  <button
                    onClick={() => pushNotifications.actions.subscribe()}
                    disabled={pushNotifications.isLoading}
                    className="px-3 py-2 text-sm font-medium text-white bg-green-600 rounded-md hover:bg-green-700 disabled:opacity-50 disabled:cursor-not-allowed"
                  >
                    {pushNotifications.isLoading ? 'Subscribing...' : 'Enable Notifications'}
                  </button>
                )}

                {pushNotifications.permission === 'granted' && pushNotifications.isSubscribed && (
                  <button
                    onClick={() => pushNotifications.actions.unsubscribe()}
                    disabled={pushNotifications.isLoading}
                    className="px-3 py-2 text-sm font-medium text-gray-700 bg-gray-100 rounded-md hover:bg-gray-200 disabled:opacity-50 disabled:cursor-not-allowed"
                  >
                    {pushNotifications.isLoading ? 'Unsubscribing...' : 'Disable Notifications'}
                  </button>
                )}

                {pushNotifications.permission === 'denied' && (
                  <div className="text-xs text-gray-500">
                    Permission denied. Enable in browser settings.
                  </div>
                )}
              </div>
            </div>

            {pushNotifications.isSubscribed && (
              <div className="px-3 py-2 text-xs text-green-700 bg-green-50 rounded-md border border-green-200">
                ✓ You'll receive notifications for workflow events and gate approvals.
              </div>
            )}
          </div>
        )}
      </section>
    </div>
  );
}
