import { useState } from "react";
import { useParams, Link } from "react-router";
import { useApi } from "../hooks/useApi";
import { api } from "../api/client";
import { LoadingSpinner } from "../components/shared/LoadingSpinner";
import { FeatureStatusBadge } from "../components/features/FeatureStatusBadge";
import { FeatureProgressBar } from "../components/features/FeatureProgressBar";

export function FeatureDetailPage() {
  const { repoId, featureName } = useParams<{ repoId: string; featureName: string }>();

  const { data, loading, error, refetch } = useApi(
    () => api.getFeature(repoId!, decodeURIComponent(featureName!)),
    [repoId, featureName],
  );

  const [syncing, setSyncing] = useState(false);
  const [running, setRunning] = useState(false);
  const [actioning, setActioning] = useState(false);
  const [closing, setClosing] = useState(false);
  const [runResult, setRunResult] = useState<{ dispatched: number; failed: number } | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  if (loading) return <LoadingSpinner />;
  if (error || !data) {
    return (
      <div className="p-6">
        <p className="text-sm text-red-500">{error ?? "Feature not found"}</p>
        <Link to="/features" className="text-sm text-indigo-600 hover:underline mt-2 inline-block">
          ← Back to Features
        </Link>
      </div>
    );
  }

  const { feature, tickets } = data;
  const name = decodeURIComponent(featureName!);

  async function handleSync() {
    setSyncing(true);
    setActionError(null);
    try {
      await api.syncFeature(repoId!, name);
      refetch();
    } catch (e) {
      setActionError(e instanceof Error ? e.message : String(e));
    } finally {
      setSyncing(false);
    }
  }

  async function handleRun() {
    setRunning(true);
    setRunResult(null);
    setActionError(null);
    try {
      const result = await api.runFeature(repoId!, name);
      setRunResult(result);
      refetch();
    } catch (e) {
      setActionError(e instanceof Error ? e.message : String(e));
    } finally {
      setRunning(false);
    }
  }

  async function handleReview() {
    setActioning(true);
    setActionError(null);
    try {
      await api.reviewFeature(repoId!, name);
      refetch();
    } catch (e) {
      setActionError(e instanceof Error ? e.message : String(e));
    } finally {
      setActioning(false);
    }
  }

  async function handleApprove() {
    setActioning(true);
    setActionError(null);
    try {
      await api.approveFeature(repoId!, name);
      refetch();
    } catch (e) {
      setActionError(e instanceof Error ? e.message : String(e));
    } finally {
      setActioning(false);
    }
  }

  async function handleClose() {
    setClosing(true);
    setActionError(null);
    try {
      await api.closeFeature(repoId!, name);
      refetch();
    } catch (e) {
      setActionError(e instanceof Error ? e.message : String(e));
    } finally {
      setClosing(false);
    }
  }

  return (
    <div className="p-6 max-w-4xl">
      {/* Breadcrumb */}
      <div className="mb-4 text-sm text-gray-500">
        <Link to="/features" className="text-indigo-600 hover:underline">
          Features
        </Link>
        <span className="mx-1">/</span>
        <span>{feature.name}</span>
      </div>

      {/* Feature Header */}
      <div className="mb-6">
        <div className="flex items-start gap-3 flex-wrap">
          <h1 className="text-xl font-semibold text-gray-900">{feature.name}</h1>
          <FeatureStatusBadge status={feature.status} />
        </div>
        <div className="mt-2 text-sm text-gray-500">
          Branch: <code className="bg-gray-100 px-1 py-0.5 rounded text-xs">{feature.branch}</code>
          {" → "}
          <code className="bg-gray-100 px-1 py-0.5 rounded text-xs">{feature.base_branch}</code>
        </div>
        <div className="mt-3 max-w-xs">
          <FeatureProgressBar
            merged={feature.tickets_merged}
            total={feature.tickets_total}
          />
        </div>
      </div>

      {/* Action Buttons */}
      <div className="flex flex-wrap gap-2 mb-6">
        <button
          onClick={handleSync}
          disabled={syncing}
          className="px-3 py-1.5 text-sm bg-white border border-gray-300 rounded-md hover:bg-gray-50 disabled:opacity-50"
        >
          {syncing ? "Syncing…" : "Sync"}
        </button>

        <button
          onClick={handleRun}
          disabled={running}
          className="px-3 py-1.5 text-sm bg-indigo-600 text-white rounded-md hover:bg-indigo-700 disabled:opacity-50"
          data-testid="run-button"
        >
          {running ? "Running…" : "Run"}
        </button>

        {feature.status === "InProgress" && (
          <button
            onClick={handleReview}
            disabled={actioning}
            className="px-3 py-1.5 text-sm bg-blue-600 text-white rounded-md hover:bg-blue-700 disabled:opacity-50"
          >
            {actioning ? "…" : "Mark Ready for Review"}
          </button>
        )}

        {feature.status === "ReadyForReview" && (
          <button
            onClick={handleApprove}
            disabled={actioning}
            className="px-3 py-1.5 text-sm bg-green-600 text-white rounded-md hover:bg-green-700 disabled:opacity-50"
            data-testid="hand-off-to-qa-button"
          >
            {actioning ? "…" : "Hand off to QA"}
          </button>
        )}

        {feature.status !== "Closed" && feature.status !== "Merged" && (
          <button
            onClick={handleClose}
            disabled={closing}
            className="px-3 py-1.5 text-sm bg-white border border-red-300 text-red-600 rounded-md hover:bg-red-50 disabled:opacity-50"
          >
            {closing ? "Closing…" : "Close"}
          </button>
        )}
      </div>

      {/* Feedback banners */}
      {runResult && (
        <div className="mb-4 px-4 py-2 bg-green-50 border border-green-200 rounded-md text-sm text-green-800" data-testid="run-result">
          Dispatched {runResult.dispatched} agent{runResult.dispatched !== 1 ? "s" : ""}
          {runResult.failed > 0 && `, ${runResult.failed} failed`}.
        </div>
      )}

      {actionError && (
        <div className="mb-4 px-4 py-2 bg-red-50 border border-red-200 rounded-md text-sm text-red-800">
          {actionError}
        </div>
      )}

      {/* Ticket Queue */}
      <section>
        <h2 className="text-sm font-semibold text-gray-700 mb-3 uppercase tracking-wide">
          Tickets ({tickets.length})
        </h2>

        {tickets.length === 0 ? (
          <p className="text-sm text-gray-400">No tickets linked to this feature.</p>
        ) : (
          <table className="min-w-full text-sm" data-testid="tickets-table">
            <thead>
              <tr className="border-b border-gray-200 text-left text-xs text-gray-500 uppercase tracking-wide">
                <th className="pb-2 font-medium">#</th>
                <th className="pb-2 font-medium">Title</th>
                <th className="pb-2 font-medium">State</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-gray-100">
              {tickets.map((ticket) => (
                <tr key={ticket.id} className="hover:bg-gray-50">
                  <td className="py-2 pr-4 text-gray-500 text-xs">
                    {ticket.source_id}
                  </td>
                  <td className="py-2 pr-4">
                    {ticket.url ? (
                      <a
                        href={ticket.url}
                        target="_blank"
                        rel="noopener noreferrer"
                        className="text-indigo-600 hover:underline"
                      >
                        {ticket.title}
                      </a>
                    ) : (
                      ticket.title
                    )}
                  </td>
                  <td className="py-2">
                    <span className={`inline-flex items-center px-2 py-0.5 rounded text-xs font-medium ${
                      ticket.state === "open"
                        ? "bg-green-100 text-green-700"
                        : "bg-gray-100 text-gray-500"
                    }`}>
                      {ticket.state}
                    </span>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </section>
    </div>
  );
}
